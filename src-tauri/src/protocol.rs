//! `maps://` (http://maps.localhost/ on Windows): serves built maps from the maps folder.
//! The map page itself comes from the copy embedded in the program, so old maps always get the newest page.
use crate::live::Live;
use crate::viewer;
use std::path::{Component, Path, PathBuf};
use tauri::http::Response;

fn content_type(p: &Path) -> &'static str {
    match p.extension().and_then(|e| e.to_str()).map(|e| e.to_lowercase()).as_deref() {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") => "application/json",
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("svg") => "image/svg+xml",
        _ => "application/octet-stream",
    }
}

fn reply(status: u16, ctype: &str, body: Vec<u8>) -> Response<Vec<u8>> {
    Response::builder()
        .status(status)
        .header("Content-Type", ctype)
        .header("Cache-Control", "no-cache")
        .header("Access-Control-Allow-Origin", "*")
        .body(body)
        .unwrap()
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Some(v) = std::str::from_utf8(&b[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub fn respond(maps_dir: &Path, live: &Live, uri_path: &str) -> Response<Vec<u8>> {
    let decoded = percent_decode(uri_path);
    let mut parts = decoded.trim_start_matches('/').splitn(2, '/');
    let slug = parts.next().unwrap_or("");
    let rest = parts.next().unwrap_or("");
    if slug.is_empty() || slug.contains("..") || slug.contains('\\') {
        return reply(404, "text/plain", b"Not found".to_vec());
    }
    let rest = if rest.is_empty() { "index.html" } else { rest };
    // Never leave the map folder.
    let rel = Path::new(rest);
    if rel.components().any(|c| !matches!(c, Component::Normal(_))) {
        return reply(403, "text/plain", b"Forbidden".to_vec());
    }
    // Live players and redrawn tiles for this map, from companion plugins (live.rs). Not a file on disk.
    if rest == "live.json" {
        return reply(200, "application/json", live.map_json(slug).to_string().into_bytes());
    }
    if let Some(bytes) = viewer::get(rest) {
        return reply(200, content_type(rel), bytes.to_vec());
    }
    let file: PathBuf = maps_dir.join(slug).join(rel);
    match std::fs::read(&file) {
        Ok(data) => reply(200, content_type(&file), data),
        Err(_) => reply(404, "text/plain", b"Not found".to_vec()),
    }
}
