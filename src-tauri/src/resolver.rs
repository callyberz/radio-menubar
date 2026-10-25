//! Resolves a fresh, playable HLS master playlist URL for a station.
//! See `config.rs` for the flow.

use crate::config::{self, Station};
use crate::diag;
use crate::net::{self, Tag};
use base64::Engine;
use reqwest::blocking::Client;
use reqwest::cookie::Jar;
use std::sync::Arc;
use url::Url;

/// A resolved stream: a client whose cookie jar holds the CloudFront signed cookies,
/// plus the master playlist URL those cookies unlock.
pub struct Session {
    pub client: Client,
    pub jar: Arc<Jar>,
    pub master: Url,
}

/// Log/network source tag for this provider (source-agnostic logging layer).
pub fn source() -> String {
    format!("provider:{}", config::PROVIDER_ID)
}

pub fn resolve(station: Station) -> Result<Session, String> {
    let src = source();
    diag::debug(&src, format!("resolving stream for {}", station.code));
    let result = resolve_inner(station, &src);
    match &result {
        Ok(s) => diag::info(&src, format!("resolved {}: {}", station.code, s.master)),
        Err(e) => diag::warn(&src, format!("resolve {} failed: {e}", station.code)),
    }
    result
}

fn resolve_inner(station: Station, src: &str) -> Result<Session, String> {
    let jar = Arc::new(Jar::default());
    let client = net::client(Some(jar.clone())).map_err(|e| e.to_string())?;
    let tag = Tag { source: src, kind: "api", station: Some(station.code) };
    let referer = [("Referer", config::REFERER)];

    // 1. tokenised playlist.js URL
    let api = config::LIVE_SRC_API.replace("{channel}", &station.channel.to_string());
    let res = net::get(&client, Some(&jar), &api, &referer, tag)?;
    let json: serde_json::Value = serde_json::from_slice(&res.body).map_err(|e| format!("live src api: {e}"))?;
    let playlist_js = json
        .pointer("/response/livePlaylistUrl")
        .and_then(|v| v.as_str())
        .ok_or("live src api: no livePlaylistUrl")?;

    // 2. playlist.js -> (redirect) cfplaylist.js; sets the signed cookies in the jar
    let res = net::get(&client, Some(&jar), playlist_js, &referer, Tag { kind: "script", ..tag })?;
    diag::debug(src, format!("playlist script from {} ({} bytes)", res.final_url, res.body.len()));
    let script = String::from_utf8_lossy(&res.body);

    let master = decode_playlist_script(&script)?;
    let master = Url::parse(&master).map_err(|e| format!("bad master url {master:?}: {e}"))?;
    if master.host_str() != Some(config::STREAM_HOST) {
        return Err(format!("unexpected stream host in {master}"));
    }
    Ok(Session { client, jar, master })
}

/// `eval(atob("..." + "\x41..." + ...))` -> `var pluXYZ=function(){return atob2(atob("..."));}`
/// -> apply the atob/atob2 chain to the innermost literal.
pub fn decode_playlist_script(script: &str) -> Result<String, String> {
    let outer = js_string_literals(script).concat();
    if outer.is_empty() {
        return Err("playlist script: no string literals".into());
    }
    let inner = latin1(&b64(&outer)?);

    let body = inner
        .split_once("return")
        .map(|(_, b)| b)
        .ok_or("playlist script: unexpected format (no return)")?;
    // Collect the call chain: e.g. ["atob2", "atob", "atob"] then the literal.
    let mut chain = Vec::new();
    let mut rest = body.trim_start();
    loop {
        if let Some(r) = rest.strip_prefix("atob2(") {
            chain.push(2u8);
            rest = r.trim_start();
        } else if let Some(r) = rest.strip_prefix("atob(") {
            chain.push(1u8);
            rest = r.trim_start();
        } else {
            break;
        }
    }
    let literal = js_string_literals(rest)
        .into_iter()
        .next()
        .ok_or("playlist script: unexpected format (no literal)")?;
    if chain.is_empty() {
        return Err("playlist script: unexpected format (no decoder chain)".into());
    }
    let mut value = literal;
    for f in chain.iter().rev() {
        value = match f {
            1 => latin1(&b64(&value)?),
            _ => latin1(&crockford_b32(&value)?),
        };
    }
    if !value.starts_with("https://") {
        return Err(format!("playlist script: decoded value is not a URL: {value:.60}"));
    }
    Ok(value)
}

fn latin1(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| b as char).collect()
}

fn b64(s: &str) -> Result<Vec<u8>, String> {
    let clean: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    base64::engine::general_purpose::STANDARD_PAD_INDIFFERENT
        .decode(clean.as_bytes())
        .map_err(|e| format!("base64: {e}"))
}

/// Crockford base32 (what the site's `atob2` implements).
fn crockford_b32(s: &str) -> Result<Vec<u8>, String> {
    const ALPHABET: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let mut out = Vec::with_capacity(s.len() * 5 / 8);
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in s.chars() {
        let c = match c.to_ascii_uppercase() {
            'O' => '0',
            'I' | 'L' => '1',
            '-' | '=' => continue,
            c => c,
        };
        let v = ALPHABET
            .iter()
            .position(|&a| a as char == c)
            .ok_or_else(|| format!("base32: bad char {c:?}"))? as u32;
        acc = ((acc << 5) | v) & 0xFFFF;
        bits += 5;
        if bits >= 8 {
            out.push((acc >> (bits - 8)) as u8);
            bits -= 8;
        }
    }
    Ok(out)
}

/// Extracts the values of all double- or single-quoted JS string literals, unescaped.
fn js_string_literals(src: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut chars = src.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '"' && c != '\'' {
            continue;
        }
        let quote = c;
        let mut s = String::new();
        while let Some(c) = chars.next() {
            match c {
                '\\' => match chars.next() {
                    Some('x') => {
                        let h: String = chars.by_ref().take(2).collect();
                        if let Some(ch) = u32::from_str_radix(&h, 16).ok().and_then(char::from_u32) {
                            s.push(ch);
                        }
                    }
                    Some('u') => {
                        let h: String = if chars.peek() == Some(&'{') {
                            chars.next();
                            chars.by_ref().take_while(|&c| c != '}').collect()
                        } else {
                            chars.by_ref().take(4).collect()
                        };
                        if let Some(ch) = u32::from_str_radix(&h, 16).ok().and_then(char::from_u32) {
                            s.push(ch);
                        }
                    }
                    Some('n') => s.push('\n'),
                    Some('r') => s.push('\r'),
                    Some('t') => s.push('\t'),
                    Some(other) => s.push(other),
                    None => break,
                },
                c if c == quote => break,
                c => s.push(c),
            }
        }
        out.push(s);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crockford_roundtrip() {
        // "https://" in Crockford base32
        assert_eq!(latin1(&crockford_b32("D1T78W3K78QJY").unwrap()), "https://");
    }

    #[test]
    fn decodes_chain() {
        let url = "https://live-s.881903.com/edge-aac/903hd/playlist.m3u8?r=a&ri=b";
        let e = |s: &str| base64::engine::general_purpose::STANDARD.encode(s);
        // function body: return atob(atob("<b64(b64(url))>"));
        let inner = format!("var pluX=function(){{return atob(atob(\"{}\"));}}", e(&e(url)));
        let enc = e(&inner);
        let (a, b) = enc.split_at(10);
        let script = format!("eval(atob(\"{a}\"+\n\"{}\"));", b.chars().map(|c| format!("\\x{:02x}", c as u32)).collect::<String>());
        assert_eq!(decode_playlist_script(&script).unwrap(), url);
    }
}

/// Live network test against the real site: `cargo test -- --ignored live_`
#[cfg(test)]
mod live_tests {
    #[test]
    #[ignore]
    fn live_resolve_and_fetch() {
        for st in crate::config::STATIONS {
            let s = super::resolve(st).expect("resolve");
            let r = s.client.get(s.master.clone()).header("Referer", crate::config::REFERER).send().unwrap();
            let status = r.status();
            let body = r.text().unwrap();
            println!("{} {} -> {status}\n{body}", st.code, s.master);
            assert!(status.is_success() && body.starts_with("#EXTM3U"));
        }
    }
}
