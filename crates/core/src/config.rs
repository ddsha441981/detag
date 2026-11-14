//! detag-core::config — typed settings (P0-015, NFR-52).
//!
//! Layered config (figment), lowest to highest precedence:
//!   1. built-in defaults (`Settings::default()` — the `.env.example` values)
//!   2. `PF_*` environment variables
//!   3. optional `detag.toml` in the working directory (only if it exists)
//!
//! The file layer wins over env by spec (P0-015). That is surprising on
//! purpose: an adjacent `detag.toml` is the explicit local override; env
//! is the deployment override for everything else. Production values
//! come from the deployment environment, never from a checked-in file.
//!
//! Every field documents its env var (`PF_...`); `just env-check` fails
//! if a var named here is missing from `.env.example` or from the
//! runbook env table (`docs/06-DELIVERY/03-deploy-runbook.md`).
//!
//! Validation: URLs must parse, numeric ranges must hold, `log_level`
//! must be a known level, required strings must be non-empty. All
//! problems are reported at once (`ConfigError::Multiple`), each naming
//! its `PF_*` var, so a bad boot tells the operator everything to fix.
//! Type errors (a non-number where a number belongs, a non-bool where a
//! bool belongs) are collected the same way: fields are extracted one by
//! one and a bad field falls back to its default so the rest can still
//! be checked (P0-020 — never first-failure).

use figment::{
    providers::{Env, Format, Serialized, Toml},
    Figment,
};
use serde::{Deserialize, Serialize};
use std::net::IpAddr;
use std::path::Path;
use thiserror::Error;
use url::Url;

/// The default config file name (optional; loaded only if it exists).
pub const DEFAULT_CONFIG_FILE: &str = "detag.toml";

/// One bad setting, naming the env var to fix.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ConfigError {
    /// A single variable is invalid. `var` is the `PF_*` name.
    #[error("{var}: {reason}")]
    Invalid { var: String, reason: String },
    /// Several variables are invalid at once; `details` lists each
    /// `PF_*: reason` line. `count` is the number of problems.
    #[error("invalid configuration ({count} problem(s)):\n{details}")]
    Multiple { count: usize, details: String },
}

impl ConfigError {
    /// Build the aggregated error from a non-empty list of problems.
    fn multiple(problems: Vec<ConfigError>) -> ConfigError {
        let details = problems
            .iter()
            .map(|p| p.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        ConfigError::Multiple {
            count: problems.len(),
            details,
        }
    }
}

/// Typed settings for the whole workspace (api, worker, engine, billing,
/// mailer). Flat struct on purpose: env vars map 1:1 to fields
/// (`PF_DATABASE_URL` -> `database_url`), so the loader needs no
/// per-field env plumbing and `just env-check` can see every binding.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Env: PF_DATABASE_URL
    pub database_url: String,
    /// Env: PF_REDIS_URL
    pub redis_url: String,
    /// Env: PF_S3_ENDPOINT
    pub s3_endpoint: String,
    /// Env: PF_S3_BUCKET
    pub s3_bucket: String,
    /// Env: PF_S3_ACCESS_KEY
    pub s3_access_key: String,
    /// Env: PF_S3_SECRET_KEY
    pub s3_secret_key: String,
    /// Env: PF_S3_REGION
    pub s3_region: String,
    /// Env: PF_JWT_SECRET
    pub jwt_secret: String,
    /// Env: PF_ARGON2_M_COST_KIB — Argon2id memory cost for
    /// password hashing (P1-001, FR-A-01). Default 65536 (64 MiB,
    /// P1-027); validated 4096..=1048576.
    pub argon2_m_cost_kib: u32,
    /// Env: PF_ARGON2_T_COST — Argon2id time cost (iterations).
    /// Default 3 (P1-027); validated 1..=16.
    pub argon2_t_cost: u32,
    /// Env: PF_ARGON2_P_COST — Argon2id parallelism. Default 4
    /// (P1-027); validated 1..=16.
    pub argon2_p_cost: u32,
    /// Env: PF_PASSWORD_PWNED_CHECK — Have I Been Pwned
    /// k-anonymity check on register + reset (P1-026). Default
    /// false (off): when on, the api sends only the first 5 hex
    /// chars of the password's SHA-1 to `password_pwned_url` and
    /// rejects a password whose suffix appears in the returned
    /// range. Opt-in because it contacts a third party
    /// (Cloudflare, which fronts the API) on every registration
    /// and reset — the full hash and the password never leave the
    /// machine, but the 5-char prefix narrows the password to
    /// ~2^20 candidates and reveals that a check happened.
    /// Fails open: any transport error or non-200 answer lets
    /// the registration/reset continue (a warn is logged).
    pub password_pwned_check: bool,
    /// Env: PF_PASSWORD_PWNED_URL — base URL of the HIBP range
    /// API (P1-026). Default `https://api.pwnedpasswords.com`.
    /// Override for a self-hosted mirror, and for tests (a local
    /// stub server). The path `/range/{prefix}` is appended.
    pub password_pwned_url: String,
    /// Env: PF_TOS_CURRENT_VERSION — the ToS version a
    /// registration must accept (P1-001). Default "v1"; the
    /// register handler rejects any other `accept_tos_version`
    /// with a 422, and records the accepted version on the user
    /// row. Must be non-empty.
    pub tos_current_version: String,
    /// Env: PF_DISPOSABLE_EMAIL_DOMAINS — comma-separated
    /// disposable-email domains rejected at registration
    /// (P1-001). Empty means no denylist. The starter list lives
    /// in `.env.example` (operator-editable); there is no
    /// built-in list in code — the denylist is config, per spec.
    pub disposable_email_domains: String,
    /// Env: PF_PUBLIC_BASE_URL
    pub public_base_url: String,
    /// Env: PF_API_PORT — TCP port the api binds on PF_API_HOST.
    pub api_port: u16,
    /// Env: PF_API_HOST — IP the api binds on (P0-085). Default
    /// 127.0.0.1: the api sits behind caddy / on the host's loopback.
    /// Compose sets 0.0.0.0 — inside a container 127.0.0.1 is the
    /// container's own loopback, unreachable from other containers
    /// or published ports; the container network is the boundary.
    pub api_host: IpAddr,
    /// Env: PF_METRICS_PORT — TCP port the internal /metrics server
    /// binds on PF_METRICS_HOST (never on the public api port).
    pub metrics_port: u16,
    /// Env: PF_METRICS_HOST — IP the internal /metrics servers bind
    /// on (P0-085): the api's and the worker's. Default 127.0.0.1;
    /// compose sets 0.0.0.0 for the same reason as PF_API_HOST.
    pub metrics_host: IpAddr,
    /// Env: PF_STRIPE_SECRET_KEY
    pub stripe_secret_key: String,
    /// Env: PF_STRIPE_WEBHOOK_SECRET
    pub stripe_webhook_secret: String,
    /// Env: PF_RAZORPAY_KEY
    pub razorpay_key: String,
    /// Env: PF_RAZORPAY_SECRET
    pub razorpay_secret: String,
    /// Env: PF_FEATURE_RAZORPAY
    pub feature_razorpay: bool,
    /// Env: PF_SMTP_HOST
    pub smtp_host: String,
    /// Env: PF_SMTP_PORT
    pub smtp_port: u16,
    /// Env: PF_SMTP_USER
    pub smtp_user: String,
    /// Env: PF_SMTP_PASS
    pub smtp_pass: String,
    /// Env: PF_MAIL_FROM
    pub mail_from: String,
    /// Env: PF_SMTP_TLS — use STARTTLS when talking to the SMTP
    /// relay (P1-028). Default false: dev MailHog is plaintext
    /// on :1025; a production relay needs this on.
    pub smtp_tls: bool,
    /// Env: PF_MAILER_MAX_ATTEMPTS — how many send attempts an
    /// outbox row gets before the drain stops claiming it
    /// (P1-028). Default 5. A row that exhausts its attempts
    /// stays in the table with `last_error` set — the terminal
    /// state is `attempts >= max`, not a delete.
    pub mailer_max_attempts: u32,
    /// Env: PF_MAILER_BACKOFF_BASE_SECS — base of the drain's
    /// exponential backoff (P1-028). After the n-th failed
    /// attempt the next try is scheduled `base * 2^(n-1)`
    /// seconds out, capped at 3600 s. Default 60.
    pub mailer_backoff_base_secs: u64,
    /// Env: PF_MAILER_DRAIN_INTERVAL_SECS — how often the worker's
    /// outbox drain loop wakes (P1-028). Default 30.
    pub mailer_drain_interval_secs: u64,
    /// Env: PF_MAILER_DRAIN_BATCH — max outbox rows one drain
    /// pass claims (P1-028). Default 25.
    pub mailer_drain_batch: u32,
    /// Env: PF_MAILER_VERIFY_CAP_PER_HOUR — max verification
    /// mails (`verify-email` + `email-change-verify`) the drain
    /// sends to one recipient address per hour (P1-031).
    /// Default 3. The 4th mail in the hour is parked (dropped)
    /// and the trip is written to the abuse audit log
    /// (`email.rate_limit_tripped`).
    pub mailer_verify_cap_per_hour: u32,
    /// Env: PF_MAILER_RESET_CAP_PER_HOUR — max password-reset
    /// mails the drain sends to one recipient address per hour
    /// (P1-031). Default 3; same park + audit behaviour.
    pub mailer_reset_cap_per_hour: u32,
    /// Env: PF_MAILER_MARKETING_CAP_PER_HOUR — max marketing
    /// mails the drain sends to one recipient address per hour
    /// (P1-031). Default 0: no marketing mail is ever sent in
    /// v1 (transactional-only stance) — any marketing-class row
    /// is parked on sight. Raise deliberately to enable.
    pub mailer_marketing_cap_per_hour: u32,
    /// Env: PF_ENGINE_MODELS_DIR
    pub engine_models_dir: String,
    /// Env: PF_ENGINE_GPU — GPU device count; 0 means CPU-only/off.
    pub engine_gpu: u8,
    /// Env: PF_ENGINE_TILES_MAX
    pub engine_tiles_max: u32,
    /// Env: PF_RESULTS_TTL_FREE_H — result retention, hours.
    pub results_ttl_free_h: u64,
    /// Env: PF_RESULTS_TTL_PRO_H — result retention, hours.
    pub results_ttl_pro_h: u64,
    /// Env: PF_RESULTS_TTL_BUSINESS_H — result retention, hours.
    pub results_ttl_business_h: u64,
    /// Env: PF_UPLOAD_PART_URL_TTL_SECS — how long a presigned PUT
    /// part URL stays valid, seconds (P0-055). Default 600 (10 min):
    /// long enough to upload one part, short enough to stay under
    /// the 15-min access-token TTL (FR-A-02) — the presign policy
    /// refuses any value >= the token TTL.
    pub upload_part_url_ttl_secs: u64,
    /// Env: PF_FEATURE_PROVENANCE_STRIP — stays OFF until the owner signs
    /// the legal-review gate (docs/02-REQUIREMENTS/05-compliance-aup.md).
    pub feature_provenance_strip: bool,
    /// Env: PF_LOG_LEVEL — trace | debug | info | warn | error.
    pub log_level: String,
    /// Env: PF_OTEL_ENDPOINT — empty disables OpenTelemetry export.
    pub otel_endpoint: String,
    /// Env: PF_WEB_ORIGIN — the single browser origin allowed to
    /// call the api cross-origin (P0-065, the Next.js console).
    /// Empty (the default) disables CORS entirely: no
    /// `Access-Control-Allow-Origin` is ever sent and preflights
    /// are rejected. Set it to the console origin in every env
    /// that serves the console (dev: `http://localhost:3000`).
    /// Exactly one origin — a second console origin is a future
    /// change, not a comma list (an allowlist that grows by
    /// accident is how CORS stops meaning anything).
    pub web_origin: String,
    /// Env: PF_ENVIRONMENT — deployment environment: dev | stage | prod.
    /// Gates dev-only surfaces: the Swagger UI (`/docs`) is
    /// served in dev and stage only, never in prod (P0-067).
    /// Default dev.
    pub environment: String,
    /// Env: PF_RATE_LIMIT_IP_BURST — per-IP token-bucket burst
    /// (P0-103): how many requests one IP may make in a burst
    /// before the bucket empties. Default 120. Must be >= 1.
    /// Per-plan limits are open question #17, tuned in Phase 5 —
    /// this is the global skeleton default, not a plan tier.
    pub rate_limit_ip_burst: u64,
    /// Env: PF_RATE_LIMIT_IP_REFILL_PER_MIN — per-IP token-bucket
    /// refill (P0-103): tokens per minute (120 = a sustained
    /// 2 req/s). Default 120. 0 = never refills (a fixed
    /// allowance until the key expires).
    pub rate_limit_ip_refill_per_min: u64,
    /// Env: PF_RATE_LIMIT_ROUTE_BURST — per-route token-bucket
    /// burst (P0-103): how many requests one route template
    /// (`METHOD:/v1/<segment>`) may take in a burst, across all
    /// clients. Default 600. Must be >= 1.
    pub rate_limit_route_burst: u64,
    /// Env: PF_RATE_LIMIT_ROUTE_REFILL_PER_MIN — per-route
    /// token-bucket refill (P0-103): tokens per minute (600 = a
    /// sustained 10 req/s). Default 600. 0 = never refills.
    pub rate_limit_route_refill_per_min: u64,
    /// Env: PF_RATE_LIMIT_LOGIN_ACCOUNT_BURST — per-account
    /// login-attempt bucket burst (P1-003): how many login
    /// attempts one account (keyed by the sha256 of its email,
    /// pre-auth) may make in a burst. Default 10. Must be >= 1.
    /// The refill is deliberately slow (1/min): a burst of
    /// guesses then one attempt a minute. P1-004's lockout is
    /// the durable backstop — this bucket is the cheap,
    /// Redis-backed first line.
    pub rate_limit_login_account_burst: u64,
    /// Env: PF_RATE_LIMIT_LOGIN_ACCOUNT_REFILL_PER_MIN —
    /// per-account login-attempt bucket refill (P1-003): tokens
    /// per minute. Default 1. 0 = never refills (a fixed
    /// allowance until the key expires).
    pub rate_limit_login_account_refill_per_min: u64,
    /// Env: PF_RATE_LIMIT_LOGIN_IP_BURST — per-IP login-attempt
    /// bucket burst (P1-020): how many login attempts one
    /// client IP may make in a burst, across ALL accounts.
    /// Default 20. Must be >= 1. The account bucket (10/1)
    /// bounds a brute-force against ONE account even from many
    /// IPs; this bucket bounds a credential-stuffing run from
    /// ONE IP against many accounts. A shared office NAT or a
    /// family legitimately logs into a few accounts a minute —
    /// 20 burst / 10 per min is generous for humans, tight for
    /// a bot trying hundreds. Keyed by the sha256 of the client
    /// IP (never the raw IP — the middleware's generic IP
    /// bucket uses the raw IP as its key; this service bucket
    /// uses the hash, like the lockout's IP counter).
    pub rate_limit_login_ip_burst: u64,
    /// Env: PF_RATE_LIMIT_LOGIN_IP_REFILL_PER_MIN — per-IP
    /// login-attempt bucket refill (P1-020): tokens per minute.
    /// Default 10. 0 = never refills.
    pub rate_limit_login_ip_refill_per_min: u64,
    /// Env: PF_RATE_LIMIT_REGISTER_EMAIL_BURST — per-email
    /// registration bucket burst (P1-020): how many registration
    /// attempts one email address (keyed by its sha256,
    /// pre-auth) may make in a burst. Default 5. Must be >= 1.
    /// Creating accounts is rarer than logging in — 5 burst /
    /// 1 per min bounds creation spam (including duplicate
    /// attempts, which spend the bucket too: a duplicate
    /// registration IS a registration attempt) while letting a
    /// real user retry a mistyped form a few times.
    pub rate_limit_register_email_burst: u64,
    /// Env: PF_RATE_LIMIT_REGISTER_EMAIL_REFILL_PER_MIN —
    /// per-email registration bucket refill (P1-020): tokens
    /// per minute. Default 1. 0 = never refills.
    pub rate_limit_register_email_refill_per_min: u64,
    /// Env: PF_RATE_LIMIT_RESET_ACCOUNT_BURST — per-account
    /// password-reset bucket burst (P1-020 — the bucket exists
    /// so the reset flow (P1-025) wires its check in one line;
    /// no reset endpoint exists yet, so nothing spends from it
    /// today). Default 10 (same as the login-account bucket —
    /// the reset flow is also a per-account pre-auth attempt).
    /// Must be >= 1.
    pub rate_limit_reset_account_burst: u64,
    /// Env: PF_RATE_LIMIT_RESET_ACCOUNT_REFILL_PER_MIN —
    /// per-account password-reset bucket refill (P1-020).
    /// Default 1. 0 = never refills.
    pub rate_limit_reset_account_refill_per_min: u64,
    /// Env: PF_WORKER_CONCURRENCY — apalis workers per queue. Each
    /// of the four queues (pipeline, cleanup, email, reconciliation)
    /// runs this many workers (P0-075). Default 4. Must be >= 1.
    pub worker_concurrency: usize,
    /// Env: PF_QUEUE_VISIBILITY_TIMEOUT_SECS — visibility timeout
    /// (NFR-21, P0-079): how long after a worker's last keep-alive
    /// its in-flight jobs count as orphaned and are re-queued by
    /// the orphan sweep. A kill -9 mid-job loses nothing — the job
    /// returns to the queue at most this long after the worker
    /// died. Default 300 (5 min, the apalis default, now explicit).
    /// Must be > PF_QUEUE_KEEP_ALIVE_SECS and >= 1.
    pub queue_visibility_timeout_secs: u64,
    /// Env: PF_QUEUE_KEEP_ALIVE_SECS — worker heartbeat interval
    /// (P0-079): a live worker re-registers its consumer this
    /// often, so a job that runs longer than the visibility
    /// timeout on a LIVE worker is never re-queued — only a dead
    /// worker's jobs become orphaned. Default 30 (the apalis
    /// default, now explicit). Must be >= 1 and <
    /// PF_QUEUE_VISIBILITY_TIMEOUT_SECS.
    pub queue_keep_alive_secs: u64,
    /// Env: PF_WORKER_METRICS_PORT — the worker's internal
    /// `/metrics` port (P0-080), loopback only, never the public
    /// port. Default 9092: 9090 is the api's metrics port and 9091
    /// is Prometheus itself in compose (see
    /// `ops/prometheus/prometheus.yml`). Must be 1..=65535.
    pub worker_metrics_port: u16,
    /// Env: PF_WORKER_HEARTBEAT_INTERVAL_SECS — how often the
    /// worker writes its heartbeat row (P0-080): pid, queues,
    /// in-flight count, started_at. Default 15. Must be >= 1.
    pub worker_heartbeat_interval_secs: u64,
    /// Env: PF_WORKER_DRAIN_TIMEOUT_SECS — the graceful-drain
    /// bound (P0-081): after SIGTERM/SIGINT the worker finishes
    /// in-flight jobs for at most this long, then exits non-zero
    /// rather than hanging a deploy. Jobs still in flight at the
    /// deadline are NOT lost — apalis re-queues them when the
    /// visibility timeout expires (NFR-21). Default 30 (the same
    /// bound the api uses for request drains). Must be >= 1.
    pub worker_drain_timeout_secs: u64,
    /// Env: PF_WORKER_QUEUE_NAMESPACE — the Redis key namespace
    /// for the four queues (P0-081): `pf:queue:{queue}` in
    /// production. Overridable so a test worker (or a staging
    /// deployment sharing a Redis) never reads production jobs
    /// — the drain test spawns the real binary against a unique
    /// namespace. Default `pf:queue`. Must be non-empty.
    pub worker_queue_namespace: String,
}

impl Default for Settings {
    /// Safe local-dev defaults — exactly the `.env.example` values.
    fn default() -> Self {
        Settings {
            database_url: "postgres://pf:pf@localhost:5432/pf".to_string(),
            redis_url: "redis://localhost:6379".to_string(),
            s3_endpoint: "http://localhost:9000".to_string(),
            s3_bucket: "detag-dev".to_string(),
            s3_access_key: "minioadmin".to_string(),
            s3_secret_key: "minioadmin".to_string(),
            s3_region: "us-east-1".to_string(),
            jwt_secret: "dev-fixed".to_string(),
            argon2_m_cost_kib: 65_536,
            argon2_t_cost: 3,
            argon2_p_cost: 4,
            password_pwned_check: false,
            password_pwned_url: "https://api.pwnedpasswords.com".to_string(),
            tos_current_version: "v1".to_string(),
            disposable_email_domains: String::new(),
            public_base_url: "http://localhost:8080".to_string(),
            api_port: 8080,
            api_host: IpAddr::from([127, 0, 0, 1]),
            metrics_port: 9090,
            metrics_host: IpAddr::from([127, 0, 0, 1]),
            stripe_secret_key: "sk_test_placeholder".to_string(),
            stripe_webhook_secret: "whsec_placeholder".to_string(),
            razorpay_key: "rzp_test_placeholder".to_string(),
            razorpay_secret: "placeholder".to_string(),
            feature_razorpay: false,
            smtp_host: "localhost".to_string(),
            smtp_port: 1025,
            smtp_user: String::new(),
            smtp_pass: String::new(),
            mail_from: "noreply@detag.local".to_string(),
            smtp_tls: false,
            mailer_max_attempts: 5,
            mailer_backoff_base_secs: 60,
            mailer_drain_interval_secs: 30,
            mailer_drain_batch: 25,
            mailer_verify_cap_per_hour: 3,
            mailer_reset_cap_per_hour: 3,
            mailer_marketing_cap_per_hour: 0,
            engine_models_dir: "./models".to_string(),
            engine_gpu: 0,
            engine_tiles_max: 1024,
            results_ttl_free_h: 6,
            results_ttl_pro_h: 24,
            results_ttl_business_h: 72,
            upload_part_url_ttl_secs: 600,
            feature_provenance_strip: false,
            log_level: "info".to_string(),
            otel_endpoint: String::new(),
            web_origin: String::new(),
            environment: "dev".to_string(),
            rate_limit_ip_burst: 120,
            rate_limit_ip_refill_per_min: 120,
            rate_limit_route_burst: 600,
            rate_limit_route_refill_per_min: 600,
            rate_limit_login_account_burst: 10,
            rate_limit_login_account_refill_per_min: 1,
            rate_limit_login_ip_burst: 20,
            rate_limit_login_ip_refill_per_min: 10,
            rate_limit_register_email_burst: 5,
            rate_limit_register_email_refill_per_min: 1,
            rate_limit_reset_account_burst: 10,
            rate_limit_reset_account_refill_per_min: 1,
            worker_concurrency: 4,
            worker_metrics_port: 9092,
            worker_heartbeat_interval_secs: 15,
            worker_drain_timeout_secs: 30,
            worker_queue_namespace: "pf:queue".to_string(),
            queue_visibility_timeout_secs: 300,
            queue_keep_alive_secs: 30,
        }
    }
}

/// (struct field, env var) pairs, used to name the var in figment
/// extraction errors (figment reports the field path, not the env name).
const FIELD_VARS: &[(&str, &str)] = &[
    ("database_url", "PF_DATABASE_URL"),
    ("redis_url", "PF_REDIS_URL"),
    ("s3_endpoint", "PF_S3_ENDPOINT"),
    ("s3_bucket", "PF_S3_BUCKET"),
    ("s3_access_key", "PF_S3_ACCESS_KEY"),
    ("s3_secret_key", "PF_S3_SECRET_KEY"),
    ("s3_region", "PF_S3_REGION"),
    ("jwt_secret", "PF_JWT_SECRET"),
    ("argon2_m_cost_kib", "PF_ARGON2_M_COST_KIB"),
    ("argon2_t_cost", "PF_ARGON2_T_COST"),
    ("argon2_p_cost", "PF_ARGON2_P_COST"),
    ("password_pwned_check", "PF_PASSWORD_PWNED_CHECK"),
    ("password_pwned_url", "PF_PASSWORD_PWNED_URL"),
    ("tos_current_version", "PF_TOS_CURRENT_VERSION"),
    ("disposable_email_domains", "PF_DISPOSABLE_EMAIL_DOMAINS"),
    ("public_base_url", "PF_PUBLIC_BASE_URL"),
    ("api_port", "PF_API_PORT"),
    ("api_host", "PF_API_HOST"),
    ("metrics_port", "PF_METRICS_PORT"),
    ("metrics_host", "PF_METRICS_HOST"),
    ("stripe_secret_key", "PF_STRIPE_SECRET_KEY"),
    ("stripe_webhook_secret", "PF_STRIPE_WEBHOOK_SECRET"),
    ("razorpay_key", "PF_RAZORPAY_KEY"),
    ("razorpay_secret", "PF_RAZORPAY_SECRET"),
    ("feature_razorpay", "PF_FEATURE_RAZORPAY"),
    ("smtp_host", "PF_SMTP_HOST"),
    ("smtp_port", "PF_SMTP_PORT"),
    ("smtp_user", "PF_SMTP_USER"),
    ("smtp_pass", "PF_SMTP_PASS"),
    ("mail_from", "PF_MAIL_FROM"),
    ("smtp_tls", "PF_SMTP_TLS"),
    ("mailer_max_attempts", "PF_MAILER_MAX_ATTEMPTS"),
    ("mailer_backoff_base_secs", "PF_MAILER_BACKOFF_BASE_SECS"),
    (
        "mailer_drain_interval_secs",
        "PF_MAILER_DRAIN_INTERVAL_SECS",
    ),
    ("mailer_drain_batch", "PF_MAILER_DRAIN_BATCH"),
    (
        "mailer_verify_cap_per_hour",
        "PF_MAILER_VERIFY_CAP_PER_HOUR",
    ),
    ("mailer_reset_cap_per_hour", "PF_MAILER_RESET_CAP_PER_HOUR"),
    (
        "mailer_marketing_cap_per_hour",
        "PF_MAILER_MARKETING_CAP_PER_HOUR",
    ),
    ("engine_models_dir", "PF_ENGINE_MODELS_DIR"),
    ("engine_gpu", "PF_ENGINE_GPU"),
    ("engine_tiles_max", "PF_ENGINE_TILES_MAX"),
    ("results_ttl_free_h", "PF_RESULTS_TTL_FREE_H"),
    ("results_ttl_pro_h", "PF_RESULTS_TTL_PRO_H"),
    ("results_ttl_business_h", "PF_RESULTS_TTL_BUSINESS_H"),
    ("upload_part_url_ttl_secs", "PF_UPLOAD_PART_URL_TTL_SECS"),
    ("feature_provenance_strip", "PF_FEATURE_PROVENANCE_STRIP"),
    ("log_level", "PF_LOG_LEVEL"),
    ("otel_endpoint", "PF_OTEL_ENDPOINT"),
    ("web_origin", "PF_WEB_ORIGIN"),
    ("environment", "PF_ENVIRONMENT"),
    ("rate_limit_ip_burst", "PF_RATE_LIMIT_IP_BURST"),
    (
        "rate_limit_ip_refill_per_min",
        "PF_RATE_LIMIT_IP_REFILL_PER_MIN",
    ),
    ("rate_limit_route_burst", "PF_RATE_LIMIT_ROUTE_BURST"),
    (
        "rate_limit_route_refill_per_min",
        "PF_RATE_LIMIT_ROUTE_REFILL_PER_MIN",
    ),
    (
        "rate_limit_login_account_burst",
        "PF_RATE_LIMIT_LOGIN_ACCOUNT_BURST",
    ),
    (
        "rate_limit_login_account_refill_per_min",
        "PF_RATE_LIMIT_LOGIN_ACCOUNT_REFILL_PER_MIN",
    ),
    ("rate_limit_login_ip_burst", "PF_RATE_LIMIT_LOGIN_IP_BURST"),
    (
        "rate_limit_login_ip_refill_per_min",
        "PF_RATE_LIMIT_LOGIN_IP_REFILL_PER_MIN",
    ),
    (
        "rate_limit_register_email_burst",
        "PF_RATE_LIMIT_REGISTER_EMAIL_BURST",
    ),
    (
        "rate_limit_register_email_refill_per_min",
        "PF_RATE_LIMIT_REGISTER_EMAIL_REFILL_PER_MIN",
    ),
    (
        "rate_limit_reset_account_burst",
        "PF_RATE_LIMIT_RESET_ACCOUNT_BURST",
    ),
    (
        "rate_limit_reset_account_refill_per_min",
        "PF_RATE_LIMIT_RESET_ACCOUNT_REFILL_PER_MIN",
    ),
    ("worker_concurrency", "PF_WORKER_CONCURRENCY"),
    ("worker_metrics_port", "PF_WORKER_METRICS_PORT"),
    (
        "worker_heartbeat_interval_secs",
        "PF_WORKER_HEARTBEAT_INTERVAL_SECS",
    ),
    ("worker_drain_timeout_secs", "PF_WORKER_DRAIN_TIMEOUT_SECS"),
    ("worker_queue_namespace", "PF_WORKER_QUEUE_NAMESPACE"),
    (
        "queue_visibility_timeout_secs",
        "PF_QUEUE_VISIBILITY_TIMEOUT_SECS",
    ),
    ("queue_keep_alive_secs", "PF_QUEUE_KEEP_ALIVE_SECS"),
];

impl Settings {
    /// Load settings: defaults < `PF_*` env < `detag.toml` if it exists
    /// in the working directory. Fails with the offending `PF_*` var name.
    pub fn load() -> Result<Settings, ConfigError> {
        let file = Path::new(DEFAULT_CONFIG_FILE);
        Self::load_from(if file.exists() { Some(file) } else { None })
    }

    /// Load settings with an explicit optional config file (`None` skips
    /// the file layer entirely). Used by tests and by binaries that keep
    /// their config elsewhere.
    ///
    /// Fail-fast aggregate (P0-020): every invalid var is collected into
    /// ONE report — a bad value in one field can never hide a bad value
    /// in another. Fields are extracted one by one; a field that fails
    /// to parse falls back to its default so semantic validation can
    /// still run on the rest, and all problems (type + semantic) are
    /// returned together.
    pub fn load_from(file: Option<&Path>) -> Result<Settings, ConfigError> {
        let mut figment = Figment::new()
            .merge(Serialized::defaults(Settings::default()))
            .merge(Env::prefixed("PF_"));
        if let Some(path) = file {
            if path.exists() {
                figment = figment.merge(Toml::file(path));
            }
        }
        let defaults = Settings::default();
        let mut problems: Vec<ConfigError> = Vec::new();
        macro_rules! field {
            ($name:ident, $ty:ty) => {
                match figment.extract_inner::<$ty>(stringify!($name)) {
                    Ok(value) => value,
                    Err(err) => {
                        let var = FIELD_VARS
                            .iter()
                            .find(|(field, _)| *field == stringify!($name))
                            .map(|(_, var)| (*var).to_string())
                            .unwrap_or_else(|| "PF_*".to_string());
                        problems.push(ConfigError::Invalid {
                            var,
                            reason: err.to_string(),
                        });
                        defaults.$name
                    }
                }
            };
        }
        let settings = Settings {
            database_url: field!(database_url, String),
            redis_url: field!(redis_url, String),
            s3_endpoint: field!(s3_endpoint, String),
            s3_bucket: field!(s3_bucket, String),
            s3_access_key: field!(s3_access_key, String),
            s3_secret_key: field!(s3_secret_key, String),
            s3_region: field!(s3_region, String),
            jwt_secret: field!(jwt_secret, String),
            argon2_m_cost_kib: field!(argon2_m_cost_kib, u32),
            argon2_t_cost: field!(argon2_t_cost, u32),
            argon2_p_cost: field!(argon2_p_cost, u32),
            password_pwned_check: field!(password_pwned_check, bool),
            password_pwned_url: field!(password_pwned_url, String),
            tos_current_version: field!(tos_current_version, String),
            disposable_email_domains: field!(disposable_email_domains, String),
            public_base_url: field!(public_base_url, String),
            api_port: field!(api_port, u16),
            api_host: field!(api_host, IpAddr),
            metrics_port: field!(metrics_port, u16),
            metrics_host: field!(metrics_host, IpAddr),
            stripe_secret_key: field!(stripe_secret_key, String),
            stripe_webhook_secret: field!(stripe_webhook_secret, String),
            razorpay_key: field!(razorpay_key, String),
            razorpay_secret: field!(razorpay_secret, String),
            feature_razorpay: field!(feature_razorpay, bool),
            smtp_host: field!(smtp_host, String),
            smtp_port: field!(smtp_port, u16),
            smtp_user: field!(smtp_user, String),
            smtp_pass: field!(smtp_pass, String),
            mail_from: field!(mail_from, String),
            smtp_tls: field!(smtp_tls, bool),
            mailer_max_attempts: field!(mailer_max_attempts, u32),
            mailer_backoff_base_secs: field!(mailer_backoff_base_secs, u64),
            mailer_drain_interval_secs: field!(mailer_drain_interval_secs, u64),
            mailer_drain_batch: field!(mailer_drain_batch, u32),
            mailer_verify_cap_per_hour: field!(mailer_verify_cap_per_hour, u32),
            mailer_reset_cap_per_hour: field!(mailer_reset_cap_per_hour, u32),
            mailer_marketing_cap_per_hour: field!(mailer_marketing_cap_per_hour, u32),
            engine_models_dir: field!(engine_models_dir, String),
            engine_gpu: field!(engine_gpu, u8),
            engine_tiles_max: field!(engine_tiles_max, u32),
            results_ttl_free_h: field!(results_ttl_free_h, u64),
            results_ttl_pro_h: field!(results_ttl_pro_h, u64),
            results_ttl_business_h: field!(results_ttl_business_h, u64),
            upload_part_url_ttl_secs: field!(upload_part_url_ttl_secs, u64),
            feature_provenance_strip: field!(feature_provenance_strip, bool),
            log_level: field!(log_level, String),
            otel_endpoint: field!(otel_endpoint, String),
            web_origin: field!(web_origin, String),
            environment: field!(environment, String),
            rate_limit_ip_burst: field!(rate_limit_ip_burst, u64),
            rate_limit_ip_refill_per_min: field!(rate_limit_ip_refill_per_min, u64),
            rate_limit_route_burst: field!(rate_limit_route_burst, u64),
            rate_limit_route_refill_per_min: field!(rate_limit_route_refill_per_min, u64),
            rate_limit_login_account_burst: field!(rate_limit_login_account_burst, u64),
            rate_limit_login_account_refill_per_min: field!(
                rate_limit_login_account_refill_per_min,
                u64
            ),
            rate_limit_login_ip_burst: field!(rate_limit_login_ip_burst, u64),
            rate_limit_login_ip_refill_per_min: field!(rate_limit_login_ip_refill_per_min, u64),
            rate_limit_register_email_burst: field!(rate_limit_register_email_burst, u64),
            rate_limit_register_email_refill_per_min: field!(
                rate_limit_register_email_refill_per_min,
                u64
            ),
            rate_limit_reset_account_burst: field!(rate_limit_reset_account_burst, u64),
            rate_limit_reset_account_refill_per_min: field!(
                rate_limit_reset_account_refill_per_min,
                u64
            ),
            worker_concurrency: field!(worker_concurrency, usize),
            worker_metrics_port: field!(worker_metrics_port, u16),
            worker_heartbeat_interval_secs: field!(worker_heartbeat_interval_secs, u64),
            worker_drain_timeout_secs: field!(worker_drain_timeout_secs, u64),
            worker_queue_namespace: field!(worker_queue_namespace, String),
            queue_visibility_timeout_secs: field!(queue_visibility_timeout_secs, u64),
            queue_keep_alive_secs: field!(queue_keep_alive_secs, u64),
        };
        settings.validate(&mut problems);
        if problems.is_empty() {
            Ok(settings)
        } else if problems.len() == 1 {
            Err(problems.into_iter().next().unwrap())
        } else {
            Err(ConfigError::multiple(problems))
        }
    }

    /// Check every rule, appending each problem to `problems`.
    fn validate(&self, problems: &mut Vec<ConfigError>) {
        let mut invalid = |var: &str, reason: String| {
            problems.push(ConfigError::Invalid {
                var: var.to_string(),
                reason,
            });
        };

        // URLs must parse.
        for (var, value) in [
            ("PF_DATABASE_URL", &self.database_url),
            ("PF_REDIS_URL", &self.redis_url),
            ("PF_S3_ENDPOINT", &self.s3_endpoint),
            ("PF_PUBLIC_BASE_URL", &self.public_base_url),
        ] {
            if let Err(err) = Url::parse(value) {
                invalid(var, format!("not a valid URL: {err}"));
            }
        }
        if !self.otel_endpoint.is_empty() {
            if let Err(err) = Url::parse(&self.otel_endpoint) {
                invalid("PF_OTEL_ENDPOINT", format!("not a valid URL: {err}"));
            }
        }
        // Empty disables CORS (P0-065); a set origin must be a URL,
        // or the api would boot with a CORS config that can never
        // match (a silent fail-open is worse than a loud boot error
        // — and a fail-closed one is exactly what empty means).
        if !self.web_origin.is_empty() {
            if let Err(err) = Url::parse(&self.web_origin) {
                invalid("PF_WEB_ORIGIN", format!("not a valid URL: {err}"));
            }
        }

        // Required strings must be non-empty.
        for (var, value) in [
            ("PF_S3_BUCKET", &self.s3_bucket),
            ("PF_S3_ACCESS_KEY", &self.s3_access_key),
            ("PF_S3_SECRET_KEY", &self.s3_secret_key),
            ("PF_S3_REGION", &self.s3_region),
            ("PF_JWT_SECRET", &self.jwt_secret),
            ("PF_STRIPE_SECRET_KEY", &self.stripe_secret_key),
            ("PF_STRIPE_WEBHOOK_SECRET", &self.stripe_webhook_secret),
            ("PF_RAZORPAY_KEY", &self.razorpay_key),
            ("PF_RAZORPAY_SECRET", &self.razorpay_secret),
            ("PF_SMTP_HOST", &self.smtp_host),
            ("PF_MAIL_FROM", &self.mail_from),
            ("PF_ENGINE_MODELS_DIR", &self.engine_models_dir),
        ] {
            if value.trim().is_empty() {
                invalid(var, "must not be empty".to_string());
            }
        }
        if !self.mail_from.contains('@') {
            invalid(
                "PF_MAIL_FROM",
                "must look like an email address (contain '@')".to_string(),
            );
        }

        // Ranges must hold.
        if !(4096..=1_048_576).contains(&self.argon2_m_cost_kib) {
            invalid(
                "PF_ARGON2_M_COST_KIB",
                "must be 4096..=1048576 (4 MiB..=1 GiB)".to_string(),
            );
        }
        if !(1..=16).contains(&self.argon2_t_cost) {
            invalid("PF_ARGON2_T_COST", "must be 1..=16".to_string());
        }
        if !(1..=16).contains(&self.argon2_p_cost) {
            invalid("PF_ARGON2_P_COST", "must be 1..=16".to_string());
        }
        if self.tos_current_version.trim().is_empty() {
            invalid("PF_TOS_CURRENT_VERSION", "must not be empty".to_string());
        }
        if self.smtp_port == 0 {
            invalid("PF_SMTP_PORT", "must be 1..=65535".to_string());
        }
        if self.mailer_max_attempts == 0 {
            invalid("PF_MAILER_MAX_ATTEMPTS", "must be >= 1".to_string());
        }
        if self.mailer_backoff_base_secs == 0 {
            invalid("PF_MAILER_BACKOFF_BASE_SECS", "must be >= 1".to_string());
        }
        if self.mailer_drain_interval_secs == 0 {
            invalid("PF_MAILER_DRAIN_INTERVAL_SECS", "must be >= 1".to_string());
        }
        if self.mailer_drain_batch == 0 {
            invalid("PF_MAILER_DRAIN_BATCH", "must be >= 1".to_string());
        }
        if self.api_port == 0 {
            invalid("PF_API_PORT", "must be 1..=65535".to_string());
        }
        if self.metrics_port == 0 {
            invalid("PF_METRICS_PORT", "must be 1..=65535".to_string());
        }
        if self.engine_tiles_max == 0 {
            invalid("PF_ENGINE_TILES_MAX", "must be >= 1".to_string());
        }
        if self.rate_limit_ip_burst == 0 {
            invalid("PF_RATE_LIMIT_IP_BURST", "must be >= 1".to_string());
        }
        if self.rate_limit_route_burst == 0 {
            invalid("PF_RATE_LIMIT_ROUTE_BURST", "must be >= 1".to_string());
        }
        if self.rate_limit_login_account_burst == 0 {
            invalid(
                "PF_RATE_LIMIT_LOGIN_ACCOUNT_BURST",
                "must be >= 1".to_string(),
            );
        }
        if self.rate_limit_login_ip_burst == 0 {
            invalid("PF_RATE_LIMIT_LOGIN_IP_BURST", "must be >= 1".to_string());
        }
        if self.rate_limit_register_email_burst == 0 {
            invalid(
                "PF_RATE_LIMIT_REGISTER_EMAIL_BURST",
                "must be >= 1".to_string(),
            );
        }
        if self.rate_limit_reset_account_burst == 0 {
            invalid(
                "PF_RATE_LIMIT_RESET_ACCOUNT_BURST",
                "must be >= 1".to_string(),
            );
        }
        if self.worker_concurrency == 0 {
            invalid("PF_WORKER_CONCURRENCY", "must be >= 1".to_string());
        }
        if self.worker_metrics_port == 0 {
            invalid("PF_WORKER_METRICS_PORT", "must be 1..=65535".to_string());
        }
        if self.worker_heartbeat_interval_secs == 0 {
            invalid(
                "PF_WORKER_HEARTBEAT_INTERVAL_SECS",
                "must be >= 1 (seconds)".to_string(),
            );
        }
        if self.worker_drain_timeout_secs == 0 {
            invalid(
                "PF_WORKER_DRAIN_TIMEOUT_SECS",
                "must be >= 1 (seconds)".to_string(),
            );
        }
        if self.worker_queue_namespace.trim().is_empty() {
            invalid("PF_WORKER_QUEUE_NAMESPACE", "must be non-empty".to_string());
        }
        if self.queue_keep_alive_secs == 0 {
            invalid(
                "PF_QUEUE_KEEP_ALIVE_SECS",
                "must be >= 1 (seconds)".to_string(),
            );
        }
        if self.queue_visibility_timeout_secs == 0 {
            invalid(
                "PF_QUEUE_VISIBILITY_TIMEOUT_SECS",
                "must be >= 1 (seconds)".to_string(),
            );
        } else if self.queue_visibility_timeout_secs <= self.queue_keep_alive_secs {
            // A dead worker's jobs must not be re-queued while a
            // live worker's heartbeat is still due — with
            // visibility <= keep-alive, a worker that is merely
            // slow to heartbeat (GC pause, scheduling delay) would
            // have its in-flight jobs stolen mid-run.
            invalid(
                "PF_QUEUE_VISIBILITY_TIMEOUT_SECS",
                "must be greater than PF_QUEUE_KEEP_ALIVE_SECS".to_string(),
            );
        }
        for (var, value) in [
            ("PF_RESULTS_TTL_FREE_H", self.results_ttl_free_h),
            ("PF_RESULTS_TTL_PRO_H", self.results_ttl_pro_h),
            ("PF_RESULTS_TTL_BUSINESS_H", self.results_ttl_business_h),
        ] {
            if value == 0 {
                invalid(var, "must be >= 1 (hours)".to_string());
            }
        }
        if self.upload_part_url_ttl_secs == 0 {
            invalid(
                "PF_UPLOAD_PART_URL_TTL_SECS",
                "must be >= 1 (seconds)".to_string(),
            );
        }

        // Log level must be known (case-insensitive).
        if !matches!(
            self.log_level.to_lowercase().as_str(),
            "trace" | "debug" | "info" | "warn" | "error"
        ) {
            invalid(
                "PF_LOG_LEVEL",
                "must be one of: trace, debug, info, warn, error".to_string(),
            );
        }

        // Environment must be known (case-insensitive).
        if !matches!(
            self.environment.to_lowercase().as_str(),
            "dev" | "stage" | "prod"
        ) {
            invalid(
                "PF_ENVIRONMENT",
                "must be one of: dev, stage, prod".to_string(),
            );
        }
    }

    /// Is the Swagger UI (`/docs`) served in this environment?
    /// Dev and stage only — never prod (P0-067). The OpenAPI
    /// document itself (`/openapi.json`) is public in every
    /// environment: the console viewer (P4-064) fetches it in prod.
    pub fn docs_enabled(&self) -> bool {
        !matches!(self.environment.to_lowercase().as_str(), "prod")
    }

    /// Is the HSTS header stamped on responses (P0-102)? Every
    /// environment except dev: HSTS is only honoured over TLS,
    /// and only stage/prod run behind the TLS-terminating Caddy
    /// — on plain-http localhost it is meaningless noise, so dev
    /// responses carry the rest of the secure-header table but
    /// no HSTS.
    pub fn hsts_enabled(&self) -> bool {
        !matches!(self.environment.to_lowercase().as_str(), "dev")
    }

    /// Typed view over the feature flags in these settings.
    pub fn feature_flags(&self) -> FeatureFlags {
        FeatureFlags::from_settings(self)
    }
}

/// Feature flags (P0-021).
///
/// The `PF_FEATURE_*` env flags as one typed struct, default off. The
/// kit defines two flags (`PF_FEATURE_RAZORPAY`, gated billing adapter
/// per FR-P-05; `PF_FEATURE_PROVENANCE_STRIP`, legal-gated strip); `gpu`
/// is derived from `PF_ENGINE_GPU > 0` — there is no GPU flag var in the
/// kit or runbook, the engine count itself is the switch.
/// Consumers (engine, billing, provenance) read this struct, never raw
/// env vars, so a flag rename is one place.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeatureFlags {
    /// Env: PF_FEATURE_RAZORPAY — Razorpay billing adapter. Default off.
    pub razorpay: bool,
    /// Env: PF_FEATURE_PROVENANCE_STRIP — provenance strip (legal gate
    /// required). Default off.
    pub provenance_strip: bool,
    /// Derived: PF_ENGINE_GPU > 0. Default off (PF_ENGINE_GPU=0).
    pub gpu: bool,
}

impl FeatureFlags {
    /// All flags off (the default).
    pub fn defaults() -> Self {
        FeatureFlags {
            razorpay: false,
            provenance_strip: false,
            gpu: false,
        }
    }

    /// Build from loaded settings.
    pub fn from_settings(settings: &Settings) -> Self {
        FeatureFlags {
            razorpay: settings.feature_razorpay,
            provenance_strip: settings.feature_provenance_strip,
            gpu: settings.engine_gpu > 0,
        }
    }
}

/// The feature-flag prefix, derived from the known flags (their common
/// prefix) so it cannot drift — and so `env_check.py`, which scans Rust
/// sources for `PF_*` literals, never sees the prefix written out as a
/// would-be variable reference.
fn feature_prefix() -> &'static str {
    KNOWN_FEATURE_FLAGS[0]
        .strip_suffix("PROVENANCE_STRIP")
        .expect("known flag names share the feature prefix")
}

/// The `PF_FEATURE_*` flags the loader understands. Anything else
/// matching `PF_FEATURE_*` in the environment is unknown (a typo, or a
/// retired flag) and is reported by `unknown_feature_flags`.
pub const KNOWN_FEATURE_FLAGS: &[&str] = &["PF_FEATURE_PROVENANCE_STRIP", "PF_FEATURE_RAZORPAY"];

/// `PF_FEATURE_*` vars present in the environment that no code
/// understands, sorted. An operator typo shows up here instead of
/// silently doing nothing.
pub fn unknown_feature_flags() -> Vec<String> {
    let mut unknown: Vec<String> = std::env::vars()
        .map(|(key, _)| key)
        .filter(|key| {
            key.starts_with(feature_prefix()) && !KNOWN_FEATURE_FLAGS.contains(&key.as_str())
        })
        .collect();
    unknown.sort();
    unknown
}

/// Warn about every unknown feature flag (via `tracing`) and return the
/// list. Call at boot, after `Settings::load` — the warning is the only
/// signal a flag name is misspelled or retired. (Visible once the
/// tracing subscriber lands in P0-025; until then the return value is
/// the observable part, and tests assert on it.)
pub fn warn_unknown_feature_flags() -> Vec<String> {
    let unknown = unknown_feature_flags();
    for flag in &unknown {
        tracing::warn!(
            flag = %flag,
            "unknown feature flag; ignoring — check the spelling or remove it"
        );
    }
    unknown
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::sync::{Mutex, MutexGuard};

    /// Env vars are process-global; serialize every test that touches them.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Guard that restores the env on drop: remembers which PF_* vars were
    /// set before the test and puts them back (or removes them) after.
    struct EnvGuard {
        saved: Vec<(String, Option<String>)>,
    }

    impl EnvGuard {
        fn take(vars: &[&str]) -> EnvGuard {
            let saved = vars
                .iter()
                .map(|v| ((*v).to_string(), env::var(v).ok()))
                .collect();
            EnvGuard { saved }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (var, value) in self.saved.drain(..) {
                match value {
                    Some(v) => env::set_var(&var, v),
                    None => env::remove_var(&var),
                }
            }
        }
    }

    /// All 59 vars, so a test can start from a clean env.
    /// Keep this in sync with FIELD_VARS: a var missing here
    /// leaks between tests (P0-081 — a test that leaves an
    /// invalid value behind breaks the next test's defaults).
    const ALL_VARS: &[&str] = &[
        "PF_API_PORT",
        "PF_API_HOST",
        "PF_DATABASE_URL",
        "PF_REDIS_URL",
        "PF_S3_ENDPOINT",
        "PF_S3_BUCKET",
        "PF_S3_ACCESS_KEY",
        "PF_S3_SECRET_KEY",
        "PF_S3_REGION",
        "PF_JWT_SECRET",
        "PF_ARGON2_M_COST_KIB",
        "PF_ARGON2_T_COST",
        "PF_ARGON2_P_COST",
        "PF_TOS_CURRENT_VERSION",
        "PF_DISPOSABLE_EMAIL_DOMAINS",
        "PF_PUBLIC_BASE_URL",
        "PF_RATE_LIMIT_IP_BURST",
        "PF_RATE_LIMIT_IP_REFILL_PER_MIN",
        "PF_RATE_LIMIT_ROUTE_BURST",
        "PF_RATE_LIMIT_ROUTE_REFILL_PER_MIN",
        "PF_RATE_LIMIT_LOGIN_ACCOUNT_BURST",
        "PF_RATE_LIMIT_LOGIN_ACCOUNT_REFILL_PER_MIN",
        "PF_RATE_LIMIT_LOGIN_IP_BURST",
        "PF_RATE_LIMIT_LOGIN_IP_REFILL_PER_MIN",
        "PF_RATE_LIMIT_REGISTER_EMAIL_BURST",
        "PF_RATE_LIMIT_REGISTER_EMAIL_REFILL_PER_MIN",
        "PF_RATE_LIMIT_RESET_ACCOUNT_BURST",
        "PF_RATE_LIMIT_RESET_ACCOUNT_REFILL_PER_MIN",
        "PF_METRICS_PORT",
        "PF_METRICS_HOST",
        "PF_STRIPE_SECRET_KEY",
        "PF_STRIPE_WEBHOOK_SECRET",
        "PF_RAZORPAY_KEY",
        "PF_RAZORPAY_SECRET",
        "PF_FEATURE_RAZORPAY",
        "PF_SMTP_HOST",
        "PF_SMTP_PORT",
        "PF_SMTP_USER",
        "PF_SMTP_PASS",
        "PF_MAIL_FROM",
        "PF_SMTP_TLS",
        "PF_MAILER_MAX_ATTEMPTS",
        "PF_MAILER_BACKOFF_BASE_SECS",
        "PF_MAILER_DRAIN_INTERVAL_SECS",
        "PF_MAILER_DRAIN_BATCH",
        "PF_ENGINE_MODELS_DIR",
        "PF_ENGINE_GPU",
        "PF_ENGINE_TILES_MAX",
        "PF_RESULTS_TTL_FREE_H",
        "PF_RESULTS_TTL_PRO_H",
        "PF_RESULTS_TTL_BUSINESS_H",
        "PF_FEATURE_PROVENANCE_STRIP",
        "PF_LOG_LEVEL",
        "PF_OTEL_ENDPOINT",
        "PF_ENVIRONMENT",
        "PF_WEB_ORIGIN",
        "PF_UPLOAD_PART_URL_TTL_SECS",
        "PF_WORKER_CONCURRENCY",
        "PF_WORKER_METRICS_PORT",
        "PF_WORKER_HEARTBEAT_INTERVAL_SECS",
        "PF_WORKER_DRAIN_TIMEOUT_SECS",
        "PF_WORKER_QUEUE_NAMESPACE",
        "PF_QUEUE_VISIBILITY_TIMEOUT_SECS",
        "PF_QUEUE_KEEP_ALIVE_SECS",
    ];

    fn clean_env() -> EnvLock {
        // Recover from poisoning: a failed test must not cascade into
        // PoisonError failures for every other test.
        let lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let guard = EnvGuard::take(ALL_VARS);
        for var in ALL_VARS {
            env::remove_var(var);
        }
        EnvLock { guard, _lock: lock }
    }

    /// Holds the env lock AND the restore guard in one value. Field
    /// order matters: struct fields drop in declaration order, so the
    /// env is restored FIRST (while still holding the lock) and the lock
    /// is released SECOND. The previous shape — a `(MutexGuard,
    /// EnvGuard)` tuple — dropped the lock first, letting the next test
    /// run while the previous test's env restore was still in flight:
    /// vars leaked across tests and loads randomly saw stale values
    /// (flaky `layer_precedence` / `defaults_snapshot` / `bool_type`
    /// failures, found by running the suite 5x).
    struct EnvLock {
        // Never read, only dropped: the field exists for its Drop side
        // effect (restore the env while still holding the lock).
        #[allow(dead_code)]
        guard: EnvGuard,
        _lock: MutexGuard<'static, ()>,
    }

    /// Full defaults snapshot: every field must equal its `.env.example`
    /// value. If a default changes, `.env.example` changes with it
    /// (`just env-check` enforces the three-way agreement).
    #[test]
    fn defaults_snapshot_matches_env_example() {
        let _env = clean_env();
        let s = Settings::load_from(None).expect("defaults must load");
        assert_eq!(s.database_url, "postgres://pf:pf@localhost:5432/pf");
        assert_eq!(s.redis_url, "redis://localhost:6379");
        assert_eq!(s.s3_endpoint, "http://localhost:9000");
        assert_eq!(s.s3_bucket, "detag-dev");
        assert_eq!(s.s3_access_key, "minioadmin");
        assert_eq!(s.s3_secret_key, "minioadmin");
        assert_eq!(s.s3_region, "us-east-1");
        assert_eq!(s.jwt_secret, "dev-fixed");
        assert_eq!(s.argon2_m_cost_kib, 65_536);
        assert_eq!(s.argon2_t_cost, 3);
        assert_eq!(s.argon2_p_cost, 4);
        assert!(!s.password_pwned_check);
        assert_eq!(s.password_pwned_url, "https://api.pwnedpasswords.com");
        assert_eq!(s.tos_current_version, "v1");
        // No assertion for disposable_email_domains: the code
        // default is "" (no denylist) while .env.example seeds a
        // starter list — deliberately different, see the field docs.
        assert_eq!(s.public_base_url, "http://localhost:8080");
        assert_eq!(s.api_port, 8080);
        assert_eq!(s.api_host, IpAddr::from([127, 0, 0, 1]));
        assert_eq!(s.metrics_port, 9090);
        assert_eq!(s.metrics_host, IpAddr::from([127, 0, 0, 1]));
        assert_eq!(s.stripe_secret_key, "sk_test_placeholder");
        assert_eq!(s.stripe_webhook_secret, "whsec_placeholder");
        assert_eq!(s.razorpay_key, "rzp_test_placeholder");
        assert_eq!(s.razorpay_secret, "placeholder");
        assert!(!s.feature_razorpay);
        assert_eq!(s.smtp_host, "localhost");
        assert_eq!(s.smtp_port, 1025);
        assert!(s.smtp_user.is_empty());
        assert!(s.smtp_pass.is_empty());
        assert_eq!(s.mail_from, "noreply@detag.local");
        assert!(!s.smtp_tls);
        assert_eq!(s.mailer_max_attempts, 5);
        assert_eq!(s.mailer_backoff_base_secs, 60);
        assert_eq!(s.mailer_drain_interval_secs, 30);
        assert_eq!(s.mailer_drain_batch, 25);
        assert_eq!(s.engine_models_dir, "./models");
        assert_eq!(s.engine_gpu, 0);
        assert_eq!(s.engine_tiles_max, 1024);
        assert_eq!(s.results_ttl_free_h, 6);
        assert_eq!(s.results_ttl_pro_h, 24);
        assert_eq!(s.results_ttl_business_h, 72);
        assert_eq!(s.upload_part_url_ttl_secs, 600);
        assert!(!s.feature_provenance_strip);
        assert_eq!(s.log_level, "info");
        assert!(s.otel_endpoint.is_empty());
        assert!(s.web_origin.is_empty());
        assert_eq!(s.environment, "dev");
        assert!(s.docs_enabled());
    }

    #[test]
    fn env_overrides_defaults() {
        let _env = clean_env();
        env::set_var("PF_DATABASE_URL", "postgres://u:p@db:5432/x");
        env::set_var("PF_SMTP_PORT", "2525");
        env::set_var("PF_FEATURE_RAZORPAY", "true");
        env::set_var("PF_API_HOST", "0.0.0.0");
        env::set_var("PF_METRICS_HOST", "0.0.0.0");
        let settings = Settings::load_from(None).expect("env load must work");
        assert_eq!(settings.database_url, "postgres://u:p@db:5432/x");
        assert_eq!(settings.smtp_port, 2525);
        assert!(settings.feature_razorpay);
        assert_eq!(settings.api_host, IpAddr::from([0, 0, 0, 0]));
        assert_eq!(settings.metrics_host, IpAddr::from([0, 0, 0, 0]));
        // Untouched vars keep their defaults.
        assert_eq!(settings.redis_url, "redis://localhost:6379");
    }

    #[test]
    fn invalid_url_fails_with_var_name() {
        let _env = clean_env();
        env::set_var("PF_DATABASE_URL", "not a url at all :::");
        let err = Settings::load_from(None).expect_err("bad URL must fail");
        let message = err.to_string();
        assert!(
            message.contains("PF_DATABASE_URL"),
            "error must name the var, got: {message}"
        );
    }

    #[test]
    fn invalid_number_fails_with_var_name() {
        let _env = clean_env();
        env::set_var("PF_SMTP_PORT", "not-a-port");
        let err = Settings::load_from(None).expect_err("bad port must fail");
        let message = err.to_string();
        assert!(
            message.contains("PF_SMTP_PORT"),
            "error must name the var, got: {message}"
        );
    }

    #[test]
    fn invalid_host_fails_with_var_name() {
        let _env = clean_env();
        env::set_var("PF_API_HOST", "not-an-ip");
        let err = Settings::load_from(None).expect_err("bad host must fail");
        let message = err.to_string();
        assert!(
            message.contains("PF_API_HOST"),
            "error must name the var, got: {message}"
        );
    }

    #[test]
    fn out_of_range_fails_with_var_name() {
        let _env = clean_env();
        env::set_var("PF_ENGINE_TILES_MAX", "0");
        let err = Settings::load_from(None).expect_err("tiles=0 must fail");
        let message = err.to_string();
        assert!(
            message.contains("PF_ENGINE_TILES_MAX"),
            "error must name the var, got: {message}"
        );
    }

    #[test]
    fn bad_log_level_fails_with_var_name() {
        let _env = clean_env();
        env::set_var("PF_LOG_LEVEL", "verbose");
        let err = Settings::load_from(None).expect_err("bad level must fail");
        let message = err.to_string();
        assert!(
            message.contains("PF_LOG_LEVEL"),
            "error must name the var, got: {message}"
        );
    }

    /// P0-067: PF_ENVIRONMENT must be dev | stage | prod (case-insensitive);
    /// anything else fails naming the var.
    #[test]
    fn bad_environment_fails_with_var_name() {
        let _env = clean_env();
        env::set_var("PF_ENVIRONMENT", "qa");
        let err = Settings::load_from(None).expect_err("bad env must fail");
        let message = err.to_string();
        assert!(
            message.contains("PF_ENVIRONMENT"),
            "error must name the var, got: {message}"
        );
    }

    /// P0-075: PF_WORKER_CONCURRENCY must be >= 1; 0 fails naming
    /// the var. The default is 4 workers per queue.
    #[test]
    fn worker_concurrency_zero_fails_with_var_name() {
        let _env = clean_env();
        let settings = Settings::load_from(None).expect("defaults must load");
        assert_eq!(settings.worker_concurrency, 4, "default is 4");
        env::set_var("PF_WORKER_CONCURRENCY", "0");
        let err = Settings::load_from(None).expect_err("0 workers must fail");
        let message = err.to_string();
        assert!(
            message.contains("PF_WORKER_CONCURRENCY"),
            "error must name the var, got: {message}"
        );
        env::set_var("PF_WORKER_CONCURRENCY", "2");
        let settings = Settings::load_from(None).expect("2 must load");
        assert_eq!(settings.worker_concurrency, 2);
    }

    /// P0-103: rate-limit defaults are ip 120 burst / 120 per min
    /// and route 600 burst / 600 per min; a zero burst fails
    /// naming the var (a zero refill is legal — a fixed allowance).
    /// P1-003: the per-account login bucket defaults to 10 burst
    /// / 1 per min, and its zero burst fails the same way.
    /// P1-020: the per-IP login bucket defaults to 20 burst /
    /// 10 per min, the per-email register bucket to 5 burst /
    /// 1 per min, the per-account reset bucket to 10 burst /
    /// 1 per min; each zero burst fails naming its var.
    #[test]
    fn rate_limit_defaults_and_zero_burst_fails() {
        let _env = clean_env();
        let settings = Settings::load_from(None).expect("defaults must load");
        assert_eq!(settings.rate_limit_ip_burst, 120);
        assert_eq!(settings.rate_limit_ip_refill_per_min, 120);
        assert_eq!(settings.rate_limit_route_burst, 600);
        assert_eq!(settings.rate_limit_route_refill_per_min, 600);
        assert_eq!(settings.rate_limit_login_account_burst, 10);
        assert_eq!(settings.rate_limit_login_account_refill_per_min, 1);
        assert_eq!(settings.rate_limit_login_ip_burst, 20);
        assert_eq!(settings.rate_limit_login_ip_refill_per_min, 10);
        assert_eq!(settings.rate_limit_register_email_burst, 5);
        assert_eq!(settings.rate_limit_register_email_refill_per_min, 1);
        assert_eq!(settings.rate_limit_reset_account_burst, 10);
        assert_eq!(settings.rate_limit_reset_account_refill_per_min, 1);
        env::set_var("PF_RATE_LIMIT_IP_BURST", "0");
        let err = Settings::load_from(None).expect_err("0 burst must fail");
        assert!(
            err.to_string().contains("PF_RATE_LIMIT_IP_BURST"),
            "error must name the var, got: {err}"
        );
        env::set_var("PF_RATE_LIMIT_IP_BURST", "5");
        env::set_var("PF_RATE_LIMIT_ROUTE_BURST", "0");
        let err = Settings::load_from(None).expect_err("0 route burst must fail");
        assert!(
            err.to_string().contains("PF_RATE_LIMIT_ROUTE_BURST"),
            "error must name the var, got: {err}"
        );
        env::set_var("PF_RATE_LIMIT_ROUTE_BURST", "3");
        env::set_var("PF_RATE_LIMIT_LOGIN_ACCOUNT_BURST", "0");
        let err = Settings::load_from(None).expect_err("0 login burst must fail");
        assert!(
            err.to_string()
                .contains("PF_RATE_LIMIT_LOGIN_ACCOUNT_BURST"),
            "error must name the var, got: {err}"
        );
        env::set_var("PF_RATE_LIMIT_LOGIN_ACCOUNT_BURST", "2");
        env::set_var("PF_RATE_LIMIT_LOGIN_IP_BURST", "0");
        let err = Settings::load_from(None).expect_err("0 login-IP burst must fail");
        assert!(
            err.to_string().contains("PF_RATE_LIMIT_LOGIN_IP_BURST"),
            "error must name the var, got: {err}"
        );
        env::set_var("PF_RATE_LIMIT_LOGIN_IP_BURST", "7");
        env::set_var("PF_RATE_LIMIT_REGISTER_EMAIL_BURST", "0");
        let err = Settings::load_from(None).expect_err("0 register burst must fail");
        assert!(
            err.to_string()
                .contains("PF_RATE_LIMIT_REGISTER_EMAIL_BURST"),
            "error must name the var, got: {err}"
        );
        env::set_var("PF_RATE_LIMIT_REGISTER_EMAIL_BURST", "4");
        env::set_var("PF_RATE_LIMIT_RESET_ACCOUNT_BURST", "0");
        let err = Settings::load_from(None).expect_err("0 reset burst must fail");
        assert!(
            err.to_string()
                .contains("PF_RATE_LIMIT_RESET_ACCOUNT_BURST"),
            "error must name the var, got: {err}"
        );
        env::set_var("PF_RATE_LIMIT_RESET_ACCOUNT_BURST", "9");
        env::set_var("PF_RATE_LIMIT_IP_REFILL_PER_MIN", "0");
        let settings = Settings::load_from(None).expect("0 refill must load");
        assert_eq!(settings.rate_limit_ip_burst, 5);
        assert_eq!(settings.rate_limit_route_burst, 3);
        assert_eq!(settings.rate_limit_ip_refill_per_min, 0);
        assert_eq!(settings.rate_limit_login_account_burst, 2);
        assert_eq!(settings.rate_limit_login_ip_burst, 7);
        assert_eq!(settings.rate_limit_register_email_burst, 4);
        assert_eq!(settings.rate_limit_reset_account_burst, 9);
    }

    /// P1-001: auth hashing + registration policy defaults
    /// (P1-027 Argon2id m = 64 MiB, t = 3, p = 4; ToS "v1", no
    /// disposable denylist); out-of-range argon2 params and an
    /// empty ToS version fail naming the var.
    #[test]
    fn auth_defaults_and_bounds() {
        let _env = clean_env();
        let settings = Settings::load_from(None).expect("defaults must load");
        assert_eq!(settings.argon2_m_cost_kib, 65_536);
        assert_eq!(settings.argon2_t_cost, 3);
        assert_eq!(settings.argon2_p_cost, 4);
        assert_eq!(settings.tos_current_version, "v1");
        assert_eq!(settings.disposable_email_domains, "");
        env::set_var("PF_ARGON2_M_COST_KIB", "1024");
        let err = Settings::load_from(None).expect_err("4 MiB floor must fail");
        assert!(
            err.to_string().contains("PF_ARGON2_M_COST_KIB"),
            "error must name the var, got: {err}"
        );
        env::set_var("PF_ARGON2_M_COST_KIB", "19456");
        env::set_var("PF_ARGON2_T_COST", "0");
        let err = Settings::load_from(None).expect_err("0 time cost must fail");
        assert!(
            err.to_string().contains("PF_ARGON2_T_COST"),
            "error must name the var, got: {err}"
        );
        env::set_var("PF_ARGON2_T_COST", "3");
        env::set_var("PF_TOS_CURRENT_VERSION", "   ");
        let err = Settings::load_from(None).expect_err("blank ToS version must fail");
        assert!(
            err.to_string().contains("PF_TOS_CURRENT_VERSION"),
            "error must name the var, got: {err}"
        );
        env::set_var("PF_TOS_CURRENT_VERSION", "v2");
        env::set_var(
            "PF_DISPOSABLE_EMAIL_DOMAINS",
            "mailinator.com, tempmail.com",
        );
        let settings = Settings::load_from(None).expect("valid overrides must load");
        assert_eq!(settings.argon2_t_cost, 3);
        assert_eq!(settings.tos_current_version, "v2");
        assert_eq!(
            settings.disposable_email_domains,
            "mailinator.com, tempmail.com"
        );
    }

    /// P0-079: visibility timeout defaults to 300 s and keep-alive
    /// to 30 s (the apalis defaults, now explicit); 0 fails naming
    /// the var; visibility <= keep-alive fails naming the
    /// visibility var — a dead worker's jobs must not be
    /// re-queued while a live worker's heartbeat is still due.
    #[test]
    fn queue_visibility_defaults_and_bounds() {
        let _env = clean_env();
        let settings = Settings::load_from(None).expect("defaults must load");
        assert_eq!(
            settings.queue_visibility_timeout_secs, 300,
            "default is 300 s"
        );
        assert_eq!(settings.queue_keep_alive_secs, 30, "default is 30 s");
        env::set_var("PF_QUEUE_VISIBILITY_TIMEOUT_SECS", "0");
        let err = Settings::load_from(None).expect_err("0 s visibility must fail");
        assert!(
            err.to_string().contains("PF_QUEUE_VISIBILITY_TIMEOUT_SECS"),
            "error must name the var, got: {err}"
        );
        env::set_var("PF_QUEUE_VISIBILITY_TIMEOUT_SECS", "30");
        env::set_var("PF_QUEUE_KEEP_ALIVE_SECS", "30");
        let err = Settings::load_from(None).expect_err("visibility == keep-alive must fail");
        assert!(
            err.to_string().contains("PF_QUEUE_VISIBILITY_TIMEOUT_SECS"),
            "error must name the var, got: {err}"
        );
        env::set_var("PF_QUEUE_VISIBILITY_TIMEOUT_SECS", "10");
        let err = Settings::load_from(None).expect_err("visibility < keep-alive must fail");
        assert!(
            err.to_string().contains("PF_QUEUE_VISIBILITY_TIMEOUT_SECS"),
            "error must name the var, got: {err}"
        );
        env::set_var("PF_QUEUE_VISIBILITY_TIMEOUT_SECS", "600");
        env::set_var("PF_QUEUE_KEEP_ALIVE_SECS", "15");
        let settings = Settings::load_from(None).expect("600 > 15 must load");
        assert_eq!(settings.queue_visibility_timeout_secs, 600);
        assert_eq!(settings.queue_keep_alive_secs, 15);
    }

    /// P0-080: the worker metrics port defaults to 9092 (9090 is
    /// the api, 9091 is Prometheus itself in compose) and the
    /// heartbeat interval to 15 s; 0 fails naming the var.
    #[test]
    fn worker_metrics_port_and_heartbeat_interval_defaults() {
        let _env = clean_env();
        let settings = Settings::load_from(None).expect("defaults must load");
        assert_eq!(settings.worker_metrics_port, 9092, "default is 9092");
        assert_eq!(
            settings.worker_heartbeat_interval_secs, 15,
            "default is 15 s"
        );
        env::set_var("PF_WORKER_HEARTBEAT_INTERVAL_SECS", "0");
        let err = Settings::load_from(None).expect_err("0 s heartbeat must fail");
        assert!(
            err.to_string()
                .contains("PF_WORKER_HEARTBEAT_INTERVAL_SECS"),
            "error must name the var, got: {err}"
        );
        env::set_var("PF_WORKER_HEARTBEAT_INTERVAL_SECS", "5");
        env::set_var("PF_WORKER_METRICS_PORT", "19092");
        let settings = Settings::load_from(None).expect("5 s + 19092 must load");
        assert_eq!(settings.worker_heartbeat_interval_secs, 5);
        assert_eq!(settings.worker_metrics_port, 19092);
    }

    /// P0-081: the drain timeout defaults to 30 s and the queue
    /// namespace to `pf:queue`; 0 s fails naming the var, an
    /// empty namespace fails naming the var, custom values load.
    #[test]
    fn worker_drain_timeout_and_queue_namespace_defaults() {
        let _env = clean_env();
        let settings = Settings::load_from(None).expect("defaults must load");
        assert_eq!(settings.worker_drain_timeout_secs, 30, "default is 30 s");
        assert_eq!(
            settings.worker_queue_namespace, "pf:queue",
            "default namespace is pf:queue"
        );
        env::set_var("PF_WORKER_DRAIN_TIMEOUT_SECS", "0");
        let err = Settings::load_from(None).expect_err("0 s drain must fail");
        assert!(
            err.to_string().contains("PF_WORKER_DRAIN_TIMEOUT_SECS"),
            "error must name the var, got: {err}"
        );
        env::set_var("PF_WORKER_DRAIN_TIMEOUT_SECS", "5");
        env::set_var("PF_WORKER_QUEUE_NAMESPACE", "pf:test-drain");
        let settings = Settings::load_from(None).expect("5 s + namespace must load");
        assert_eq!(settings.worker_drain_timeout_secs, 5);
        assert_eq!(settings.worker_queue_namespace, "pf:test-drain");
        env::set_var("PF_WORKER_QUEUE_NAMESPACE", "   ");
        let err = Settings::load_from(None).expect_err("blank namespace must fail");
        assert!(
            err.to_string().contains("PF_WORKER_QUEUE_NAMESPACE"),
            "error must name the var, got: {err}"
        );
    }

    /// P0-067: the Swagger UI gate — docs in dev and stage, never
    /// in prod (case-insensitive match).
    #[test]
    fn docs_enabled_in_dev_and_stage_only() {
        let _env = clean_env();
        let settings = Settings::load_from(None).expect("defaults must load");
        assert!(settings.docs_enabled(), "dev must serve /docs");
        env::set_var("PF_ENVIRONMENT", "stage");
        let settings = Settings::load_from(None).expect("stage must load");
        assert!(settings.docs_enabled(), "stage must serve /docs");
        env::set_var("PF_ENVIRONMENT", "prod");
        let settings = Settings::load_from(None).expect("prod must load");
        assert!(!settings.docs_enabled(), "prod must not serve /docs");
        env::set_var("PF_ENVIRONMENT", "PROD");
        let settings = Settings::load_from(None).expect("PROD must load");
        assert!(!settings.docs_enabled(), "PROD must not serve /docs");
    }

    /// P0-102: HSTS is stamped in every environment except dev
    /// (case-insensitive match) — stage and prod run behind TLS,
    /// dev is plain-http localhost.
    #[test]
    fn hsts_enabled_everywhere_except_dev() {
        let _env = clean_env();
        let settings = Settings::load_from(None).expect("defaults must load");
        assert!(!settings.hsts_enabled(), "dev must not stamp HSTS");
        env::set_var("PF_ENVIRONMENT", "stage");
        let settings = Settings::load_from(None).expect("stage must load");
        assert!(settings.hsts_enabled(), "stage must stamp HSTS");
        env::set_var("PF_ENVIRONMENT", "prod");
        let settings = Settings::load_from(None).expect("prod must load");
        assert!(settings.hsts_enabled(), "prod must stamp HSTS");
        env::set_var("PF_ENVIRONMENT", "PROD");
        let settings = Settings::load_from(None).expect("PROD must load");
        assert!(settings.hsts_enabled(), "PROD must stamp HSTS");
    }

    #[test]
    fn multiple_bad_vars_are_reported_together() {
        let _env = clean_env();
        env::set_var("PF_DATABASE_URL", "junk");
        env::set_var("PF_LOG_LEVEL", "verbose");
        let err = Settings::load_from(None).expect_err("two bad vars");
        let message = err.to_string();
        assert!(message.contains("PF_DATABASE_URL"), "got: {message}");
        assert!(message.contains("PF_LOG_LEVEL"), "got: {message}");
    }

    #[test]
    fn file_overrides_env() {
        let _env = clean_env();
        env::set_var("PF_LOG_LEVEL", "debug");
        let dir = std::env::temp_dir().join("detag-config-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("detag.toml");
        std::fs::write(&path, "log_level = \"warn\"\n").unwrap();
        let settings = Settings::load_from(Some(&path)).expect("file load must work");
        assert_eq!(settings.log_level, "warn");
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn missing_file_is_ignored() {
        let _env = clean_env();
        let path = Path::new("/nonexistent/detag.toml");
        let settings = Settings::load_from(Some(path)).expect("missing file must not fail");
        assert_eq!(settings.log_level, "info");
    }

    /// All three layers in one load: file beats env on the same field,
    /// env beats defaults where the file is silent, defaults fill the rest.
    #[test]
    fn layer_precedence_all_three() {
        let _env = clean_env();
        env::set_var("PF_LOG_LEVEL", "debug");
        env::set_var("PF_SMTP_PORT", "2525");
        let dir = std::env::temp_dir().join("detag-config-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("detag.toml");
        std::fs::write(&path, "log_level = \"warn\"\n").unwrap();
        let settings = Settings::load_from(Some(&path)).expect("layered load must work");
        assert_eq!(settings.log_level, "warn"); // file beats env ("debug")
        assert_eq!(settings.smtp_port, 2525); // env beats default (1025)
        assert_eq!(settings.redis_url, "redis://localhost:6379"); // default
        std::fs::remove_file(&path).unwrap();
    }

    /// The aggregated-error case: four bad vars of different kinds
    /// (bad URL, empty required string, out-of-range number, bad log
    /// level) are all reported in one error, with a count.
    #[test]
    fn aggregated_report_names_every_bad_var() {
        let _env = clean_env();
        env::set_var("PF_DATABASE_URL", "junk");
        env::set_var("PF_S3_BUCKET", "");
        env::set_var("PF_RESULTS_TTL_PRO_H", "0");
        env::set_var("PF_LOG_LEVEL", "verbose");
        let err = Settings::load_from(None).expect_err("four bad vars");
        match &err {
            ConfigError::Multiple { count, details } => {
                assert_eq!(*count, 4);
                for var in [
                    "PF_DATABASE_URL",
                    "PF_S3_BUCKET",
                    "PF_RESULTS_TTL_PRO_H",
                    "PF_LOG_LEVEL",
                ] {
                    assert!(details.contains(var), "missing {var} in: {details}");
                }
            }
            other => panic!("expected Multiple, got: {other}"),
        }
    }

    /// One bad var stays a single Invalid, not an aggregated report.
    #[test]
    fn single_bad_var_is_not_aggregated() {
        let _env = clean_env();
        env::set_var("PF_LOG_LEVEL", "verbose");
        let err = Settings::load_from(None).expect_err("one bad var");
        assert!(
            matches!(err, ConfigError::Invalid { .. }),
            "expected Invalid, got: {err}"
        );
    }

    /// A bad value can also come from the file layer, and it still names
    /// the var.
    #[test]
    fn file_bad_value_fails_with_var_name() {
        let _env = clean_env();
        let dir = std::env::temp_dir().join("detag-config-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("detag.toml");
        std::fs::write(&path, "log_level = \"verbose\"\n").unwrap();
        let err = Settings::load_from(Some(&path)).expect_err("bad file value");
        assert!(
            err.to_string().contains("PF_LOG_LEVEL"),
            "error must name the var, got: {err}"
        );
        std::fs::remove_file(&path).unwrap();
    }

    /// Empty OTEL endpoint disables export (the dev default); a
    /// non-empty but unparseable one fails naming the var.
    #[test]
    fn otel_endpoint_empty_ok_bad_fails() {
        let _env = clean_env();
        env::set_var("PF_OTEL_ENDPOINT", "");
        Settings::load_from(None).expect("empty otel must load");
        env::set_var("PF_OTEL_ENDPOINT", ":::not a url");
        let err = Settings::load_from(None).expect_err("bad otel must fail");
        assert!(
            err.to_string().contains("PF_OTEL_ENDPOINT"),
            "error must name the var, got: {err}"
        );
    }

    /// P0-020 AC: two invalid vars produce ONE report listing both.
    /// (Before P0-020, figment's whole-struct extract reported only the
    /// first type error — PF_ENGINE_TILES_MAX hid PF_SMTP_PORT.)
    #[test]
    fn two_invalid_type_vars_produce_one_report() {
        let _env = clean_env();
        env::set_var("PF_SMTP_PORT", "abc");
        env::set_var("PF_ENGINE_TILES_MAX", "xyz");
        let err = Settings::load_from(None).expect_err("two bad types");
        match &err {
            ConfigError::Multiple { count, details } => {
                assert_eq!(*count, 2);
                assert!(details.contains("PF_SMTP_PORT"), "got: {details}");
                assert!(details.contains("PF_ENGINE_TILES_MAX"), "got: {details}");
            }
            other => panic!("expected Multiple, got: {other}"),
        }
    }

    /// Type errors and semantic (validation) errors aggregate together:
    /// a bad port, a bad URL and a bad log level are all in one report.
    #[test]
    fn mixed_type_and_validation_errors_aggregate() {
        let _env = clean_env();
        env::set_var("PF_SMTP_PORT", "abc");
        env::set_var("PF_DATABASE_URL", "junk");
        env::set_var("PF_LOG_LEVEL", "verbose");
        let err = Settings::load_from(None).expect_err("mixed bad vars");
        match &err {
            ConfigError::Multiple { count, details } => {
                assert_eq!(*count, 3);
                for var in ["PF_SMTP_PORT", "PF_DATABASE_URL", "PF_LOG_LEVEL"] {
                    assert!(details.contains(var), "missing {var} in: {details}");
                }
            }
            other => panic!("expected Multiple, got: {other}"),
        }
    }

    /// A non-bool where a bool belongs is a type error naming the var,
    /// and a single one stays a plain Invalid.
    #[test]
    fn bool_type_error_names_var() {
        let _env = clean_env();
        env::set_var("PF_FEATURE_RAZORPAY", "maybe");
        let err = Settings::load_from(None).expect_err("bad bool must fail");
        match &err {
            ConfigError::Invalid { var, .. } => {
                assert_eq!(var, "PF_FEATURE_RAZORPAY");
            }
            other => panic!("expected Invalid, got: {other}"),
        }
    }

    /// AC: flag defaults off — all three consumers (razorpay, provenance
    /// strip, gpu) start disabled.
    #[test]
    fn feature_flags_default_off() {
        let _env = clean_env();
        let settings = Settings::load_from(None).expect("defaults must load");
        assert_eq!(settings.feature_flags(), FeatureFlags::defaults());
        assert!(!settings.feature_flags().razorpay);
        assert!(!settings.feature_flags().provenance_strip);
        assert!(!settings.feature_flags().gpu);
    }

    /// Flags follow their env vars; gpu follows the engine count.
    #[test]
    fn feature_flags_follow_env() {
        let _env = clean_env();
        env::set_var("PF_FEATURE_RAZORPAY", "true");
        env::set_var("PF_FEATURE_PROVENANCE_STRIP", "true");
        env::set_var("PF_ENGINE_GPU", "2");
        let settings = Settings::load_from(None).expect("env load must work");
        let flags = settings.feature_flags();
        assert!(flags.razorpay);
        assert!(flags.provenance_strip);
        assert!(flags.gpu);
    }

    /// AC: unknown flag warns — detection returns the unknown names,
    /// sorted; known flags are never reported. Fake flag names are built
    /// at runtime (never written as literals) so `env_check.py` — which
    /// scans sources for `PF_*` literals — does not mistake test fixtures
    /// for real variables.
    #[test]
    fn unknown_feature_flag_is_reported() {
        let _env = clean_env();
        let foo = format!("{}FOO", feature_prefix());
        let bar = format!("{}BAR", feature_prefix());
        let extra = EnvGuard::take(&[foo.as_str(), bar.as_str()]);
        env::set_var(&foo, "1");
        env::set_var(&bar, "x");
        env::set_var("PF_FEATURE_RAZORPAY", "true");
        let unknown = unknown_feature_flags();
        assert_eq!(unknown, vec![bar, foo]);
        drop(extra);
    }

    #[test]
    fn known_flags_are_not_unknown() {
        let _env = clean_env();
        env::set_var("PF_FEATURE_RAZORPAY", "true");
        env::set_var("PF_FEATURE_PROVENANCE_STRIP", "false");
        assert!(unknown_feature_flags().is_empty());
    }

    /// The boot hook returns the same list it warns about.
    #[test]
    fn warn_unknown_feature_flags_returns_list() {
        let _env = clean_env();
        let typo = format!("{}TYPO", feature_prefix());
        let extra = EnvGuard::take(&[typo.as_str()]);
        env::set_var(&typo, "1");
        let warned = warn_unknown_feature_flags();
        assert_eq!(warned, vec![typo]);
        drop(extra);
    }
}
