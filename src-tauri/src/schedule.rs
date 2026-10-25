//! Programme timetable from the broadcaster's public API (the same data the website's
//! live player shows). Fetched in Rust: no CORS problems, and one place to change.

use crate::config;
use serde::Serialize;
use std::collections::HashMap;

#[derive(Debug, Serialize, Clone)]
pub struct Programme {
    pub name: String,
    pub hosts: String,
    /// Unix seconds
    pub start: i64,
    pub end: i64,
}

/// Returns `{ "881": [...], "903": [...] }`, sorted by start time.
pub fn fetch() -> Result<HashMap<String, Vec<Programme>>, String> {
    let client = crate::net::client(None).map_err(|e| e.to_string())?;
    let tag = crate::net::Tag { source: "schedule", kind: "api", station: None };
    let res = crate::net::get(&client, None, config::SCHEDULE_API, &[("Referer", config::REFERER)], tag)?;
    let json: serde_json::Value = serde_json::from_slice(&res.body).map_err(|e| format!("schedule: {e}"))?;
    let lists = json
        .pointer("/response/channelTimetableList")
        .and_then(|v| v.as_object())
        .ok_or("schedule: unexpected response")?;

    let mut out = HashMap::new();
    for (channel, entries) in lists {
        let Some(station) = config::station_by_channel(channel) else { continue };
        let mut progs: Vec<Programme> = entries
            .as_array()
            .map(|a| a.as_slice())
            .unwrap_or_default()
            .iter()
            .filter_map(|e| {
                Some(Programme {
                    name: e.get("program_name")?.as_str()?.trim().to_string(),
                    hosts: e.get("host_name").and_then(|h| h.as_str()).unwrap_or("").to_string(),
                    start: e.get("start_ts")?.as_i64()?,
                    end: e.get("end_ts")?.as_i64()?,
                })
            })
            .filter(|p| !p.name.is_empty() && p.end > p.start)
            .collect();
        progs.sort_by_key(|p| p.start);
        out.insert(station.code.to_string(), progs);
    }
    if out.is_empty() {
        return Err("schedule: no stations in response".into());
    }
    crate::diag::debug(
        "schedule",
        format!("timetable loaded: {}", out.iter().map(|(k, v)| format!("{k}: {} entries", v.len())).collect::<Vec<_>>().join(", ")),
    );
    Ok(out)
}
