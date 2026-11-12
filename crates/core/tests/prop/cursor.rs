//! Cursor codec properties (P0-096): `pagination::Cursor`
//! encode/decode roundtrip over arbitrary timestamps and ids, and
//! strict rejection of truncated/garbage input.

use detag_core::pagination::Cursor;
use proptest::prelude::*;
use uuid::Uuid;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1000))]

    /// Encode then decode returns the same cursor, for any timestamp
    /// and any id — the pagination contract the list endpoints rely on.
    #[test]
    fn cursor_roundtrips(ts in any::<i64>(), id in any::<u128>()) {
        let cursor = Cursor { ts, id: Uuid::from_u128(id) };
        let decoded = Cursor::decode(&cursor.encode())
            .expect("a cursor we just encoded must decode");
        prop_assert_eq!(decoded, cursor);
    }

    /// Truncating an encoding breaks it (strict decode: a cursor
    /// that does not decode is a client error, never a shifted page).
    #[test]
    fn cursor_truncated_fails_to_decode(ts in any::<i64>(), id in any::<u128>()) {
        let cursor = Cursor { ts, id: Uuid::from_u128(id) };
        let encoded = cursor.encode();
        prop_assume!(!encoded.is_empty());
        let truncated = &encoded[..encoded.len() - 1];
        prop_assert!(Cursor::decode(truncated).is_err(), "truncated: {}", truncated);
    }

    /// Garbage never decodes to a cursor. (A valid encoding is
    /// base64url of `{"ts":...,"id":"..."}` JSON — at least ~60
    /// chars — so no string in this strategy can be one by accident.)
    #[test]
    fn cursor_garbage_fails_to_decode(s in "[a-zA-Z0-9]{1,32}") {
        prop_assert!(Cursor::decode(&s).is_err(), "garbage decoded: {}", s);
    }
}
