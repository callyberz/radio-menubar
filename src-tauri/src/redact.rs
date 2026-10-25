//! Masks secrets (signed cookies, stream tokens, token-like query params, long opaque blobs)
//! in log messages, URLs, headers and body previews.
//!
//! Redaction is applied when *displaying* or *exporting* diagnostics (unless the user turns on
//! "Show sensitive values" for the session) and *always* when writing the on-disk log file.
//! The in-memory ring buffers keep the raw values.

use regex::{Captures, Regex};
use std::sync::OnceLock;

pub const MASK: &str = "•••";

/// Query/cookie parameter names whose values are always masked.
/// `r`/`ri` = stream session tokens, `n1`/`n2`/`rcft` = playlist.js tokens.
const SENSITIVE_PARAMS: &str = r"r|ri|n1|n2|rcft";
/// Any parameter whose name contains one of these is treated as token-like.
const TOKENISH_WORDS: &str = r"token|sig|auth|key|policy|secret|session|pass|credential|jwt|hdnts";

fn param_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(&format!(
            r#"(?i)(^|[?&;\s"'(,{{])((?:{SENSITIVE_PARAMS})|[a-z0-9_.~\-]*(?:{TOKENISH_WORDS})[a-z0-9_.~\-]*)=([^&\s"'<>;,)}}]+)"#
        ))
        .expect("valid regex")
    })
}

/// Long opaque runs (base64-ish, mixed case + digits), e.g. signatures and encoded tokens.
fn blob_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"[A-Za-z0-9+/_~\-%]{40,}={0,2}").expect("valid regex"))
}

fn looks_opaque(s: &str) -> bool {
    s.chars().any(|c| c.is_ascii_uppercase())
        && s.chars().any(|c| c.is_ascii_lowercase())
        && s.chars().any(|c| c.is_ascii_digit())
}

/// Masks secrets inside free text (log messages, URLs, bodies, header values).
pub fn text(s: &str) -> String {
    let s = param_re().replace_all(s, |c: &Captures| {
        if c[3].starts_with(MASK) {
            c[0].to_string()
        } else {
            format!("{}{}={MASK}", &c[1], &c[2])
        }
    });
    blob_re()
        .replace_all(&s, |c: &Captures| {
            let m = &c[0];
            // file paths and URL paths are runs of short segments; a token is one long segment
            if m.split('/').any(|seg| seg.len() >= 24 && looks_opaque(seg)) {
                format!("{}{MASK}", &m[..4])
            } else {
                m.to_string()
            }
        })
        .into_owned()
}

/// Masks a URL's sensitive query parameters, e.g. `playlist.m3u8?r=•••&ri=•••`.
pub fn url(u: &str) -> String {
    text(u)
}

/// Masks a header value according to the header name.
pub fn header(name: &str, value: &str) -> String {
    match name.to_ascii_lowercase().as_str() {
        // "a=1; b=2" -> "a=•••; b=•••"
        "cookie" => value
            .split(';')
            .map(|pair| match pair.split_once('=') {
                Some((k, _)) => format!("{}={MASK}", k.trim()),
                None => pair.trim().to_string(),
            })
            .collect::<Vec<_>>()
            .join("; "),
        // "name=value; Path=/; Secure" -> "name=•••; Path=/; Secure"
        "set-cookie" => {
            let mut parts = value.splitn(2, ';');
            let first = parts.next().unwrap_or("");
            let rest = parts.next().map(|r| format!(";{r}")).unwrap_or_default();
            match first.split_once('=') {
                Some((k, _)) => format!("{}={MASK}{rest}", k.trim()),
                None => format!("{MASK}{rest}"),
            }
        }
        "authorization" | "proxy-authorization" | "x-amz-security-token" => MASK.to_string(),
        _ => text(value),
    }
}

pub fn headers(h: &[(String, String)]) -> Vec<(String, String)> {
    h.iter().map(|(k, v)| (k.clone(), header(k, v))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_stream_tokens_in_urls() {
        let u = "https://live-s.881903.com/edge-aac/903hd/playlist.m3u8?r=lLSE79hekIAaydy3&ri=0461039757c718d294fb";
        assert_eq!(url(u), "https://live-s.881903.com/edge-aac/903hd/playlist.m3u8?r=•••&ri=•••");
        let p = "https://playlist.881903.com/web/v4/903hd/playlist.js?t=1792893116&n1=34fc36e383f8&n2=7PzRz9zc%3D";
        assert_eq!(url(p), "https://playlist.881903.com/web/v4/903hd/playlist.js?t=1792893116&n1=•••&n2=•••");
    }

    #[test]
    fn masks_token_like_params_but_keeps_harmless_ones() {
        let u = "https://x.example/a?access_token=abc123&X-Amz-Signature=deadbeef&page=2&s=7&for=me";
        assert_eq!(url(u), "https://x.example/a?access_token=•••&X-Amz-Signature=•••&page=2&s=7&for=me");
    }

    #[test]
    fn masks_cookie_headers() {
        assert_eq!(
            header("Cookie", "CloudFront-Policy=eyJTdGF0; CloudFront-Signature=qgrV~yw; CloudFront-Key-Pair-Id=APKAJD57"),
            "CloudFront-Policy=•••; CloudFront-Signature=•••; CloudFront-Key-Pair-Id=•••"
        );
        assert_eq!(
            header("set-cookie", "CloudFront-Key-Pair-Id=APKAJD57M4WB2ORCAM4Q; path=/edge-aac/903hd; secure; HttpOnly"),
            "CloudFront-Key-Pair-Id=•••; path=/edge-aac/903hd; secure; HttpOnly"
        );
        assert_eq!(header("Authorization", "Bearer x"), "•••");
        assert_eq!(header("Content-Type", "audio/aac"), "audio/aac");
    }

    #[test]
    fn masks_tokens_in_free_text_and_json() {
        let msg = "resolved 903: https://live-s.881903.com/edge-aac/903hd/playlist.m3u8?r=qIQSBGuZO371mnjQ&ri=284bb47592";
        let out = text(msg);
        assert!(!out.contains("qIQSBGuZO371mnjQ") && !out.contains("284bb47592"), "{out}");
        let json = r#"{"livePlaylistUrl":"https://p.example/playlist.js?t=1&n1=27f4f415&n2=iTfUoli%2FL8%3D"}"#;
        let out = text(json);
        assert!(out.contains("n1=•••") && out.contains("n2=•••") && !out.contains("27f4f415"), "{out}");
    }

    #[test]
    fn masks_long_opaque_blobs_only() {
        let blob = "dmFyIHBsdThPS2lDTjRJUHhUOU9NQ1BOVlN3PWZ1bmN0aW9uKCl7cmV0dXJu";
        assert_eq!(text(&format!("eval(atob(\"{blob}\"))")), "eval(atob(\"dmFy•••\"))");
        // lowercase hex paths (e.g. image URLs) are left alone
        let img = "https://stream.881903.com/public/02a8f4a500e5138a25b6658ac23d4c4d/2026/10/963850e777355e4b52b2f69b56e96ba0.jpg";
        assert_eq!(text(img), img);
    }

    #[test]
    fn leaves_file_paths_alone() {
        let p = "exported diagnostics to /private/tmp/claude-501/-Users-calvinlee-github/8aeb7202-c242-424b-a172-214f4aee3a81/scratchpad/export.txt";
        assert_eq!(text(p), p);
    }

    #[test]
    fn idempotent() {
        let once = text("a?r=abc&ri=def");
        assert_eq!(text(&once), once);
    }
}
