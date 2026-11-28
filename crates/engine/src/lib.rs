//! Detag engine: detection, mask ops, inpainting, encode, QC metrics.
//! Implemented in Phase 2; Phase 0 only scaffolds the module tree.

pub mod budget;
pub mod cli;
pub mod detect;
pub mod encode;
pub mod exif;
pub mod guards;
pub mod hash;
pub mod icc;
pub mod inpaint;
pub mod intake;
pub mod mask;
pub mod metrics;
pub mod orientation;
pub mod registry;

/// Image formats the engine accepts in v1 (FR-D-07).
/// Always: png, jpeg, webp. `tiff` joins when the `tiff` cargo
/// feature is on (default); `avif` joins when the `avif` cargo
/// feature is on (opt-in: needs system libdav1d — ADR-0071).
pub fn supported_image_formats() -> &'static [&'static str] {
    #[cfg(all(feature = "tiff", feature = "avif"))]
    const FORMATS: &[&str] = &["png", "jpeg", "webp", "tiff", "avif"];
    #[cfg(all(feature = "tiff", not(feature = "avif")))]
    const FORMATS: &[&str] = &["png", "jpeg", "webp", "tiff"];
    #[cfg(all(not(feature = "tiff"), feature = "avif"))]
    const FORMATS: &[&str] = &["png", "jpeg", "webp", "avif"];
    #[cfg(all(not(feature = "tiff"), not(feature = "avif")))]
    const FORMATS: &[&str] = &["png", "jpeg", "webp"];
    FORMATS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_formats_listed() {
        let formats = supported_image_formats();
        assert!(formats.contains(&"png"));
        assert!(formats.contains(&"jpeg"));
        assert!(formats.contains(&"webp"));
        #[cfg(feature = "tiff")]
        assert!(formats.contains(&"tiff"));
        #[cfg(not(feature = "tiff"))]
        assert!(!formats.contains(&"tiff"));
        #[cfg(feature = "avif")]
        assert!(formats.contains(&"avif"));
        #[cfg(not(feature = "avif"))]
        assert!(!formats.contains(&"avif"));
    }
}
