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

use figment::{
    providers::{Env, Format, Serialized, Toml},
    Figment,
};
use serde::{Deserialize, Serialize};
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
    /// Env: PF_PUBLIC_BASE_URL
    pub public_base_url: String,
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
    /// Env: PF_FEATURE_PROVENANCE_STRIP — stays OFF until the owner signs
    /// the legal-review gate (docs/02-REQUIREMENTS/05-compliance-aup.md).
    pub feature_provenance_strip: bool,
    /// Env: PF_LOG_LEVEL — trace | debug | info | warn | error.
    pub log_level: String,
    /// Env: PF_OTEL_ENDPOINT — empty disables OpenTelemetry export.
    pub otel_endpoint: String,
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
            public_base_url: "http://localhost:8080".to_string(),
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
            engine_models_dir: "./models".to_string(),
            engine_gpu: 0,
            engine_tiles_max: 1024,
            results_ttl_free_h: 6,
            results_ttl_pro_h: 24,
            results_ttl_business_h: 72,
            feature_provenance_strip: false,
            log_level: "info".to_string(),
            otel_endpoint: String::new(),
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
    ("public_base_url", "PF_PUBLIC_BASE_URL"),
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
    ("engine_models_dir", "PF_ENGINE_MODELS_DIR"),
    ("engine_gpu", "PF_ENGINE_GPU"),
    ("engine_tiles_max", "PF_ENGINE_TILES_MAX"),
    ("results_ttl_free_h", "PF_RESULTS_TTL_FREE_H"),
    ("results_ttl_pro_h", "PF_RESULTS_TTL_PRO_H"),
    ("results_ttl_business_h", "PF_RESULTS_TTL_BUSINESS_H"),
    ("feature_provenance_strip", "PF_FEATURE_PROVENANCE_STRIP"),
    ("log_level", "PF_LOG_LEVEL"),
    ("otel_endpoint", "PF_OTEL_ENDPOINT"),
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
    pub fn load_from(file: Option<&Path>) -> Result<Settings, ConfigError> {
        let mut figment = Figment::new()
            .merge(Serialized::defaults(Settings::default()))
            .merge(Env::prefixed("PF_"));
        if let Some(path) = file {
            if path.exists() {
                figment = figment.merge(Toml::file(path));
            }
        }
        let settings: Settings = figment.extract().map_err(|err| {
            let message = err.to_string();
            // Figment reports the key in env case (e.g. "SMTP_PORT"), so
            // match field names case-insensitively.
            let lower = message.to_lowercase();
            let var = FIELD_VARS
                .iter()
                .find(|(field, _)| lower.contains(field))
                .map(|(_, var)| (*var).to_string())
                .unwrap_or_else(|| "PF_*".to_string());
            ConfigError::Invalid {
                var,
                reason: message,
            }
        })?;
        settings.validate()?;
        Ok(settings)
    }

    /// Check every rule; returns all problems at once.
    fn validate(&self) -> Result<(), ConfigError> {
        let mut problems: Vec<ConfigError> = Vec::new();
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
        if self.smtp_port == 0 {
            invalid("PF_SMTP_PORT", "must be 1..=65535".to_string());
        }
        if self.engine_tiles_max == 0 {
            invalid("PF_ENGINE_TILES_MAX", "must be >= 1".to_string());
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

        if problems.is_empty() {
            Ok(())
        } else if problems.len() == 1 {
            Err(problems.into_iter().next().unwrap())
        } else {
            Err(ConfigError::multiple(problems))
        }
    }
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

    /// All 28 vars, so a test can start from a clean env.
    const ALL_VARS: &[&str] = &[
        "PF_DATABASE_URL",
        "PF_REDIS_URL",
        "PF_S3_ENDPOINT",
        "PF_S3_BUCKET",
        "PF_S3_ACCESS_KEY",
        "PF_S3_SECRET_KEY",
        "PF_S3_REGION",
        "PF_JWT_SECRET",
        "PF_PUBLIC_BASE_URL",
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
        "PF_ENGINE_MODELS_DIR",
        "PF_ENGINE_GPU",
        "PF_ENGINE_TILES_MAX",
        "PF_RESULTS_TTL_FREE_H",
        "PF_RESULTS_TTL_PRO_H",
        "PF_RESULTS_TTL_BUSINESS_H",
        "PF_FEATURE_PROVENANCE_STRIP",
        "PF_LOG_LEVEL",
        "PF_OTEL_ENDPOINT",
    ];

    fn clean_env() -> (MutexGuard<'static, ()>, EnvGuard) {
        // Recover from poisoning: a failed test must not cascade into
        // PoisonError failures for every other test.
        let lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let guard = EnvGuard::take(ALL_VARS);
        for var in ALL_VARS {
            env::remove_var(var);
        }
        (lock, guard)
    }

    #[test]
    fn defaults_match_env_example() {
        let _env = clean_env();
        let settings = Settings::load_from(None).expect("defaults must load");
        assert_eq!(settings.database_url, "postgres://pf:pf@localhost:5432/pf");
        assert_eq!(settings.redis_url, "redis://localhost:6379");
        assert_eq!(settings.s3_endpoint, "http://localhost:9000");
        assert_eq!(settings.s3_bucket, "detag-dev");
        assert_eq!(settings.smtp_port, 1025);
        assert_eq!(settings.engine_tiles_max, 1024);
        assert_eq!(settings.engine_gpu, 0);
        assert_eq!(
            (
                settings.results_ttl_free_h,
                settings.results_ttl_pro_h,
                settings.results_ttl_business_h
            ),
            (6, 24, 72)
        );
        assert!(!settings.feature_razorpay);
        assert!(!settings.feature_provenance_strip);
        assert_eq!(settings.log_level, "info");
        assert!(settings.otel_endpoint.is_empty());
    }

    #[test]
    fn env_overrides_defaults() {
        let _env = clean_env();
        env::set_var("PF_DATABASE_URL", "postgres://u:p@db:5432/x");
        env::set_var("PF_SMTP_PORT", "2525");
        env::set_var("PF_FEATURE_RAZORPAY", "true");
        let settings = Settings::load_from(None).expect("env load must work");
        assert_eq!(settings.database_url, "postgres://u:p@db:5432/x");
        assert_eq!(settings.smtp_port, 2525);
        assert!(settings.feature_razorpay);
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
}
