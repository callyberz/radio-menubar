//! A tiny loopback HTTP proxy for the HLS stream.
//!
//! The broadcaster protects its HLS files with CloudFront *signed cookies* (HttpOnly, on
//! `live-s.881903.com`). The webview's `<audio>` element (AVFoundation) won't send cookies
//! that belong to a different, third-party site, so the app keeps them in Rust
//! and serves the playlists and segments to `<audio>` from `http://127.0.0.1:<port>/live/<code>/...`.
//!
//! Routes (anything else is a 404):
//!   GET /live/{code}/playlist.m3u8   -> the session's master playlist
//!   GET /live/{code}/{relative path}  -> same directory on the stream host
//! Relative URIs inside the playlists resolve against these paths by themselves; absolute
//! URIs on the stream host are rewritten to the proxy.

use crate::config;
use crate::diag::{self, NetEntry};
use crate::net;
use std::time::Instant;
use crate::resolver::{self, Session};
use std::collections::HashMap;
use std::io::Read;
use std::sync::{Arc, Mutex};
use tiny_http::{Header, Method, Request, Response, Server, StatusCode};
use url::Url;

pub struct StreamProxy {
    port: u16,
    sessions: Mutex<HashMap<String, Arc<Session>>>,
    generation: Mutex<u64>,
}

impl StreamProxy {
    pub fn start() -> Result<Arc<Self>, String> {
        let server = Server::http("127.0.0.1:0").map_err(|e| e.to_string())?;
        let port = server
            .server_addr()
            .to_ip()
            .map(|a| a.port())
            .ok_or("proxy: no ip address")?;
        let proxy = Arc::new(Self { port, sessions: Mutex::new(HashMap::new()), generation: Mutex::new(0) });
        let p = proxy.clone();
        std::thread::Builder::new()
            .name("stream-proxy".into())
            .spawn(move || {
                for req in server.incoming_requests() {
                    let p = p.clone();
                    std::thread::spawn(move || p.handle(req));
                }
            })
            .map_err(|e| e.to_string())?;
        diag::info("proxy", format!("stream proxy listening on 127.0.0.1:{port}"));
        Ok(proxy)
    }

    /// Resolves a *fresh* session (new tokens and cookies) and returns the local URL for `<audio>`.
    /// Called every time playback starts, so "live" always means the live edge.
    pub fn prepare(&self, code: &str) -> Result<String, String> {
        let station = config::station(code).ok_or_else(|| format!("unknown station {code}"))?;
        if std::env::var("RADIO_DEBUG_FAIL_STREAM").is_ok_and(|v| v == "1") {
            return Err("simulated failure (RADIO_DEBUG_FAIL_STREAM=1)".into());
        }
        let session = resolver::resolve(station)?;
        self.sessions.lock().unwrap().insert(code.to_string(), Arc::new(session));
        let mut g = self.generation.lock().unwrap();
        *g += 1;
        Ok(format!("http://127.0.0.1:{}/live/{code}/playlist.m3u8?s={}", self.port, *g))
    }

    fn session(&self, code: &str, refresh: bool) -> Result<Arc<Session>, String> {
        if !refresh
            && let Some(s) = self.sessions.lock().unwrap().get(code) {
                return Ok(s.clone());
            }
        let station = config::station(code).ok_or_else(|| format!("unknown station {code}"))?;
        let s = Arc::new(resolver::resolve(station)?);
        self.sessions.lock().unwrap().insert(code.to_string(), s.clone());
        Ok(s)
    }

    fn handle(&self, req: Request) {
        let result = self.route(&req);
        let response = match result {
            Ok(r) => r,
            Err((status, msg)) => {
                diag::warn("proxy", format!("{} -> {status}: {msg}", req.url()));
                Response::new(StatusCode(status), vec![], Box::new(std::io::Cursor::new(msg.into_bytes())) as Box<dyn Read + Send>, None, None)
            }
        };
        let _ = req.respond(response);
    }

    fn route(&self, req: &Request) -> Result<Response<Box<dyn Read + Send>>, (u16, String)> {
        if !matches!(req.method(), Method::Get | Method::Head) {
            return Err((405, "method not allowed".into()));
        }
        let local = Url::parse(&format!("http://127.0.0.1{}", req.url())).map_err(|e| (400, e.to_string()))?;
        let mut segs = local.path_segments().ok_or((404, "not found".into()))?;
        if segs.next() != Some("live") {
            return Err((404, "not found".into()));
        }
        let code = segs.next().ok_or((404, "not found".into()))?.to_string();
        config::station(&code).ok_or((404, "unknown station".into()))?;
        let rest: Vec<&str> = segs.collect();
        if rest.is_empty() || rest.iter().any(|s| s.is_empty() || *s == "..") {
            return Err((404, "not found".into()));
        }
        let is_master = rest == ["playlist.m3u8"];
        let range = req
            .headers()
            .iter()
            .find(|h| h.field.equiv("Range"))
            .map(|h| h.value.as_str().to_string());

        // Try with the current session; on 401/403 (expired cookies) re-resolve once.
        let mut refresh = false;
        loop {
            let session = self.session(&code, refresh).map_err(|e| (502, e))?;
            let upstream = if is_master {
                session.master.clone()
            } else {
                let mut u = session.master.join(&rest.join("/")).map_err(|e| (400, e.to_string()))?;
                u.set_query(local.query());
                u
            };
            if upstream.host_str() != Some(config::STREAM_HOST) {
                return Err((403, "refusing to proxy foreign host".into()));
            }
            let mut rb = session.client.get(upstream.clone()).header("Referer", config::REFERER);
            if let Some(r) = &range {
                rb = rb.header("Range", r);
            }
            let up_req = rb.build().map_err(|e| (502, e.to_string()))?;
            let mut entry = NetEntry {
                ts: diag::now_ms(),
                source: "proxy".into(),
                kind: if upstream.path().ends_with(".m3u8") { "playlist" } else { "segment" }.into(),
                station: Some(code.clone()),
                method: "GET".into(),
                url: upstream.to_string(),
                req_headers: net::request_headers(&up_req, Some(&session.jar)),
                ..Default::default()
            };
            let started = Instant::now();
            let resp = match session.client.execute(up_req) {
                Ok(r) => r,
                Err(e) => {
                    entry.duration_ms = started.elapsed().as_millis() as u64;
                    entry.error = Some(e.to_string());
                    diag::record_net(entry);
                    return Err((502, format!("upstream: {e}")));
                }
            };
            let status = resp.status().as_u16();
            entry.status = Some(status);
            entry.resp_headers = net::header_pairs(resp.headers());
            entry.content_type = resp.headers().get("content-type").and_then(|v| v.to_str().ok()).map(str::to_string);
            if !(200..300).contains(&status) {
                entry.duration_ms = started.elapsed().as_millis() as u64;
                diag::record_net(entry);
                if (status == 401 || status == 403) && !refresh {
                    diag::warn("proxy", format!("upstream {status} for {code}, refreshing session"));
                    refresh = true;
                    continue;
                }
                return Err((status.max(400), format!("upstream status {status}")));
            }
            let ctype = entry.content_type.clone().unwrap_or_else(|| "application/octet-stream".into());
            let mut headers = vec![
                hdr("Content-Type", &ctype),
                hdr("Cache-Control", "no-cache"),
                hdr("Access-Control-Allow-Origin", "*"),
            ];
            let is_playlist = ctype.contains("mpegurl") || upstream.path().ends_with(".m3u8");
            if is_playlist {
                let text = resp.text().map_err(|e| (502, e.to_string()))?;
                entry.duration_ms = started.elapsed().as_millis() as u64;
                entry.size = Some(text.len() as u64);
                entry.body = net::preview(Some(&ctype), text.as_bytes());
                diag::record_net(entry);
                let body = rewrite_playlist(&text, &upstream, &session.master, self.port, &code).into_bytes();
                let len = body.len();
                return Ok(Response::new(StatusCode(status), headers, Box::new(std::io::Cursor::new(body)), Some(len), None));
            }
            for name in ["Content-Range", "Accept-Ranges"] {
                if let Some(v) = resp.headers().get(name).and_then(|v| v.to_str().ok()) {
                    headers.push(hdr(name, v));
                }
            }
            let len = resp.content_length().map(|l| l as usize);
            entry.size = resp.content_length();
            let body = net::RecordingReader::new(resp, entry, started);
            return Ok(Response::new(StatusCode(status), headers, Box::new(body), len, None));
        }
    }
}

fn hdr(k: &str, v: &str) -> Header {
    Header::from_bytes(k.as_bytes(), v.as_bytes()).expect("valid header")
}

/// Rewrites absolute URIs that point into the stream directory so they go through the proxy.
/// Relative URIs are left alone: they already resolve against the proxy path.
fn rewrite_playlist(text: &str, playlist_url: &Url, master: &Url, port: u16, code: &str) -> String {
    let base_dir = master.join("./").map(|u| u.to_string()).unwrap_or_default();
    let local_dir = format!("http://127.0.0.1:{port}/live/{code}/");
    let map_uri = |uri: &str| -> String {
        match playlist_url.join(uri) {
            Ok(abs) if uri.contains("://") || uri.starts_with('/') => {
                let s = abs.to_string();
                match s.strip_prefix(&base_dir) {
                    Some(rel) => format!("{local_dir}{rel}"),
                    None => s,
                }
            }
            _ => uri.to_string(),
        }
    };
    let mut out = String::with_capacity(text.len() + 64);
    for line in text.lines() {
        let t = line.trim();
        if t.is_empty() {
            out.push_str(line);
        } else if t.starts_with('#') {
            // URI="..." attributes (EXT-X-MEDIA, EXT-X-KEY, EXT-X-MAP, ...)
            if let Some(start) = t.find("URI=\"") {
                let s = start + 5;
                if let Some(end) = t[s..].find('"') {
                    out.push_str(&t[..s]);
                    out.push_str(&map_uri(&t[s..s + end]));
                    out.push_str(&t[s + end..]);
                } else {
                    out.push_str(line);
                }
            } else {
                out.push_str(line);
            }
        } else {
            out.push_str(&map_uri(t));
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrites_absolute_keeps_relative() {
        let m = Url::parse("https://live-s.881903.com/edge-aac/903hd/playlist.m3u8?r=1").unwrap();
        let txt = "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1\nchunks.m3u8\nhttps://live-s.881903.com/edge-aac/903hd/a.aac\nhttps://other.example/x.aac\n";
        let out = rewrite_playlist(txt, &m, &m, 1234, "903");
        assert!(out.contains("\nchunks.m3u8\n"));
        assert!(out.contains("http://127.0.0.1:1234/live/903/a.aac"));
        assert!(out.contains("https://other.example/x.aac"));
    }
}
