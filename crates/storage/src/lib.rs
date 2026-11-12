//! Detag storage: StorageProvider trait + S3/MinIO implementation (E0.6).

pub mod provider;

#[cfg(test)]
mod tests {
    #[test]
    fn crate_loads() {
        assert!(!env!("CARGO_PKG_NAME").is_empty());
    }
}
