//! Integration harness (P0-071, E0.1): the axum test client and the
//! fixture builder the endpoint tests share.
//!
//! Two pieces:
//!
//! - [`TestApp`] — an in-process app (oneshot client, the axum test
//!   client pattern) with `get` / `post_json` / `request` helpers and
//!   a [`TestResponse`] whose `json()` parses the body. `spawn_app()`
//!   builds one from the environment fixtures; `spawn_with` takes
//!   explicit settings (e.g. prod, for the docs test).
//! - [`TestFixtures`] — where the services come from. `from_env`
//!   uses the native services on this VM (or compose in CI) and
//!   creates + migrates a scratch database, `detag_api_harness`
//!   (never the dev `pf` database — same rule as the core repo live
//!   tests). `containers` starts postgres, redis and MinIO in
//!   testcontainers instead — docker only, so callers env-gate it
//!   (`PF_TEST_CONTAINERS=1`); it skips on this VM (DOCKER-001).
//!
//! `seed_user` inserts a row via the users repo. The password hash is
//! an explicit argument: there is no argon2 hasher in the workspace
//! yet, so tests that only need the row pass [`TEST_PASSWORD_HASH`]
//!   — a placeholder, NOT a valid argon2 hash. A login test must pass
//! a real hash (the hasher lands with P1-001/P1-003).
//!
//! Not yet: `issue_token`. Minting a token needs the JWT signer and
//! the claims shape, both Phase 1 decisions (P1-003, login). Until
//! then, tests that need a token call the login endpoint once it
//! exists — no fake token is minted here, because a token the
//! verifier rejects is worse than no helper at all.

// A shared helper module: every test binary compiles this file, and
// not every binary uses every item yet (post_json / seed_user are
// for the Phase 1 endpoint tests). Unused items are intentional.
#![allow(dead_code)]

use axum::{
    body::Body,
    http::{HeaderMap, Method, Request, StatusCode},
    Router,
};
use detag_api::{build_router, AppState};
use detag_core::{
    config::Settings,
    db::{connect_lazy, DbPool, PoolConfig},
    metrics::Metrics,
    migrate::migrate_up,
    repos::users::{self, User},
};
use std::path::PathBuf;
use testcontainers::{runners::AsyncRunner, ContainerAsync, GenericImage, ImageExt};
use tower::ServiceExt;
use uuid::Uuid;

/// Scratch database the harness creates and migrates. Never the dev
/// `pf` database — test rows stay out of the developer's data.
pub const SCRATCH_DB: &str = "detag_api_harness";

/// Placeholder password hash for [`TestApp::seed_user_unique`]. NOT a
/// valid argon2 hash — a login attempt against it fails by design.
/// Pass a real argon2id hash to `seed_user` when the test logs in.
pub const TEST_PASSWORD_HASH: &str = "argon2id-test-placeholder";

/// Where the services come from. Owns any testcontainers it started
/// (dropping the struct stops them), so keep it alive as long as the
/// app under test runs — [`TestApp`] does this for you.
pub struct TestFixtures {
    /// Postgres URL of the migrated scratch database.
    pub database_url: String,
    /// Redis URL.
    pub redis_url: String,
    /// S3 endpoint URL.
    pub s3_endpoint: String,
    /// S3 bucket the app is configured with.
    pub s3_bucket: String,
    /// S3 access key.
    pub s3_access_key: String,
    /// S3 secret key.
    pub s3_secret_key: String,
    containers: Option<TestContainers>,
}

struct TestContainers {
    pg: ContainerAsync<GenericImage>,
    redis: ContainerAsync<GenericImage>,
    minio: ContainerAsync<GenericImage>,
}

fn env_or(name: &str, fallback: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| fallback.to_string())
}

fn migrations_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("migrations")
}

/// Postgres `42P04` — another test binary won the CREATE DATABASE
/// race; the database exists now, which is all we needed. The race
/// can also surface as `23505` on `pg_database_datname_index` (the
/// winner's catalog row lands first) — same outcome.
fn is_duplicate_database(err: &sqlx::Error) -> bool {
    let Some(db_err) = err.as_database_error() else {
        return false;
    };
    match db_err.code().as_deref() {
        Some("42P04") => true,
        Some("23505") => db_err.constraint() == Some("pg_database_datname_index"),
        _ => false,
    }
}

/// Create the scratch database on the server the base URL points at
/// (if it is not there yet) and return its URL. The base URL keeps
/// its user, host, port and query string; only the database name
/// changes.
async fn ensure_scratch_db(base_url: &str) -> String {
    use sqlx::Connection as _;

    let mut admin = sqlx::PgConnection::connect(base_url)
        .await
        .expect("harness: Postgres must be up (native `just dev` or compose) — see DOCKER-001");
    let exists: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_database WHERE datname = $1)")
            .bind(SCRATCH_DB)
            .fetch_one(&mut admin)
            .await
            .expect("harness: pg_database lookup");
    if !exists {
        // Literal: the name is a const, never user input (same rule
        // as the core repo live tests' drop_db_sql).
        match sqlx::query("CREATE DATABASE detag_api_harness")
            .execute(&mut admin)
            .await
        {
            Ok(_) => {}
            Err(err) if is_duplicate_database(&err) => {}
            Err(err) => panic!("harness: CREATE DATABASE {SCRATCH_DB}: {err}"),
        }
    }
    let (head, query) = match base_url.split_once('?') {
        Some((head, query)) => (head, Some(query)),
        None => (base_url, None),
    };
    let base = match head.rfind('/') {
        Some(idx) => &head[..idx],
        None => head,
    };
    match query {
        Some(query) => format!("{base}/{SCRATCH_DB}?{query}"),
        None => format!("{base}/{SCRATCH_DB}"),
    }
}

impl TestFixtures {
    /// Fixtures from the environment: native services on this VM,
    /// compose in CI. Creates + migrates the scratch database
    /// (idempotent — concurrent test binaries serialize on the
    /// CREATE race and on sqlx's migration advisory lock).
    pub async fn from_env() -> Self {
        let defaults = Settings::default();
        let base_url = env_or("PF_DATABASE_URL", &defaults.database_url);
        let database_url = ensure_scratch_db(&base_url).await;
        let mut fixtures = Self {
            database_url,
            redis_url: env_or("PF_REDIS_URL", &defaults.redis_url),
            s3_endpoint: env_or("PF_S3_ENDPOINT", &defaults.s3_endpoint),
            s3_bucket: env_or("PF_S3_BUCKET", &defaults.s3_bucket),
            s3_access_key: env_or("PF_S3_ACCESS_KEY", &defaults.s3_access_key),
            s3_secret_key: env_or("PF_S3_SECRET_KEY", &defaults.s3_secret_key),
            containers: None,
        };
        fixtures.migrate().await;
        fixtures
    }

    /// Fixtures from testcontainers: postgres:16, redis:7 and
    /// minio/minio, each on a random host port. Docker only — call
    /// it only when `PF_TEST_CONTAINERS=1` (CI does); on this VM it
    /// fails to start containers (DOCKER-001).
    ///
    /// The MinIO bucket is NOT created: bucket creation is an ops
    /// concern (same note as the storage live test), so the storage
    /// health probe reports down until the test creates its bucket.
    pub async fn containers() -> Result<Self, testcontainers::TestcontainersError> {
        use testcontainers::core::{IntoContainerPort, WaitFor};

        let pg = GenericImage::new("postgres", "16")
            .with_exposed_port(5432.tcp())
            .with_wait_for(WaitFor::message_on_stdout(
                "database system is ready to accept connections",
            ))
            .with_env_var("POSTGRES_USER", "pf")
            .with_env_var("POSTGRES_PASSWORD", "pf")
            .with_env_var("POSTGRES_DB", "pf")
            .start()
            .await?;
        let pg_port = pg.get_host_port_ipv4(5432).await?;
        let pg_base = format!("postgres://pf:pf@127.0.0.1:{pg_port}/pf");
        let database_url = ensure_scratch_db(&pg_base).await;

        let redis = GenericImage::new("redis", "7")
            .with_exposed_port(6379.tcp())
            .with_wait_for(WaitFor::message_on_stdout("Ready to accept connections"))
            .start()
            .await?;
        let redis_port = redis.get_host_port_ipv4(6379).await?;

        let minio = GenericImage::new("minio/minio", "latest")
            .with_exposed_port(9000.tcp())
            .with_wait_for(WaitFor::message_on_stdout("API: http://"))
            .with_env_var("MINIO_ROOT_USER", "minioadmin")
            .with_env_var("MINIO_ROOT_PASSWORD", "minioadmin")
            .with_cmd(["server", "/data"])
            .start()
            .await?;
        let minio_port = minio.get_host_port_ipv4(9000).await?;

        let mut fixtures = Self {
            database_url,
            redis_url: format!("redis://127.0.0.1:{redis_port}"),
            s3_endpoint: format!("http://127.0.0.1:{minio_port}"),
            s3_bucket: "detag-test".to_string(),
            s3_access_key: "minioadmin".to_string(),
            s3_secret_key: "minioadmin".to_string(),
            containers: Some(TestContainers { pg, redis, minio }),
        };
        fixtures.migrate().await;
        Ok(fixtures)
    }

    /// Apply all pending migrations to the scratch database.
    /// Idempotent; safe to call from parallel tests (sqlx takes the
    /// migration advisory lock).
    async fn migrate(&mut self) {
        let pool = connect_lazy(&self.database_url, &PoolConfig::default())
            .expect("harness: scratch pool builds");
        migrate_up(&pool, &migrations_dir())
            .await
            .expect("harness: migrations apply to the scratch DB");
    }

    /// Settings pointing at these fixtures (everything else is the
    /// local-dev default).
    pub fn settings(&self) -> Settings {
        let mut settings = Settings::default();
        settings.database_url.clone_from(&self.database_url);
        settings.redis_url.clone_from(&self.redis_url);
        settings.s3_endpoint.clone_from(&self.s3_endpoint);
        settings.s3_bucket.clone_from(&self.s3_bucket);
        settings.s3_access_key.clone_from(&self.s3_access_key);
        settings.s3_secret_key.clone_from(&self.s3_secret_key);
        settings
    }
}

/// An in-process app under test: the router served through tower's
/// oneshot (the axum test client pattern — no sockets, no spawned
/// binary), plus a pool on the fixture database for seeding.
pub struct TestApp {
    /// The settings the app was built from.
    pub settings: Settings,
    /// The app's state (its pool is the one the app itself uses).
    pub state: AppState,
    /// The router (state + middleware), for oneshot requests.
    pub router: Router,
    /// The harness's own pool on the fixture database, for seeding.
    pub pool: DbPool,
    _fixtures: Option<TestFixtures>,
}

impl TestApp {
    /// App from the environment fixtures (native services here,
    /// compose in CI). The scratch database is created + migrated.
    pub async fn spawn() -> Self {
        Self::spawn_with_fixtures(TestFixtures::from_env().await).await
    }

    /// App from explicit fixtures (e.g. testcontainers).
    pub async fn spawn_with_fixtures(fixtures: TestFixtures) -> Self {
        let settings = fixtures.settings();
        Self::spawn_with_settings(settings, Some(fixtures)).await
    }

    /// App from explicit settings. The caller owns the database the
    /// settings point at — use it when the test only needs the
    /// router (e.g. the prod docs test), not a migrated scratch DB.
    pub async fn spawn_with(settings: Settings) -> Self {
        Self::spawn_with_settings(settings, None).await
    }

    /// App from explicit settings AND explicit fixtures (P1-001):
    /// the fixtures' migrated scratch database with the caller's
    /// settings (e.g. the register test's disposable-email
    /// denylist). Use it when the test needs both a real database
    /// and non-default settings.
    pub async fn spawn_with_fixtures_and_settings(
        fixtures: TestFixtures,
        settings: Settings,
    ) -> Self {
        Self::spawn_with_settings(settings, Some(fixtures)).await
    }

    async fn spawn_with_settings(settings: Settings, fixtures: Option<TestFixtures>) -> Self {
        let pool = connect_lazy(&settings.database_url, &PoolConfig::default())
            .expect("harness: seed pool builds");
        let state = AppState::from_settings(&settings).expect("harness: AppState builds");
        let router = build_router(state.clone(), Metrics::new());
        Self {
            settings,
            state,
            router,
            pool,
            _fixtures: fixtures,
        }
    }

    /// Send a raw request through the router (oneshot).
    pub async fn request(&self, req: Request<Body>) -> TestResponse {
        let res = self
            .router
            .clone()
            .oneshot(req)
            .await
            .unwrap_or_else(|err| panic!("harness: oneshot request failed: {err:?}"));
        let status = res.status();
        let headers = res.headers().clone();
        let body = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .expect("harness: read response body");
        TestResponse {
            status,
            headers,
            body: body.to_vec(),
        }
    }

    /// GET `path` (no body, no auth headers).
    pub async fn get(&self, path: &str) -> TestResponse {
        let req = Request::builder()
            .method(Method::GET)
            .uri(path)
            .body(Body::empty())
            .expect("harness: GET request builds");
        self.request(req).await
    }

    /// POST `path` with a JSON body.
    pub async fn post_json(&self, path: &str, body: serde_json::Value) -> TestResponse {
        let req = Request::builder()
            .method(Method::POST)
            .uri(path)
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&body).expect("harness: JSON body serializes"),
            ))
            .expect("harness: POST request builds");
        self.request(req).await
    }

    /// Insert a user row via the users repo. The hash is explicit —
    /// see [`TEST_PASSWORD_HASH`].
    pub async fn seed_user(&self, email: &str, password_hash: &str) -> User {
        users::create(&self.pool, email, password_hash)
            .await
            .expect("harness: seed_user inserts")
    }

    /// Insert a user with a generated unique email and the
    /// placeholder hash. Use it when the test only needs the row to
    /// exist (a login test must pass a real hash — see the module
    /// docs).
    pub async fn seed_user_unique(&self) -> User {
        self.seed_user(&unique_email(), TEST_PASSWORD_HASH).await
    }
}

/// `spawn_app()` — the spec's name for [`TestApp::spawn`].
pub async fn spawn_app() -> TestApp {
    TestApp::spawn().await
}

/// A fresh email address, unique per call (the column is unique, and
/// tests run in parallel against one scratch database).
pub fn unique_email() -> String {
    format!("harness-{}@example.com", Uuid::now_v7())
}

/// A response from the test client: status, headers, raw body.
pub struct TestResponse {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
}

impl TestResponse {
    /// Parse the body as JSON. Panics (with the body in the
    /// message) when it is not JSON — a test helper, not library
    /// code.
    pub fn json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.body).unwrap_or_else(|err| {
            panic!(
                "response body is not JSON ({err}): {}",
                String::from_utf8_lossy(&self.body)
            )
        })
    }

    /// The body as text.
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}
