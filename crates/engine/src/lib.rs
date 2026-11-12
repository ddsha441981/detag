//! Detag engine: detection, mask ops, inpainting, encode, QC metrics.
//! Implemented in Phase 2; Phase 0 only scaffolds the module tree.

pub mod cli;
pub mod detect;
pub mod encode;
pub mod inpaint;
pub mod mask;
pub mod metrics;
pub mod registry;

/// Image formats the engine accepts in v1 (FR-D-07).
pub fn supported_image_formats() -> &'static [&'static str] {
    &["png", "jpeg", "webp"]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_formats_listed() {
        assert!(supported_image_formats().contains(&"png"));
    }
}
