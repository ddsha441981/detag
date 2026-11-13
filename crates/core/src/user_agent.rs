//! User-agent parsing for the session list (P1-019).
//!
//! [`parse_user_agent`] turns a raw `User-Agent` string into
//! a human-readable device label (`"Chrome on macOS"`), for
//! `GET /v1/me/sessions` — the user tells their sessions
//! apart by device, not by a raw UA string.
//!
//! It is a small heuristic table, not a UA database: the
//! browsers and operating systems real clients send, matched
//! by well-known tokens, with `"Unknown …"` fallbacks. A UA
//! that matches nothing is `"Unknown device"` — the raw
//! string is never echoed back (it is untrusted input, and
//! there is nothing to gain by showing it). The order
//! matters: Edge and Opera both contain `"Chrome"`, so they
//! are checked first; Safari contains `"Version/"` and
//! `"Safari/"` but no `"Chrome"`; a mobile token (Android /
//! iPhone / iPad) wins the OS over the desktop token in the
//! same string.

/// Parse a raw `User-Agent` into `"Browser on OS"`.
/// Unknown browser → `"Unknown browser"`; unknown OS →
/// `"Unknown OS"`; both unknown → `"Unknown device"`.
pub fn parse_user_agent(ua: &str) -> String {
    let browser = parse_browser(ua);
    let os = parse_os(ua);
    match (browser, os) {
        (None, None) => "Unknown device".to_string(),
        (Some(browser), None) => format!("{browser} on Unknown OS"),
        (None, Some(os)) => format!("Unknown browser on {os}"),
        (Some(browser), Some(os)) => format!("{browser} on {os}"),
    }
}

/// The browser family, or `None` when no known token matches.
/// Edge and Opera carry a Chrome token too, so they are
/// checked before Chrome; Safari is `Version/… Safari/…`
/// without Chrome.
fn parse_browser(ua: &str) -> Option<&'static str> {
    if ua.contains("Edg/") {
        Some("Edge")
    } else if ua.contains("OPR/") {
        Some("Opera")
    } else if ua.contains("Firefox/") {
        Some("Firefox")
    } else if ua.contains("Chrome/") {
        Some("Chrome")
    } else if ua.contains("Safari/") && ua.contains("Version/") {
        Some("Safari")
    } else {
        None
    }
}

/// The operating system, or `None` when no known token
/// matches. A mobile token wins over a desktop token in the
/// same string (an Android UA contains `"Linux"`; an iPhone
/// UA contains `"Mac OS X"`).
fn parse_os(ua: &str) -> Option<&'static str> {
    if ua.contains("Android") {
        Some("Android")
    } else if ua.contains("iPhone") || ua.contains("iPad") {
        Some("iOS")
    } else if ua.contains("Windows NT") {
        Some("Windows")
    } else if ua.contains("Mac OS X") || ua.contains("Macintosh") {
        Some("macOS")
    } else if ua.contains("Linux") {
        Some("Linux")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chrome_on_macos() {
        assert_eq!(
            parse_user_agent(
                "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) \
                 AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36"
            ),
            "Chrome on macOS"
        );
    }

    #[test]
    fn edge_beats_chrome() {
        assert_eq!(
            parse_user_agent(
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) \
                 AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36 Edg/120.0.0.0"
            ),
            "Edge on Windows"
        );
    }

    #[test]
    fn firefox_on_linux() {
        assert_eq!(
            parse_user_agent(
                "Mozilla/5.0 (X11; Linux x86_64; rv:121.0) Gecko/20100101 Firefox/121.0"
            ),
            "Firefox on Linux"
        );
    }

    #[test]
    fn safari_on_ios() {
        assert_eq!(
            parse_user_agent(
                "Mozilla/5.0 (iPhone; CPU iPhone OS 17_2 like Mac OS X) \
                 AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.2 Mobile/15E148 Safari/604.1"
            ),
            "Safari on iOS"
        );
    }

    #[test]
    fn android_beats_linux() {
        assert_eq!(
            parse_user_agent(
                "Mozilla/5.0 (Linux; Android 14; Pixel 8) \
                 AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Mobile Safari/537.36"
            ),
            "Chrome on Android"
        );
    }

    #[test]
    fn unknown_is_honest() {
        assert_eq!(parse_user_agent("curl/8.5.0"), "Unknown device");
        assert_eq!(
            parse_user_agent("Mozilla/5.0 (compatible; SomeBot/1.0)"),
            "Unknown device"
        );
        assert_eq!(parse_user_agent(""), "Unknown device");
        // A known browser on an unknown OS still names the browser.
        assert_eq!(
            parse_user_agent("Mozilla/5.0 (FreeBSD 13.0) Chrome/120.0.0.0"),
            "Chrome on Unknown OS"
        );
    }
}
