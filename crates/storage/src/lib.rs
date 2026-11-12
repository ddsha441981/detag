//! Detag storage: StorageProvider trait + S3/MinIO implementation (E0.6).

pub mod health;
pub mod keys;
pub mod memory;
pub mod policy;
pub mod provider;
pub mod s3;

#[cfg(test)]
mod tests {
    #[test]
    fn crate_loads() {
        assert!(!env!("CARGO_PKG_NAME").is_empty());
    }
}
