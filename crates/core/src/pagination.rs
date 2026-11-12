//! Cursor pagination helpers (P0-068, api-spec "Pagination").
//!
//! List endpoints take `?limit=&cursor=` and answer
//! `{"items": [...], "next_cursor": "..."}`. The cursor is an
//! opaque, base64url-encoded `{"ts": <unix millis>, "id": <uuid>}`
//! — a keyset position, not an offset: rows are ordered by
//! `(created_at, id)` and the next page starts strictly after the
//! cursor. A client must never construct one; a tampered cursor is
//! a 400 (`AppError::ValidationFailed`, PF-2002), never a 500 and
//! never a silent restart from the first page.
//!
//! Rules:
//! - `limit` defaults to 20 and clamps to 1..=100
//!   ([`clamp_limit`]).
//! - The codec is strict: bad base64, bad JSON, a missing field,
//!   or an unparsable uuid all fail as
//!   [`PaginationError::InvalidCursor`].
//! - [`Page::from_window`] takes the `limit + 1` rows a list
//!   query fetches and produces the page + the next cursor (or
//!   none).
//! - [`next_link`] builds the RFC 8288 `Link` header value for
//!   the next page, preserving the request's other query params.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// The default page size when the client sends no `limit`.
pub const DEFAULT_LIMIT: u32 = 20;
/// The largest page a client may ask for.
pub const MAX_LIMIT: u32 = 100;

/// Clamp a client-supplied limit into 1..=[`MAX_LIMIT`]; `None`
/// is the default page size. A 0 clamps up to 1 — it is a limit,
/// not an error: there is no page of size 0 to return.
pub fn clamp_limit(limit: Option<u32>) -> u32 {
    limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT)
}

/// A keyset position: the `(created_at, id)` of the last row on a
/// page — `ts` is Unix milliseconds (not seconds: rows created in
/// the same millisecond still order by `id`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cursor {
    /// `created_at` of the last row, Unix milliseconds.
    pub ts: i64,
    /// `id` of the last row.
    pub id: Uuid,
}

impl Cursor {
    /// Encode to the opaque wire form: base64url (no padding) of
    /// the JSON `{"ts": ..., "id": "..."}`. The encoding is
    /// URL-safe, so the cursor travels in a query string
    /// unescaped.
    pub fn encode(&self) -> String {
        let json = serde_json::to_string(self).expect("Cursor is always serializable");
        URL_SAFE_NO_PAD.encode(json.as_bytes())
    }

    /// Decode the wire form. Strict: any failure — bad base64, bad
    /// JSON, a missing field, a bad uuid — is
    /// [`PaginationError::InvalidCursor`]. A tampered cursor never
    /// decodes to a different valid position: the JSON shape is
    /// fixed, so a flipped character breaks the parse (or the
    /// base64) instead of silently shifting the window.
    pub fn decode(s: &str) -> Result<Self, PaginationError> {
        let bytes = URL_SAFE_NO_PAD
            .decode(s)
            .map_err(|_| PaginationError::InvalidCursor)?;
        serde_json::from_slice(&bytes).map_err(|_| PaginationError::InvalidCursor)
    }
}

/// One page of a list response, shaped the way the api-spec shapes
/// it (`{"items": [...], "next_cursor": "..."}`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Page<T> {
    /// This page's rows (at most the requested limit).
    pub items: Vec<T>,
    /// The cursor for the next page, or `None` when this is the
    /// last page. Serializes as `next_cursor`, omitted when none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

impl<T> Page<T> {
    /// Build a page from the `limit + 1` rows a list query fetches
    /// (in `(created_at, id)` order). When more than `limit` rows
    /// came back there is a next page: rows past the limit are
    /// dropped and the cursor is the new last row's position.
    /// `cursor_of` reads a row's keyset position.
    pub fn from_window(mut items: Vec<T>, limit: u32, cursor_of: impl Fn(&T) -> Cursor) -> Self {
        let next_cursor = if items.len() > limit as usize {
            items.truncate(limit as usize);
            items.last().map(|last| cursor_of(last).encode())
        } else {
            None
        };
        Self { items, next_cursor }
    }
}

/// Pagination failures. Only one kind exists: the client sent a
/// cursor that does not decode. It maps to
/// `AppError::ValidationFailed` (PF-2002, 400) — a tampered cursor
/// is a client error, never a 500.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PaginationError {
    /// The cursor is not a base64url-encoded `{"ts","id"}` object.
    #[error("invalid pagination cursor")]
    InvalidCursor,
}

/// Build the RFC 8288 `Link` header value for the next page:
/// `<{uri with cursor set}>; rel="next"`. The request's other
/// query params (limit, filters) are preserved; an existing
/// `cursor` is replaced. `uri` is the request path + query
/// (`/v1/jobs?limit=20&status=done`); the result is a relative
/// reference, which is what `Link` carries for same-origin pages.
pub fn next_link(uri: &str, next_cursor: &str) -> String {
    // Parse against a dummy base so `url` can hold the query; only
    // the path + query are ever returned.
    let mut url = url::Url::parse(&format!("http://x{uri}")).expect("request URI parses");
    let kept: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(key, _)| key != "cursor")
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    url.query_pairs_mut()
        .clear()
        .extend_pairs(kept)
        .append_pair("cursor", next_cursor);
    let path_and_query = &url[url::Position::BeforePath..];
    format!("<{path_and_query}>; rel=\"next\"")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::AppError;

    fn cursor() -> Cursor {
        Cursor {
            ts: 1_791_234_567_890,
            id: Uuid::parse_str("019aa1b2-c3d4-7e5f-8000-000000000001").expect("fixture uuid"),
        }
    }

    #[test]
    fn round_trip_is_exact_and_url_safe() {
        let encoded = cursor().encode();
        assert_eq!(Cursor::decode(&encoded).expect("round trip"), cursor());
        // base64url, no padding: safe to paste into a query string.
        assert!(
            encoded
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
            "cursor must be url-safe: {encoded}"
        );
    }

    #[test]
    fn tampered_cursor_is_rejected() {
        let encoded = cursor().encode();
        // Flip one character at the start, middle, and end: every
        // single-character tamper must fail, not shift the window.
        for pos in [0, encoded.len() / 2, encoded.len() - 1] {
            let mut tampered: Vec<char> = encoded.chars().collect();
            tampered[pos] = if tampered[pos] == 'A' { 'B' } else { 'A' };
            let tampered: String = tampered.into_iter().collect();
            assert_eq!(
                Cursor::decode(&tampered),
                Err(PaginationError::InvalidCursor),
                "tampered cursor at {pos} must fail: {tampered}"
            );
        }
        // Truncated, garbage, valid-base64-but-not-JSON, JSON with
        // a missing field, JSON with a bad uuid — all invalid.
        for bad in [
            &encoded[..encoded.len() - 4],
            "!!!not-base64!!!",
            &URL_SAFE_NO_PAD.encode(b"hello, not json"),
            &URL_SAFE_NO_PAD.encode(br#"{"ts": 123}"#),
            &URL_SAFE_NO_PAD.encode(br#"{"ts": 123, "id": "not-a-uuid"}"#),
            "",
        ] {
            assert_eq!(
                Cursor::decode(bad),
                Err(PaginationError::InvalidCursor),
                "must reject: {bad:?}"
            );
        }
    }

    #[test]
    fn invalid_cursor_maps_to_400_validation_failed() {
        let err = AppError::from(PaginationError::InvalidCursor);
        assert_eq!(err, AppError::ValidationFailed);
        assert_eq!(err.code(), "PF-2002");
        assert_eq!(err.status(), 400);
    }

    #[test]
    fn limit_clamps_to_1_100_with_default_20() {
        assert_eq!(clamp_limit(None), 20);
        assert_eq!(clamp_limit(Some(0)), 1);
        assert_eq!(clamp_limit(Some(1)), 1);
        assert_eq!(clamp_limit(Some(50)), 50);
        assert_eq!(clamp_limit(Some(100)), 100);
        assert_eq!(clamp_limit(Some(101)), 100);
        assert_eq!(clamp_limit(Some(u32::MAX)), 100);
    }

    #[test]
    fn page_from_window_keeps_limit_and_cursors_the_last_row() {
        let rows: Vec<(i64, Uuid)> = (0..5)
            .map(|i| {
                (
                    1_791_234_567_000 + i,
                    Uuid::parse_str(&format!("019aa1b2-c3d4-7e5f-8000-00000000000{i}"))
                        .expect("fixture uuid"),
                )
            })
            .collect();
        let cursor_of = |(ts, id): &(i64, Uuid)| Cursor { ts: *ts, id: *id };

        // Exactly one page: no next cursor, all rows kept.
        let page = Page::from_window(rows[..3].to_vec(), 3, cursor_of);
        assert_eq!(page.items.len(), 3);
        assert_eq!(page.next_cursor, None);

        // One row over the limit: the extra row drops, the cursor
        // is the new last row's position.
        let page = Page::from_window(rows[..4].to_vec(), 3, cursor_of);
        assert_eq!(page.items.len(), 3);
        let next = page.next_cursor.expect("there is a next page");
        assert_eq!(
            Cursor::decode(&next).expect("next decodes"),
            cursor_of(&rows[2])
        );

        // Empty window: an empty page with no next cursor.
        let page: Page<(i64, Uuid)> = Page::from_window(vec![], 3, cursor_of);
        assert!(page.items.is_empty());
        assert_eq!(page.next_cursor, None);
    }

    #[test]
    fn page_serializes_to_the_spec_shape() {
        let page = Page {
            items: vec![1, 2],
            next_cursor: Some(cursor().encode()),
        };
        let json = serde_json::to_value(&page).expect("serializes");
        assert_eq!(json["items"], serde_json::json!([1, 2]));
        assert_eq!(json["next_cursor"], serde_json::json!(cursor().encode()));
        let last = Page::<i32> {
            items: vec![],
            next_cursor: None,
        };
        let json = serde_json::to_value(&last).expect("serializes");
        assert!(
            json.get("next_cursor").is_none(),
            "no next_cursor on the last page: {json}"
        );
    }

    #[test]
    fn next_link_preserves_params_and_replaces_cursor() {
        let link = next_link("/v1/jobs?limit=20&status=done&cursor=old", "newcursor");
        assert_eq!(
            link,
            "</v1/jobs?limit=20&status=done&cursor=newcursor>; rel=\"next\""
        );
        let link = next_link("/v1/jobs", "c");
        assert_eq!(link, "</v1/jobs?cursor=c>; rel=\"next\"");
    }
}
