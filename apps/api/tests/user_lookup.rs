//! P1-109: the user lookup helper — live, through the real
//! HTTP layer.
//!
//! - `user_lookup_resolves_by_email_and_audits` — the AC
//!   (email leg): an admin looks a user up by email and gets
//!   the P1-106 view (the user id, the profile email, the
//!   Free plan, no secrets anywhere in the body), and exactly
//!   one `admin.users.lookup` row is written (actor the
//!   admin, target the user, after `{lookup_kind: "email",
//!   found: true}`, no reason, no raw searched value). The
//!   email match is case-insensitive (citext — an uppercased
//!   email resolves to the same user and writes a second
//!   row).
//! - `user_lookup_resolves_by_id_and_job_id_and_audits` —
//!   the id leg (the same view, after `{lookup_kind: "id",
//!   found: true}`) and the job-id leg (a seeded job resolves
//!   through its owner — the response names the USER, not
//!   the job — after `{lookup_kind: "job_id", found:
//!   true}`). A deletion-pending user still resolves: the
//!   view carries `deleted_at` in the profile (the view's
//!   rule — `users::by_id` has no deleted filter).
//! - `user_lookup_miss_is_404_and_still_audits` — the AC
//!   (miss leg): an unknown email / id / job id is 404
//!   `PF-2010` AND still writes one row each (target `null`,
//!   after `{lookup_kind, found: false}`) — a probing
//!   pattern is visible only if misses are logged.
//! - `user_lookup_denies_non_admitted_callers` — anonymous
//!   401, plain user 403, an admin user's own API key 403;
//!   support is admitted (200, the P1-106 guard — the
//!   ticket says "support role"). None of the denials audits.
//! - `user_lookup_rejects_bad_query` — zero keys, two keys,
//!   a malformed email, a malformed UUID, and an unknown
//!   query key are each 422 `PF-2002`; none of them audits
//!   (a 422 means no lookup happened).

mod common;

use axum::http::{Method, Request, StatusCode};
use common::{spawn_app, TestApp};
use detag_core::repos::audit_logs;
use serde_json::json;
use uuid::Uuid;

/// The request id: a valid UUID, echoed by the middleware.
const REQUEST_ID: &str = "123e4567-e89b-42d3-a456-426614174113";

/// The password every test user registers with.
const PASSWORD: &str = "correct horse battery staple";

/// A per-request XFF address: each request gets its own IP
/// bucket, so the tests never trip each other's rate limits
/// (the P1-003 lesson).
fn unique_xff() -> String {
    format!("203.0.113.{}", Uuid::now_v7())
}

fn unique_email(prefix: &str) -> String {
    format!("{prefix}-{}@example.com", Uuid::now_v7())
}

/// Register a user, return its id.
async fn register_user(app: &TestApp, email: &str) -> Uuid {
    let body = json!({
        "email": email,
        "password": PASSWORD,
        "accept_tos_version": "v1",
        "accept_aup_version": "v1",
    });
    let res = app
        .request(
            Request::builder()
                .method(Method::POST)
                .uri("/v1/auth/register")
                .header("x-request-id", REQUEST_ID)
                .header("x-forwarded-for", unique_xff())
                .header("content-type", "application/json")
                .body(axum::body::Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.text());
    res.json()["id"].as_str().unwrap().parse().unwrap()
}

/// Log in, return the access token.
async fn login_token(app: &TestApp, email: &str) -> String {
    let body = json!({
        "email": email,
        "password": PASSWORD,
    });
    let res = app
        .request(
            Request::builder()
                .method(Method::POST)
                .uri("/v1/auth/login")
                .header("x-request-id", REQUEST_ID)
                .header("x-forwarded-for", unique_xff())
                .header("content-type", "application/json")
                .body(axum::body::Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    res.json()["access_token"].as_str().unwrap().to_string()
}

/// Register a user, set its role in the database, log in,
/// return (id, token). The role is set by a direct UPDATE —
/// there is no self-serve way to become an admin or support
/// (the role is set by the owner out of band).
async fn make_role(app: &TestApp, email: &str, role: &str) -> (Uuid, String) {
    let id = register_user(app, email).await;
    sqlx::query("UPDATE users SET role = $1 WHERE id = $2")
        .bind(role)
        .bind(id)
        .execute(&app.pool)
        .await
        .expect("set role");
    let token = login_token(app, email).await;
    (id, token)
}

/// Mint a full-scope key for the credential holder; return
/// the secret.
async fn create_key(app: &TestApp, token: &str) -> String {
    let body =
        json!({ "name": "lookup probe", "scopes": ["jobs:read", "jobs:write", "webhooks:manage"] });
    let res = app
        .request(
            Request::builder()
                .method(Method::POST)
                .uri("/v1/api-keys")
                .header("x-request-id", REQUEST_ID)
                .header("x-forwarded-for", unique_xff())
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(axum::body::Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.text());
    res.json()["secret"].as_str().unwrap().to_string()
}

/// Seed one queued image job for the user; return its id.
/// A job's `upload_id` is nullable, so no upload is needed.
async fn seed_job(app: &TestApp, user_id: Uuid) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO jobs (id, user_id, kind, mode, status, lane, created_at) \
         VALUES ($1, $2, 'image', 'inpaint', 'queued', 'standard', now())",
    )
    .bind(id)
    .bind(user_id)
    .execute(&app.pool)
    .await
    .expect("seed job");
    id
}

/// GET the lookup endpoint with the given query string and
/// an optional credential (`None` = anonymous).
async fn lookup(app: &TestApp, query: &str, credential: Option<&str>) -> common::TestResponse {
    let mut req = Request::builder()
        .method(Method::GET)
        .uri(format!("/v1/admin/users/lookup{query}"))
        .header("x-request-id", REQUEST_ID)
        .header("x-forwarded-for", unique_xff());
    if let Some(credential) = credential {
        req = req.header("authorization", format!("Bearer {credential}"));
    }
    app.request(req.body(axum::body::Body::empty()).unwrap())
        .await
}

/// The `admin.users.lookup` rows for one user target.
async fn lookup_rows(app: &TestApp, user_id: Uuid) -> Vec<audit_logs::AuditLog> {
    audit_logs::list_for_target(&app.pool, "user", &user_id.to_string())
        .await
        .expect("load audit")
        .into_iter()
        .filter(|row| row.action == "admin.users.lookup")
        .collect()
}

/// The `admin.users.lookup` miss rows for one actor (a miss
/// has no target, so it cannot be listed by target — list
/// by action and filter by actor + `found: false`).
async fn miss_rows_for_actor(app: &TestApp, actor: Uuid) -> Vec<audit_logs::AuditLog> {
    audit_logs::list_for_action(&app.pool, "admin.users.lookup")
        .await
        .expect("load audit")
        .into_iter()
        .filter(|row| {
            row.actor_user_id == Some(actor)
                && row.target_id.is_none()
                && row.after.as_ref().and_then(|a| a["found"].as_bool()) == Some(false)
        })
        .collect()
}

fn problem_code(res: &common::TestResponse) -> String {
    res.json()["code"].as_str().unwrap_or("").to_string()
}

#[tokio::test]
async fn user_lookup_resolves_by_email_and_audits() {
    let app = spawn_app().await;
    let (admin_id, admin_token) = make_role(&app, &unique_email("admin"), "admin").await;
    let user = app.seed_user_unique().await;
    let email = user.email.clone();

    let res = lookup(&app, &format!("?email={email}"), Some(&admin_token)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let raw = res.text();
    // No secret field name anywhere in the body — the P1-106
    // shape, proven on the wire, not just in the unit tests.
    for forbidden in ["password_hash", "refresh_hash", "family_id"] {
        assert!(
            !raw.contains(forbidden),
            "view body must not contain {forbidden}: {raw}"
        );
    }
    let body = res.json();
    assert_eq!(body["user_id"], user.id.to_string());
    assert_eq!(body["profile"]["email"], email);
    assert_eq!(body["profile"]["role"], "user");
    assert_eq!(body["plan"]["code"], "free");

    // Exactly one audit row: actor the admin, target the
    // user, after the kind + found — and no raw searched
    // value anywhere in the row.
    let rows = lookup_rows(&app, user.id).await;
    assert_eq!(rows.len(), 1, "one audit row for the lookup");
    let row = &rows[0];
    assert_eq!(row.actor_user_id, Some(admin_id), "actor is the admin");
    assert_eq!(row.target_type, "user");
    assert_eq!(
        row.target_id.as_deref(),
        Some(user.id.to_string()).as_deref()
    );
    assert!(row.before.is_none(), "a lookup has no before state");
    assert!(row.reason.is_none(), "a lookup carries no reason");
    assert_eq!(row.after.as_ref().unwrap()["lookup_kind"], "email");
    assert_eq!(row.after.as_ref().unwrap()["found"], true);
    assert!(
        !serde_json::to_string(row.after.as_ref().unwrap())
            .unwrap()
            .contains(&email),
        "the audit row must not carry the searched email"
    );

    // Case-insensitive (citext): the uppercased email
    // resolves to the same user and writes a second row.
    let res = lookup(
        &app,
        &format!("?email={}", email.to_uppercase()),
        Some(&admin_token),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(res.json()["user_id"], user.id.to_string());
    let rows = lookup_rows(&app, user.id).await;
    assert_eq!(rows.len(), 2, "the second lookup writes a second row");
    assert_eq!(rows[1].after.as_ref().unwrap()["lookup_kind"], "email");
}

#[tokio::test]
async fn user_lookup_resolves_by_id_and_job_id_and_audits() {
    let app = spawn_app().await;
    let (admin_id, admin_token) = make_role(&app, &unique_email("admin"), "admin").await;
    let user = app.seed_user_unique().await;
    let job_id = seed_job(&app, user.id).await;

    // By id: the same view, after `{lookup_kind: "id",
    // found: true}`.
    let res = lookup(&app, &format!("?id={}", user.id), Some(&admin_token)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(res.json()["user_id"], user.id.to_string());
    let rows = lookup_rows(&app, user.id).await;
    assert_eq!(rows.len(), 1, "one audit row for the id lookup");
    assert_eq!(rows[0].actor_user_id, Some(admin_id));
    assert_eq!(rows[0].after.as_ref().unwrap()["lookup_kind"], "id");
    assert_eq!(rows[0].after.as_ref().unwrap()["found"], true);

    // By job id: the response names the USER, not the job —
    // the job id is only the key that found them.
    let res = lookup(&app, &format!("?job_id={job_id}"), Some(&admin_token)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let body = res.json();
    assert_eq!(body["user_id"], user.id.to_string());
    assert_eq!(body["profile"]["email"], user.email);
    assert_eq!(body["recent_jobs"][0]["id"], job_id.to_string());
    let rows = lookup_rows(&app, user.id).await;
    assert_eq!(rows.len(), 2, "the job lookup writes a second row");
    assert_eq!(rows[1].after.as_ref().unwrap()["lookup_kind"], "job_id");
    assert_eq!(rows[1].after.as_ref().unwrap()["found"], true);
    assert!(
        !serde_json::to_string(rows[1].after.as_ref().unwrap())
            .unwrap()
            .contains(&job_id.to_string()),
        "the audit row must not carry the searched job id"
    );

    // A deletion-pending user still resolves: the view
    // follows `get_admin_user_view`'s rule — `users::by_id`
    // has no deleted filter, so the user is returned with
    // `deleted_at` in the profile, not 404.
    sqlx::query("UPDATE users SET status = 'deletion_pending', deleted_at = now() WHERE id = $1")
        .bind(user.id)
        .execute(&app.pool)
        .await
        .expect("mark user deleting");
    let res = lookup(&app, &format!("?id={}", user.id), Some(&admin_token)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(res.json()["user_id"], user.id.to_string());
    assert!(
        res.json()["profile"]["deleted_at"].is_string(),
        "the view carries the deletion time: {}",
        res.text()
    );
    assert_eq!(lookup_rows(&app, user.id).await.len(), 3);
}

#[tokio::test]
async fn user_lookup_miss_is_404_and_still_audits() {
    let app = spawn_app().await;
    let (admin_id, admin_token) = make_role(&app, &unique_email("admin"), "admin").await;
    let before = miss_rows_for_actor(&app, admin_id).await.len();

    // Unknown email: 404 PF-2010, one miss row.
    let res = lookup(
        &app,
        &format!("?email=nobody-{}@example.com", Uuid::now_v7()),
        Some(&admin_token),
    )
    .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND, "{}", res.text());
    assert_eq!(problem_code(&res), "PF-2010");
    // Unknown id: 404, one miss row.
    let res = lookup(&app, &format!("?id={}", Uuid::now_v7()), Some(&admin_token)).await;
    assert_eq!(res.status, StatusCode::NOT_FOUND, "{}", res.text());
    assert_eq!(problem_code(&res), "PF-2010");
    // Unknown job id: 404, one miss row.
    let res = lookup(
        &app,
        &format!("?job_id={}", Uuid::now_v7()),
        Some(&admin_token),
    )
    .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND, "{}", res.text());
    assert_eq!(problem_code(&res), "PF-2010");

    let rows = miss_rows_for_actor(&app, admin_id).await;
    assert_eq!(rows.len(), before + 3, "every miss writes one row");
    let kinds: Vec<&str> = rows
        .iter()
        .skip(before)
        .map(|r| r.after.as_ref().unwrap()["lookup_kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, vec!["email", "id", "job_id"]);
    for row in rows.iter().skip(before) {
        assert_eq!(row.target_id, None, "a miss has no target");
        assert_eq!(row.actor_user_id, Some(admin_id));
        assert_eq!(row.target_type, "user");
        assert_eq!(row.after.as_ref().unwrap()["found"], false);
    }
}

#[tokio::test]
async fn user_lookup_denies_non_admitted_callers() {
    let app = spawn_app().await;
    let (admin_id, admin_token) = make_role(&app, &unique_email("admin"), "admin").await;
    let (_support_id, support_token) = make_role(&app, &unique_email("support"), "support").await;
    let (_user_id, user_token) = make_role(&app, &unique_email("user"), "user").await;
    // P1-055: key creation is plan-gated — the admin user
    // needs a plan first.
    app.grant_plan(admin_id, "pro").await;
    let admin_key = create_key(&app, &admin_token).await;
    let user = app.seed_user_unique().await;
    let query = format!("?id={}", user.id);
    let before = lookup_rows(&app, user.id).await.len();

    // Anonymous: 401 PF-1001.
    let res = lookup(&app, &query, None).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    assert_eq!(problem_code(&res), "PF-1001");
    // Plain user: 403 PF-1002.
    let res = lookup(&app, &query, Some(&user_token)).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.text());
    assert_eq!(problem_code(&res), "PF-1002");
    // An admin user's own API key: 403 — no admin ever.
    let res = lookup(&app, &query, Some(&admin_key)).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.text());
    assert_eq!(problem_code(&res), "PF-1002");
    // Support is admitted (the P1-106 guard — the ticket
    // says "support role"): 200, and the lookup audits with
    // support as the actor.
    let res = lookup(&app, &query, Some(&support_token)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(res.json()["user_id"], user.id.to_string());

    // The denials audited nothing; only the support hit
    // wrote a row.
    let rows = lookup_rows(&app, user.id).await;
    assert_eq!(rows.len(), before + 1, "only the support hit audits");
    assert_eq!(rows[before].actor_user_id, Some(_support_id));
    assert_eq!(miss_rows_for_actor(&app, admin_id).await.len(), 0);
}

#[tokio::test]
async fn user_lookup_rejects_bad_query() {
    let app = spawn_app().await;
    let (admin_id, admin_token) = make_role(&app, &unique_email("admin"), "admin").await;
    let user = app.seed_user_unique().await;
    let before = lookup_rows(&app, user.id).await.len();
    let misses_before = miss_rows_for_actor(&app, admin_id).await.len();

    // No key at all.
    let res = lookup(&app, "", Some(&admin_token)).await;
    assert_eq!(
        res.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        res.text()
    );
    assert_eq!(problem_code(&res), "PF-2002");
    // Two keys.
    let res = lookup(
        &app,
        &format!("?email={}&id={}", user.email, user.id),
        Some(&admin_token),
    )
    .await;
    assert_eq!(
        res.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        res.text()
    );
    assert_eq!(problem_code(&res), "PF-2002");
    // A malformed email.
    let res = lookup(&app, "?email=not-an-email", Some(&admin_token)).await;
    assert_eq!(
        res.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        res.text()
    );
    assert_eq!(problem_code(&res), "PF-2002");
    // A malformed id.
    let res = lookup(&app, "?id=not-a-uuid", Some(&admin_token)).await;
    assert_eq!(
        res.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        res.text()
    );
    assert_eq!(problem_code(&res), "PF-2002");
    // A malformed job id.
    let res = lookup(&app, "?job_id=not-a-uuid", Some(&admin_token)).await;
    assert_eq!(
        res.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        res.text()
    );
    assert_eq!(problem_code(&res), "PF-2002");
    // An unknown query key: a typo'd key must not silently
    // resolve nothing.
    let res = lookup(&app, &format!("?emai={}", user.email), Some(&admin_token)).await;
    assert_eq!(
        res.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        res.text()
    );
    assert_eq!(problem_code(&res), "PF-2002");

    // None of the rejections audited (a 422 means no lookup
    // happened).
    assert_eq!(
        lookup_rows(&app, user.id).await.len(),
        before,
        "validation failures audit nothing"
    );
    assert_eq!(
        miss_rows_for_actor(&app, admin_id).await.len(),
        misses_before,
        "validation failures write no miss row either"
    );
}
