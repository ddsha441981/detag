//! Body limit (P0-065, per-class caps P0-104): JSON bodies are
//! capped per route class, uploads are not.
//!
//! One cap for every JSON route was too blunt: a login body is
//! under 1 KiB, a job-create body with hints can be tens of KiB,
//! and a provider webhook payload can reach tens of KiB — so
//! each module carries the cap of its class ([`BodyLimitClass`]):
//!
//! | class | cap | modules | why this size |
//! | ----- | --- | ------- | ------------- |
//! | `Small` | 64 KiB | auth, meta | credentials + tiny DTOs; meta takes no body at all. 64 KiB is ~64x headroom over any legitimate auth body. |
//! | `Standard` | 1 MiB | jobs, admin | job-create hints arrays, plan/flag edits — KB-scale, 1 MiB generous (the P0-065 cap, unchanged). |
//! | `Webhook` | 256 KiB | billing | payment DTOs + provider webhook payloads (a Stripe/Razorpay event object can reach tens of KiB). |
//!
//! The caps are consts, not env-configurable: no ticket asks to
//! tune them per env, and a knob nobody asked for is a knob that
//! gets mis-set. An over-cap body is rejected by the limit layer
//! before the handler runs, and the error mapper renders it as
//! 413 PF-2003 (`PayloadTooLarge`).
//!
//! Uploads are exempt structurally, not by size: upload bytes
//! never transit the api at all — the client PUTs parts straight
//! to presigned S3 URLs (P0-055). The only upload request the api
//! sees is the small create/complete/abort JSON, and the uploads
//! nest carries no limit layer (see `routes::api_router`), so a
//! future streaming upload endpoint cannot be capped by accident.
//! The exemption is proven by the order test in
//! `tests/middleware_chain.rs` (a 2 MiB POST to `/v1/uploads`
//! is not 413). The per-class caps are proven by the same file:
//! a 65 KiB body is 413 on auth and admitted on jobs.

use tower_http::limit::RequestBodyLimitLayer;

/// A route class's body cap: small JSON (auth, meta), standard
/// JSON (jobs, admin), or webhook-sized JSON (billing).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyLimitClass {
    /// 64 KiB: credentials and tiny DTOs.
    Small,
    /// 1 MiB: job-create hints, plan/flag edits.
    Standard,
    /// 256 KiB: payment DTOs and provider webhook payloads.
    Webhook,
}

impl BodyLimitClass {
    /// The cap in bytes for this class.
    pub fn bytes(self) -> usize {
        match self {
            BodyLimitClass::Small => 64 * 1024,
            BodyLimitClass::Standard => MAX_JSON_BODY_BYTES,
            BodyLimitClass::Webhook => 256 * 1024,
        }
    }
}

/// Maximum request body for standard JSON routes: 1 MiB.
pub const MAX_JSON_BODY_BYTES: usize = 1_048_576;

/// The limit layer for a route class, applied per-module in
/// `routes::api_router` (every module except uploads).
pub fn body_limit_layer_for(class: BodyLimitClass) -> RequestBodyLimitLayer {
    RequestBodyLimitLayer::new(class.bytes())
}

/// The limit layer for the standard class (the P0-065 default).
pub fn body_limit_layer() -> RequestBodyLimitLayer {
    body_limit_layer_for(BodyLimitClass::Standard)
}
