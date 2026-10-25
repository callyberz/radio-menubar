//! Every external URL and station constant lives here, so it is easy to change them
//! if the broadcaster changes its site.
//!
//! How a stream URL is obtained (same steps as the public web player at
//! https://www.881903.com/live/903, no login involved):
//!
//! 1. `GET {SITE}/api/live/src/{channel}` returns JSON with `response.livePlaylistUrl`,
//!    a short-lived tokenised URL to `playlist.881903.com/.../playlist.js`.
//! 2. That URL redirects to `live-s.881903.com/.../cfplaylist.js`, which sets three CloudFront
//!    signed cookies (valid ~24 h, scoped to `/edge-aac/{key}/`) and returns a small script
//!    whose function returns the HLS master playlist URL, wrapped in base64 / Crockford-base32
//!    layers. `resolver.rs` unwraps it.
//! 3. The master playlist (`/edge-aac/{key}/playlist.m3u8?r=..&ri=..`) and its segments
//!    (plain ADTS AAC, no encryption) need those cookies. The local proxy (`proxy.rs`)
//!    supplies them, so `<audio>` in the webview can play the stream.

/// Provider id used in log/network source tags (`provider:881903`).
pub const PROVIDER_ID: &str = "881903";
/// `{channel}` is replaced with the numeric channel id.
pub const LIVE_SRC_API: &str = "https://www.881903.com/api/live/src/{channel}";
/// Programme timetable for all channels (about 48 h ahead).
pub const SCHEDULE_API: &str = "https://www.881903.com/api/live/toolbar";
/// Host that serves the HLS playlists and segments. The proxy only forwards to this host.
pub const STREAM_HOST: &str = "live-s.881903.com";
pub const REFERER: &str = "https://www.881903.com/";
/// A normal desktop Safari user agent: we fetch exactly what a browser visitor gets.
pub const USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Safari/605.1.15";

#[derive(Debug, Clone, Copy)]
pub struct Station {
    /// Code used throughout the app and shown in the menu bar.
    pub code: &'static str,
    /// Channel id used by the site's API (`CR1_881: 1, CR2_903: 2`).
    pub channel: u32,
}

pub const STATIONS: [Station; 2] = [
    Station { code: "881", channel: 1 },
    Station { code: "903", channel: 2 },
];

pub fn station(code: &str) -> Option<Station> {
    STATIONS.iter().copied().find(|s| s.code == code)
}

pub fn station_by_channel(channel: &str) -> Option<Station> {
    STATIONS.iter().copied().find(|s| s.channel.to_string() == channel)
}
