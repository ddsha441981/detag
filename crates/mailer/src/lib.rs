//! Detag mailer: SMTP + template rendering + outbox pattern.

pub mod smtp;
pub mod templates;

#[cfg(test)]
mod tests {
    #[test]
    fn crate_loads() {
        assert!(!env!("CARGO_PKG_NAME").is_empty());
    }
}
