//! Body limit (P0-065): JSON bodies are capped, uploads are not.
//!
//! [`MAX_JSON_BODY_BYTES`] is 1 MiB. The largest JSON bodies in
//! the api spec are job-create (hints), auth, and billing — all
//! KB-scale — so 1 MiB is generous for any legitimate request and
//! still bounds the memory a single request can force the api to
//! hold. The limit is a const, not env-configurable: no ticket
//! asks to tune it per env, and a knob nobody asked for is a knob
//! that gets mis-set.
//!
//! Uploads are exempt structurally, not by size: upload bytes
//! never transit the api at all — the client PUTs parts straight
//! to presigned S3 URLs (P0-055). The only upload request the api
//! sees is the small create/complete/abort JSON, and the uploads
//! nest carries no limit layer (see `routes::api_router`), so a
//! future streaming upload endpoint cannot be capped by accident.
//! The exemption is proven by the order test in
//! `tests/middleware_chain.rs` (a 2 MiB POST to `/v1/uploads`
//! is not 413).

use tower_http::limit::RequestBodyLimitLayer;

/// Maximum request body for non-upload routes: 1 MiB.
pub const MAX_JSON_BODY_BYTES: usize = 1_048_576;

/// The limit layer, applied per-module in `routes::api_router`
/// (every module except uploads).
pub fn body_limit_layer() -> RequestBodyLimitLayer {
    RequestBodyLimitLayer::new(MAX_JSON_BODY_BYTES)
}
