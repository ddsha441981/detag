//! Print the generated OpenAPI document to stdout (P0-067).
//!
//! The single source is `detag_api::openapi::ApiDoc::openapi()` —
//! the same document the api serves at `/openapi.json`. Used by
//! `just openapi` (write `docs/api/openapi.json`) and
//! `just openapi-check` (fail on drift); never edit the committed
//! JSON by hand.

use detag_api::openapi::ApiDoc;
use utoipa::OpenApi;

fn main() {
    let json = ApiDoc::openapi()
        .to_json()
        .expect("the OpenAPI document must serialize");
    println!("{json}");
}
