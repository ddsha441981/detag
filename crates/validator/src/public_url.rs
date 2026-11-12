//! URL validation (P0-101).
//!
//! Webhook URLs (and any future fetch-a-URL input) must point at the
//! public internet — a `http://169.254.169.254/` webhook is an SSRF
//! hole, not a feature. Codes: `required` (empty), `invalid_url`
//! (unparsable — including an empty host, which the parser rejects),
//! `invalid_scheme` (not http/https), `private_url` (localhost, a
//! non-public IP literal, or a single-label hostname).
//!
//! Limitation, stated plainly: DNS names are NOT resolved here — a
//! hostname that resolves to a private IP passes this check. The
//! fetch layer must re-validate the resolved address before
//! connecting (SSRF defense-in-depth, Phase 3). What this rule
//! guarantees is that the *literal* URL is public.

use detag_core::error::FieldError;
use std::net::IpAddr;
use url::Url;

/// Validate a public URL for the named field. Returns the parsed URL
/// on success (callers use the parsed form, not the raw string).
pub fn validate_public_url(field: &str, value: &str) -> Result<Url, FieldError> {
    if value.trim().is_empty() {
        return Err(FieldError::new(field, "required", "is required"));
    }
    let url = Url::parse(value.trim())
        .map_err(|_| FieldError::new(field, "invalid_url", "must be a valid URL"))?;
    if url.scheme() != "http" && url.scheme() != "https" {
        return Err(FieldError::new(
            field,
            "invalid_scheme",
            "must use http or https",
        ));
    }
    let host = match url.host() {
        // `host_str()` keeps IPv6 brackets ("[::1]"), which do not
        // parse as an IpAddr — match on the parsed host instead.
        Some(url::Host::Ipv4(v4)) => {
            if !is_public_ip(&IpAddr::V4(v4)) {
                return Err(FieldError::new(
                    field,
                    "private_url",
                    "must be a public URL",
                ));
            }
            return Ok(url);
        }
        Some(url::Host::Ipv6(v6)) => {
            if !is_public_ip(&IpAddr::V6(v6)) {
                return Err(FieldError::new(
                    field,
                    "private_url",
                    "must be a public URL",
                ));
            }
            return Ok(url);
        }
        Some(url::Host::Domain(domain)) => domain.to_string(),
        // Unreachable for http(s): the parser rejects an empty host
        // ("empty host" parse error), so a parsed URL always has one.
        // Kept for exhaustiveness.
        None => {
            return Err(FieldError::new(field, "invalid_url", "must be a valid URL"));
        }
    };
    if host.is_empty() {
        return Err(FieldError::new(field, "invalid_url", "must be a valid URL"));
    }
    if is_localhost(&host) || !host.contains('.') {
        // Localhost, *.localhost, and single-label hostnames
        // (intranet names, not public DNS) are not public.
        return Err(FieldError::new(
            field,
            "private_url",
            "must be a public URL",
        ));
    }
    Ok(url)
}

fn is_localhost(host: &str) -> bool {
    let lower = host.to_ascii_lowercase();
    lower == "localhost" || lower.ends_with(".localhost")
}

/// True only for globally routable addresses. Everything else —
/// loopback, private, link-local, CGNAT, unspecified, reserved,
/// multicast, documentation, unique-local — is non-public.
fn is_public_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            !(v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_multicast()
                || v4.is_unspecified()
                // Explicit ranges (std's is_reserved / is_benchmarking /
                // is_shared are unstable on this toolchain):
                // 240.0.0.0/4 reserved, 198.18.0.0/15 benchmarking,
                // 100.64.0.0/10 shared (CGNAT).
                || v4.octets()[0] >= 240
                || (v4.octets()[0] == 198 && (18..=19).contains(&v4.octets()[1]))
                || (v4.octets()[0] == 100 && (64..=127).contains(&v4.octets()[1]))
                // 192.0.0.0/24 (IETF protocol assignments) and the
                // documentation /24s: `is_reserved` covers 240/4
                // only, so check them explicitly.
                || (v4.octets()[0] == 192 && v4.octets()[1] == 0 && v4.octets()[2] == 0)
                || (v4.octets()[0] == 192 && v4.octets()[1] == 0 && v4.octets()[2] == 2)
                || (v4.octets()[0] == 198 && v4.octets()[1] == 51 && v4.octets()[2] == 100)
                || (v4.octets()[0] == 203 && v4.octets()[1] == 0 && v4.octets()[2] == 113))
        }
        IpAddr::V6(v6) => {
            let seg = v6.segments();
            !(v6.is_loopback()
                || v6.is_multicast()
                || v6.is_unspecified()
                // fe80::/10 link-local
                || (seg[0] & 0xffc0) == 0xfe80
                // fc00::/7 unique-local
                || (seg[0] & 0xfe00) == 0xfc00
                // 2001:db8::/32 documentation
                || (seg[0] == 0x2001 && seg[1] == 0x0db8)
                // ::ffff:0:0/96 (IPv4-mapped): judge by the v4 part.
                || (seg[0] == 0
                    && seg[1] == 0
                    && seg[2] == 0
                    && seg[3] == 0
                    && seg[4] == 0
                    && seg[5] == 0xffff
                    && !is_public_ip(&IpAddr::V4(
                        std::net::Ipv4Addr::new(
                            (seg[6] >> 8) as u8,
                            (seg[6] & 0xff) as u8,
                            (seg[7] >> 8) as u8,
                            (seg[7] & 0xff) as u8,
                        ),
                    ))))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_public_urls() {
        for good in [
            "https://example.com/hook",
            "http://api.example.co.uk/v1/cb?x=1",
            "https://8.8.8.8/hook",
            "https://[2606:4700:4700::1111]/hook",
        ] {
            assert!(validate_public_url("webhook_url", good).is_ok(), "{good}");
        }
    }

    #[test]
    fn rejects_private_and_local() {
        for bad in [
            "http://localhost/hook",
            "http://LOCALHOST:8080/hook",
            "http://foo.localhost/hook",
            "http://127.0.0.1/hook",
            "http://10.0.0.5/hook",
            "http://172.16.0.1/hook",
            "http://192.168.1.1/hook",
            "http://169.254.169.254/latest/meta-data/",
            "http://100.64.0.1/hook",
            "http://0.0.0.0/hook",
            "http://[::1]/hook",
            "http://[fe80::1]/hook",
            "http://[fc00::1]/hook",
            "http://[fd12:3456::1]/hook",
            "http://[2001:db8::1]/hook",
            "http://intranet/hook",
        ] {
            let err = validate_public_url("webhook_url", bad).expect_err(bad);
            assert_eq!(err.code, "private_url", "{bad}");
        }
    }

    #[test]
    fn rejects_bad_shapes() {
        let cases = [
            ("", "required"),
            ("not a url", "invalid_url"),
            // Empty host: the parser rejects it ("empty host"), so
            // the code is invalid_url, not a separate missing-host.
            ("https://", "invalid_url"),
            ("https://?q=1", "invalid_url"),
            ("ftp://example.com/file", "invalid_scheme"),
            // Three slashes: the url crate parses the segment as a
            // single-label host ("path-only") — rejected as private.
            ("https:///path-only", "private_url"),
        ];
        for (value, code) in cases {
            let err = validate_public_url("webhook_url", value).expect_err(value);
            assert_eq!(err.code, code, "{value}");
        }
    }
}
