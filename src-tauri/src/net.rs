//! Outbound HTTP with every request recorded in the Network log (`diag::record_net`).
//! Redirects are followed by hand so each hop is its own entry.

use crate::config;
use crate::diag::{self, NetEntry};
use reqwest::blocking::{Client, Response};
use reqwest::cookie::{CookieStore, Jar};
use reqwest::header::HeaderMap;
use std::io::Read;
use std::sync::Arc;
use std::time::{Duration, Instant};
use url::Url;

const MAX_REDIRECTS: usize = 5;

/// Labels for a recorded request.
#[derive(Clone, Copy)]
pub struct Tag<'a> {
    pub source: &'a str,
    pub kind: &'a str,
    pub station: Option<&'a str>,
}

pub fn client(jar: Option<Arc<Jar>>) -> reqwest::Result<Client> {
    let mut b = Client::builder()
        .user_agent(config::USER_AGENT)
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(8))
        .timeout(Duration::from_secs(20));
    if let Some(jar) = jar {
        b = b.cookie_provider(jar);
    }
    b.build()
}

pub fn header_pairs(h: &HeaderMap) -> Vec<(String, String)> {
    h.iter()
        .map(|(k, v)| (k.as_str().to_string(), String::from_utf8_lossy(v.as_bytes()).into_owned()))
        .collect()
}

fn is_text(ct: &str) -> bool {
    let ct = ct.to_ascii_lowercase();
    ct.starts_with("text/") || ["json", "javascript", "mpegurl", "xml", "x-www-form-urlencoded"].iter().any(|t| ct.contains(t))
}

pub fn preview(ct: Option<&str>, body: &[u8]) -> Option<String> {
    if !ct.is_some_and(is_text) {
        return None;
    }
    let cut = body.len().min(diag::BODY_PREVIEW_BYTES);
    let mut s = String::from_utf8_lossy(&body[..cut]).into_owned();
    if body.len() > cut {
        s.push_str("\n… (truncated)");
    }
    Some(s)
}

/// The request headers we send, including the User-Agent and the cookies the jar will add.
pub fn request_headers(req: &reqwest::blocking::Request, jar: Option<&Jar>) -> Vec<(String, String)> {
    let mut h = header_pairs(req.headers());
    if !h.iter().any(|(k, _)| k.eq_ignore_ascii_case("user-agent")) {
        h.insert(0, ("user-agent".into(), config::USER_AGENT.into()));
    }
    if let Some(cookie) = jar.and_then(|j| j.cookies(req.url())) {
        h.push(("cookie".into(), String::from_utf8_lossy(cookie.as_bytes()).into_owned()));
    }
    h
}

pub struct Fetched {
    pub body: Vec<u8>,
    pub final_url: Url,
}

/// GETs `url` (reading the whole body), following redirects manually and recording every hop.
pub fn get(client: &Client, jar: Option<&Jar>, url: &str, headers: &[(&str, &str)], tag: Tag) -> Result<Fetched, String> {
    let mut url = Url::parse(url).map_err(|e| format!("bad url: {e}"))?;
    for _hop in 0..=MAX_REDIRECTS {
        let mut rb = client.get(url.clone());
        for (k, v) in headers {
            rb = rb.header(*k, *v);
        }
        let req = rb.build().map_err(|e| e.to_string())?;
        let mut entry = NetEntry {
            ts: diag::now_ms(),
            source: tag.source.into(),
            kind: tag.kind.into(),
            station: tag.station.map(Into::into),
            method: "GET".into(),
            url: url.to_string(),
            req_headers: request_headers(&req, jar),
            ..Default::default()
        };
        let started = Instant::now();
        let resp = match client.execute(req) {
            Ok(r) => r,
            Err(e) => {
                entry.duration_ms = started.elapsed().as_millis() as u64;
                entry.error = Some(e.to_string());
                diag::record_net(entry);
                return Err(format!("{}: {e}", tag.kind));
            }
        };
        let status = resp.status().as_u16();
        let resp_headers = resp.headers().clone();
        let location = resp_headers.get("location").and_then(|v| v.to_str().ok()).map(str::to_string);
        let ct = resp_headers.get("content-type").and_then(|v| v.to_str().ok()).map(str::to_string);
        let body = read_all(resp);
        entry.duration_ms = started.elapsed().as_millis() as u64;
        entry.status = Some(status);
        entry.content_type = ct.clone();
        entry.resp_headers = header_pairs(&resp_headers);
        match body {
            Ok(body) => {
                entry.size = Some(body.len() as u64);
                entry.body = preview(ct.as_deref(), &body);
                if (300..400).contains(&status)
                    && let Some(loc) = location {
                        entry.kind = "redirect".into();
                        diag::record_net(entry);
                        url = url.join(&loc).map_err(|e| format!("bad redirect: {e}"))?;
                        continue;
                    }
                diag::record_net(entry);
                if !(200..300).contains(&status) {
                    return Err(format!("{} HTTP {status}", tag.kind));
                }
                return Ok(Fetched { body, final_url: url });
            }
            Err(e) => {
                entry.error = Some(e.clone());
                diag::record_net(entry);
                return Err(e);
            }
        }
    }
    Err("too many redirects".into())
}

fn read_all(mut resp: Response) -> Result<Vec<u8>, String> {
    let mut buf = Vec::new();
    resp.read_to_end(&mut buf).map_err(|e| format!("reading body: {e}"))?;
    Ok(buf)
}

/// Wraps a streamed body (proxy segments): counts bytes and records the entry when the
/// transfer finishes (or the reader is dropped early).
pub struct RecordingReader<R: Read> {
    inner: R,
    entry: Option<NetEntry>,
    started: Instant,
    bytes: u64,
}

impl<R: Read> RecordingReader<R> {
    pub fn new(inner: R, entry: NetEntry, started: Instant) -> Self {
        Self { inner, entry: Some(entry), started, bytes: 0 }
    }
    fn finish(&mut self, error: Option<String>) {
        if let Some(mut e) = self.entry.take() {
            e.duration_ms = self.started.elapsed().as_millis() as u64;
            e.size = Some(self.bytes);
            if error.is_some() {
                e.error = error;
            }
            diag::record_net(e);
        }
    }
}

impl<R: Read> Read for RecordingReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self.inner.read(buf) {
            Ok(0) => {
                self.finish(None);
                Ok(0)
            }
            Ok(n) => {
                self.bytes += n as u64;
                Ok(n)
            }
            Err(e) => {
                self.finish(Some(e.to_string()));
                Err(e)
            }
        }
    }
}

impl<R: Read> Drop for RecordingReader<R> {
    fn drop(&mut self) {
        if self.entry.is_some() {
            let expected = self.entry.as_ref().and_then(|e| e.size);
            let err = match expected {
                Some(n) if n > self.bytes => Some(format!("client closed after {} of {n} bytes", self.bytes)),
                _ => None,
            };
            self.finish(err);
        }
    }
}
