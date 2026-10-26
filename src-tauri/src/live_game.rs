//! The in-game pack (pack/ in the source): a behavior pack for ordinary and co-op worlds. Such worlds cannot reach
//! the network from scripts, so the pack writes live link messages to Minecraft's content log, and this module
//! reads them as the log grows and hands them to live.rs. It also builds the .mcpack (with the app's own list of
//! see-through blocks filled in), and checks and turns on the game's "Content Log File" setting.
use crate::color_tables::{DYES, EXACT, LEAVES, WOODS};
use crate::colors::{SKIP_EXACT, SKIP_PATTERN};
use crate::live::Live;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const MANIFEST: &str = include_str!("../../pack/behavior/manifest.json");
const SCRIPT: &str = include_str!("../../pack/behavior/scripts/main.js");
const RP_MANIFEST: &str = include_str!("../../pack/resource/manifest.json");
const RP_HUD: &str = include_str!("../../pack/resource/ui/hud_screen.json");
const RP_ITEM_TEXTURES: &str = include_str!("../../pack/resource/textures/item_texture.json");
const BP_ITEM: &str = include_str!("../../pack/behavior/items/map.json");
const BP_RECIPE: &str = include_str!("../../pack/behavior/recipes/map.json");
const ICON: &[u8] = include_bytes!("../icons/128x128.png");
/// Every pack line starts with this, followed by one JSON message.
const TAG: &str = "BMLIVE1 ";
/// Behavior pack (scripts) and resource pack (minimap HUD, map glyphs) in one file.
pub const PACK_FILE: &str = "Bedrock Map Live.mcaddon";

/// What the Live link tab shows about the in-game pack.
#[derive(Default, Clone)]
pub struct GameState {
    pub log_dirs: Vec<String>,
    pub watching: Option<String>,
    pub last_line: Option<u64>,
    pub lines: u64,
    pub bad_lines: u64,
    pub minecraft_running: bool,
}

fn env_dir(var: &str) -> Option<PathBuf> {
    std::env::var_os(var).map(PathBuf::from)
}

/// Folders the game writes its content log to: current (GDK) and Preview builds, and the older Microsoft Store build.
pub fn log_dirs() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(a) = env_dir("APPDATA") {
        v.push(a.join("Minecraft Bedrock").join("logs"));
        v.push(a.join("Minecraft Bedrock Preview").join("logs"));
    }
    if let Some(l) = env_dir("LOCALAPPDATA") {
        v.push(l.join("Packages").join("Microsoft.MinecraftUWP_8wekyb3d8bbwe").join("LocalState").join("logs"));
    }
    v
}

/// Every options.txt (one per signed-in player, plus the older Store build's).
pub fn options_files() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(a) = env_dir("APPDATA") {
        for app in ["Minecraft Bedrock", "Minecraft Bedrock Preview"] {
            if let Ok(rd) = fs::read_dir(a.join(app).join("Users")) {
                for u in rd.flatten() {
                    let p = u.path().join("games").join("com.mojang").join("minecraftpe").join("options.txt");
                    if p.exists() {
                        v.push(p);
                    }
                }
            }
        }
    }
    if let Some(l) = env_dir("LOCALAPPDATA") {
        let p = l.join("Packages").join("Microsoft.MinecraftUWP_8wekyb3d8bbwe").join("LocalState").join("games").join("com.mojang").join("minecraftpe").join("options.txt");
        if p.exists() {
            v.push(p);
        }
    }
    v
}

/// Whether "Content Log File" is on: true if any player profile has it on, None when no settings were found.
pub fn content_log_on() -> Option<bool> {
    let files = options_files();
    if files.is_empty() {
        return None;
    }
    Some(files.iter().any(|f| fs::read_to_string(f).map(|s| s.lines().any(|l| l.trim() == "content_log_file:1")).unwrap_or(false)))
}

/// Turns "Content Log File" on in every profile. Minecraft must be closed, or it writes its own settings back.
pub fn enable_content_log() -> Result<usize, String> {
    if minecraft_running() {
        return Err("Close Minecraft first: it saves its settings when it closes and would undo the change.".into());
    }
    let files = options_files();
    if files.is_empty() {
        return Err("Could not find Minecraft's settings. Turn on Settings > Creator > Content Log File in the game.".into());
    }
    let mut changed = 0;
    for f in files {
        let s = fs::read_to_string(&f).map_err(|e| e.to_string())?;
        if s.lines().any(|l| l.trim() == "content_log_file:1") {
            continue;
        }
        let nl = if s.contains("\r\n") { "\r\n" } else { "\n" };
        let mut found = false;
        let mut lines: Vec<String> = s
            .lines()
            .map(|l| {
                if l.trim_start().starts_with("content_log_file:") {
                    found = true;
                    "content_log_file:1".to_string()
                } else {
                    l.to_string()
                }
            })
            .collect();
        if !found {
            lines.push("content_log_file:1".into());
        }
        fs::write(&f, lines.join(nl) + nl).map_err(|e| e.to_string())?;
        changed += 1;
    }
    Ok(changed)
}

#[cfg(windows)]
fn minecraft_running() -> bool {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    ["Minecraft.Windows.exe", "Minecraft.Win10.DX11.exe"].iter().any(|exe| {
        std::process::Command::new("tasklist")
            .args(["/FI", &format!("IMAGENAME eq {}", exe), "/NH", "/FO", "CSV"])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).to_lowercase().contains(&exe.to_lowercase()))
            .unwrap_or(false)
    })
}
#[cfg(not(windows))]
fn minecraft_running() -> bool {
    false
}

/// Replaces the text between `start` and the next `/*BM:END*/` in the script.
fn fill(script: &str, start: &str, with: &str) -> String {
    match script.find(start) {
        Some(a) => {
            let from = a + start.len();
            match script[from..].find("/*BM:END*/") {
                Some(b) => format!("{}{}{}", &script[..from], with, &script[from + b..]),
                None => script.to_string(),
            }
        }
        None => script.to_string(),
    }
}

fn table_json(t: &[(&str, [u8; 3])]) -> String {
    let m: serde_json::Map<String, Value> = t.iter().map(|(k, v)| (k.to_string(), json!(v))).collect();
    Value::Object(m).to_string()
}

/// The pack's script with the app's see-through blocks and colour tables filled in.
pub fn pack_script() -> String {
    let mut s = fill(SCRIPT, "/*BM:SKIP*/", &serde_json::to_string(SKIP_EXACT).unwrap_or_else(|_| "[]".into()));
    s = fill(&s, "/*BM:SKIPRE*/", &serde_json::to_string(SKIP_PATTERN).unwrap_or_else(|_| "''".into()));
    for (marker, table) in [("/*BM:EXACT*/", EXACT), ("/*BM:LEAVES*/", LEAVES), ("/*BM:DYES*/", DYES), ("/*BM:WOODS*/", WOODS)] {
        s = fill(&s, marker, &table_json(table));
    }
    s
}

/// Number of map font pages (U+E200 .. U+F1FF).
pub const GLYPH_PAGES: usize = 16;

/// The colour level (0..15) of each row and column of a font page. Neighbouring cells, including the first and last
/// (the texture wraps around), differ by at most two levels: when the game shrinks the font texture it blends a
/// cell with its neighbours, and with plain 0..15 order a dark blue cell next to the page edge picked up bright
/// blue (and so on), which showed up as specks. The script uses the inverse of this order (`POS` in main.js).
pub const GLYPH_ORDER: [usize; 16] = [0, 2, 4, 6, 8, 10, 12, 14, 15, 13, 11, 9, 7, 5, 3, 1];

/// One of the map's font pages, U+E200 + 256 * page onwards (see the pack's main.js): 16 x 16 cells of 16 px, a
/// 16-level colour cube. Page r holds red level r; the cell in row i, column j is green GLYPH_ORDER[i] and blue
/// GLYPH_ORDER[j]. Every cell is fully opaque, so all characters are equally wide and the map's columns line up.
pub fn glyph_page(page: usize) -> Vec<u8> {
    let mut px = vec![0u8; 256 * 256 * 4];
    for cell in 0..256 {
        let (row, col) = (cell / 16, cell % 16);
        let c = [(page * 17) as u8, (GLYPH_ORDER[row] * 17) as u8, (GLYPH_ORDER[col] * 17) as u8, 255];
        for y in 0..16 {
            for x in 0..16 {
                let o = ((row * 16 + y) * 256 + col * 16 + x) * 4;
                px[o..o + 4].copy_from_slice(&c);
            }
        }
    }
    png_bytes(256, 256, &px)
}

/// The "you are here" arrow drawn over the middle of the maps, pointing north (0), north-east (1), ... clockwise:
/// 32 x 32, yellow with a black outline, transparent around it.
pub fn arrow_texture(d: usize) -> Vec<u8> {
    let a = d as f64 * std::f64::consts::FRAC_PI_4;
    let shape = [(0.0, -13.0), (11.0, 11.0), (0.0, 5.0), (-11.0, 11.0)];
    let pts: Vec<(f64, f64)> = shape.iter().map(|&(x, y)| (x * a.cos() - y * a.sin(), x * a.sin() + y * a.cos())).collect();
    let inside = |x: f64, y: f64, grow: f64| {
        // point in polygon, with the polygon scaled up by `grow` for the outline
        let mut c = false;
        for k in 0..pts.len() {
            let (xi, yi) = (pts[k].0 * grow, pts[k].1 * grow);
            let (xj, yj) = (pts[(k + 3) % 4].0 * grow, pts[(k + 3) % 4].1 * grow);
            if (yi > y) != (yj > y) && x < (xj - xi) * (y - yi) / (yj - yi) + xi {
                c = !c;
            }
        }
        c
    };
    let mut px = vec![0u8; 32 * 32 * 4];
    for y in 0..32usize {
        for x in 0..32usize {
            let (fx, fy) = (x as f64 - 15.5, y as f64 - 15.5);
            let c = if inside(fx, fy, 1.0) {
                [255, 214, 64, 255]
            } else if inside(fx, fy, 1.22) {
                [0, 0, 0, 255]
            } else {
                [0, 0, 0, 0]
            };
            px[(y * 32 + x) * 4..(y * 32 + x) * 4 + 4].copy_from_slice(&c);
        }
    }
    png_bytes(32, 32, &px)
}

/// The Map item's 16 x 16 icon: a folded paper map with land, water and a red marker.
pub fn map_icon() -> Vec<u8> {
    let mut px = vec![0u8; 16 * 16 * 4];
    for y in 0..16usize {
        for x in 0..16usize {
            let o = (y * 16 + x) * 4;
            let edge = x == 1 || x == 14 || y == 2 || y == 13;
            let inside = (1..=14).contains(&x) && (2..=13).contains(&y);
            let c: [u8; 4] = if !inside {
                [0, 0, 0, 0]
            } else if edge {
                [92, 64, 38, 255]
            } else if (x as i32 - 10).pow(2) + (y as i32 - 6).pow(2) <= 2 {
                [200, 40, 40, 255]
            } else if x + y / 2 < 9 || (x > 9 && y > 9) {
                [70, 110, 200, 255]
            } else if x == 5 || x == 10 {
                [196, 176, 128, 255] // folds
            } else {
                [110, 160, 70, 255]
            };
            px[o..o + 4].copy_from_slice(&c);
        }
    }
    png_bytes(16, 16, &px)
}

fn png_bytes(w: u32, h: u32, rgba: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut wr = enc.write_header().expect("png header");
        wr.write_image_data(rgba).expect("png data");
    }
    out
}

/// The .mcaddon file: the behavior pack (scripts) and the resource pack (minimap HUD and map glyphs), each in its
/// own folder. The behavior pack depends on the resource pack, so activating it on a world adds both.
pub fn build_pack() -> Result<Vec<u8>, String> {
    use zip::write::SimpleFileOptions;
    let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let opts = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    let mut files: Vec<(String, Vec<u8>)> = vec![
        ("Bedrock Map Live/manifest.json".into(), MANIFEST.as_bytes().to_vec()),
        ("Bedrock Map Live/scripts/main.js".into(), pack_script().into_bytes()),
        ("Bedrock Map Live/pack_icon.png".into(), ICON.to_vec()),
        ("Bedrock Map Live/items/map.json".into(), BP_ITEM.as_bytes().to_vec()),
        ("Bedrock Map Live/recipes/map.json".into(), BP_RECIPE.as_bytes().to_vec()),
        ("Bedrock Map Live RP/manifest.json".into(), RP_MANIFEST.as_bytes().to_vec()),
        ("Bedrock Map Live RP/ui/hud_screen.json".into(), RP_HUD.as_bytes().to_vec()),
        ("Bedrock Map Live RP/textures/item_texture.json".into(), RP_ITEM_TEXTURES.as_bytes().to_vec()),
        ("Bedrock Map Live RP/textures/items/bmlive_map.png".into(), map_icon()),
        ("Bedrock Map Live RP/pack_icon.png".into(), ICON.to_vec()),
    ];
    for page in 0..GLYPH_PAGES {
        files.push((format!("Bedrock Map Live RP/font/glyph_{:X}.png", 0xE2 + page), glyph_page(page)));
    }
    for d in 0..8 {
        files.push((format!("Bedrock Map Live RP/textures/bmlive/arrow_{}.png", d), arrow_texture(d)));
    }
    for (name, bytes) in files {
        w.start_file(name, opts).map_err(|e| e.to_string())?;
        w.write_all(&bytes).map_err(|e| e.to_string())?;
    }
    Ok(w.finish().map_err(|e| e.to_string())?.into_inner())
}

/// Writes the .mcpack into the app's data folder and returns its path.
pub fn write_pack(data_dir: &Path) -> Result<PathBuf, String> {
    let p = data_dir.join(PACK_FILE);
    fs::create_dir_all(data_dir).map_err(|e| e.to_string())?;
    fs::write(&p, build_pack()?).map_err(|e| e.to_string())?;
    Ok(p)
}

/// Opens a file with its default program (for a .mcpack: Minecraft imports it), or shows it in the file manager.
pub fn open_path(p: &Path, reveal: bool) -> Result<(), String> {
    let r = if cfg!(windows) {
        let mut c = std::process::Command::new("explorer.exe");
        if reveal {
            c.arg(format!("/select,{}", p.display()));
        } else {
            c.arg(p);
        }
        c.spawn()
    } else if cfg!(target_os = "macos") {
        let mut c = std::process::Command::new("open");
        if reveal {
            c.arg("-R");
        }
        c.arg(p).spawn()
    } else {
        std::process::Command::new("xdg-open").arg(if reveal { p.parent().unwrap_or(p) } else { p }).spawn()
    };
    r.map(|_| ()).map_err(|e| format!("Could not open {}: {}", p.display(), e))
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
}

/// The newest log file in the log folders.
fn newest_log(dirs: &[PathBuf]) -> Option<(PathBuf, SystemTime)> {
    let mut best: Option<(PathBuf, SystemTime)> = None;
    for d in dirs {
        let Ok(rd) = fs::read_dir(d) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            let ext = p.extension().and_then(|x| x.to_str()).map(|x| x.to_lowercase()).unwrap_or_default();
            if ext != "log" && ext != "txt" {
                continue;
            }
            let Ok(m) = e.metadata().and_then(|m| m.modified()) else { continue };
            if best.as_ref().map_or(true, |(_, t)| m > *t) {
                best = Some((p, m));
            }
        }
    }
    best
}

/// Follows the newest content log like `tail -f` and passes pack lines to the live link.
pub fn start(live: Arc<Live>, state: Arc<Mutex<GameState>>) {
    let dirs = log_dirs();
    // Logs written before the app started are old news: start reading those at their current end.
    let mut seen: HashMap<PathBuf, u64> = HashMap::new();
    for d in &dirs {
        if let Ok(rd) = fs::read_dir(d) {
            for e in rd.flatten() {
                seen.insert(e.path(), e.metadata().map(|m| m.len()).unwrap_or(0));
            }
        }
    }
    state.lock().unwrap().log_dirs = dirs.iter().map(|d| d.display().to_string()).collect();
    std::thread::Builder::new()
        .name("live-game-log".into())
        .spawn(move || {
            let mut cur: Option<PathBuf> = None;
            let (mut pos, mut rest, mut utf16, mut tick) = (0u64, Vec::<u8>::new(), false, 0u32);
            loop {
                std::thread::sleep(Duration::from_millis(500));
                tick += 1;
                if tick % 10 == 1 {
                    let running = minecraft_running();
                    state.lock().unwrap().minecraft_running = running;
                }
                let Some((path, _)) = newest_log(&dirs) else { continue };
                if cur.as_ref() != Some(&path) {
                    pos = seen.get(&path).copied().unwrap_or(0);
                    rest.clear();
                    utf16 = false;
                    state.lock().unwrap().watching = Some(path.display().to_string());
                    cur = Some(path.clone());
                }
                let Ok(mut f) = File::open(&path) else { continue };
                let len = f.metadata().map(|m| m.len()).unwrap_or(0);
                if len < pos {
                    pos = 0; // the game started the file over
                    rest.clear();
                }
                if len == pos {
                    continue;
                }
                if pos == 0 {
                    let mut bom = [0u8; 2];
                    utf16 = f.read_exact(&mut bom).is_ok() && bom == [0xff, 0xfe];
                }
                let take = (len - pos).min(16 << 20);
                let mut buf = vec![0u8; take as usize];
                if f.seek(SeekFrom::Start(pos)).is_err() || f.read_exact(&mut buf).is_err() {
                    continue;
                }
                pos += take;
                rest.extend_from_slice(&buf);
                let text = if utf16 {
                    // keep an odd trailing byte for the next read
                    let even = rest.len() & !1;
                    let units: Vec<u16> = rest[..even].chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
                    let s = String::from_utf16_lossy(&units);
                    let cut = s.rfind('\n').map(|i| i + 1).unwrap_or(0);
                    let done: String = s[..cut].to_string();
                    let keep_units = s[cut..].encode_utf16().count();
                    let mut left: Vec<u8> = rest[even - keep_units * 2..].to_vec();
                    std::mem::swap(&mut rest, &mut left);
                    done
                } else {
                    let cut = rest.iter().rposition(|b| *b == b'\n').map(|i| i + 1).unwrap_or(0);
                    let done = String::from_utf8_lossy(&rest[..cut]).into_owned();
                    rest.drain(..cut);
                    done
                };
                for line in text.lines() {
                    let Some(i) = line.find(TAG) else { continue };
                    let body = line[i + TAG.len()..].trim();
                    let mut st = state.lock().unwrap();
                    match serde_json::from_str::<Value>(body) {
                        Ok(v) => {
                            st.lines += 1;
                            st.last_line = Some(now_ms());
                            drop(st);
                            live.game_line(&v);
                        }
                        Err(_) => st.bad_lines += 1,
                    }
                }
            }
        })
        .ok();
}

/// For the Live link tab.
pub fn state_json(state: &GameState, data_dir: &Path) -> Value {
    json!({
        "logDirs": state.log_dirs,
        "watching": state.watching,
        "lastLine": state.last_line,
        "lines": state.lines,
        "badLines": state.bad_lines,
        "contentLog": content_log_on(),
        "minecraftRunning": state.minecraft_running,
        "packFile": data_dir.join(PACK_FILE).display().to_string()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pack_script_gets_the_full_skip_list() {
        let s = pack_script();
        assert!(s.contains("\"lily_of_the_valley\""));
        assert!(s.contains("_pressure_plate"));
        assert!(!s.contains("'air', 'cave_air'"), "the short default list is replaced");
    }

    #[test]
    fn addon_has_both_packs_and_the_behavior_pack_needs_the_resource_pack() {
        let bytes = build_pack().unwrap();
        let mut z = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        let read = |z: &mut zip::ZipArchive<std::io::Cursor<Vec<u8>>>, n: &str| {
            let mut s = String::new();
            z.by_name(n).unwrap().read_to_string(&mut s).unwrap();
            s
        };
        let bp: Value = serde_json::from_str(&read(&mut z, "Bedrock Map Live/manifest.json")).unwrap();
        let rp: Value = serde_json::from_str(&read(&mut z, "Bedrock Map Live RP/manifest.json")).unwrap();
        assert_eq!(bp["modules"][0]["entry"], "scripts/main.js");
        assert_eq!(rp["modules"][0]["type"], "resources");
        let deps = bp["dependencies"].as_array().unwrap();
        assert!(deps.iter().any(|d| d["uuid"] == rp["header"]["uuid"] && d["version"] == rp["header"]["version"]));
        for f in [
            "Bedrock Map Live/scripts/main.js",
            "Bedrock Map Live/items/map.json",
            "Bedrock Map Live/recipes/map.json",
            "Bedrock Map Live RP/ui/hud_screen.json",
            "Bedrock Map Live RP/textures/item_texture.json",
            "Bedrock Map Live RP/textures/items/bmlive_map.png",
            "Bedrock Map Live RP/font/glyph_E2.png",
            "Bedrock Map Live RP/font/glyph_F1.png",
            "Bedrock Map Live RP/textures/bmlive/arrow_0.png",
            "Bedrock Map Live RP/textures/bmlive/arrow_7.png",
        ] {
            assert!(z.by_name(f).is_ok(), "{} missing", f);
        }
        // the item and its icon are wired up: item icon name -> item_texture.json entry
        let item: Value = serde_json::from_str(&read(&mut z, "Bedrock Map Live/items/map.json")).unwrap();
        let tex: Value = serde_json::from_str(&read(&mut z, "Bedrock Map Live RP/textures/item_texture.json")).unwrap();
        let icon = item["minecraft:item"]["components"]["minecraft:icon"].as_str().unwrap();
        assert_eq!(tex["texture_data"][icon]["textures"], "textures/items/bmlive_map");
    }

    #[test]
    fn glyph_pages_match_the_scripts_colour_code() {
        // colour (r, g, b), 0..15 each, is character U+E200 + r*256 + pos(g)*16 + pos(b), pos = GLYPH_ORDER inverted
        let decode = |png: Vec<u8>| -> Vec<u8> {
            let mut r = png::Decoder::new(std::io::Cursor::new(png)).read_info().unwrap();
            let mut buf = vec![0u8; r.output_buffer_size()];
            r.next_frame(&mut buf).unwrap();
            buf
        };
        let pages: Vec<Vec<u8>> = (0..GLYPH_PAGES).map(|p| decode(glyph_page(p))).collect();
        let pos = |v: usize| GLYPH_ORDER.iter().position(|&o| o == v).unwrap();
        let at = |page: usize, row: usize, col: usize, dx: usize, dy: usize| -> [u8; 4] {
            let o = ((row * 16 + dy) * 256 + col * 16 + dx) * 4;
            let px = &pages[page];
            [px[o], px[o + 1], px[o + 2], px[o + 3]]
        };
        assert_eq!(at(0, pos(0), pos(0), 8, 8), [0, 0, 0, 255]);
        assert_eq!(at(15, pos(7), pos(2), 8, 8), [255, 119, 34, 255]);
        assert_eq!(at(15, pos(15), pos(15), 8, 8), [255, 255, 255, 255]);
        // every colour appears once, and every cell is fully opaque (equal widths, so the columns line up)
        let mut seen = std::collections::HashSet::new();
        for page in 0..GLYPH_PAGES {
            for row in 0..16 {
                for col in 0..16 {
                    for (dx, dy) in [(0, 0), (15, 0), (0, 15), (15, 15)] {
                        assert_eq!(at(page, row, col, dx, dy)[3], 255, "page {} cell {},{} corner not opaque", page, row, col);
                    }
                    seen.insert(at(page, row, col, 8, 8));
                }
            }
        }
        assert_eq!(seen.len(), 4096);
        // neighbouring cells, also across the page edge, are close colours
        for i in 0..16 {
            let (a, b) = (GLYPH_ORDER[i] as i32, GLYPH_ORDER[(i + 1) % 16] as i32);
            assert!((a - b).abs() <= 2, "levels {} and {} side by side", a, b);
        }
        // the script uses the same order
        let order = format!("{:?}", GLYPH_ORDER);
        assert!(pack_script().contains(&format!("const ORDER = {};", order)), "main.js ORDER differs from {}", order);
    }

    #[test]
    fn arrows_point_the_right_way() {
        let decode = |png: Vec<u8>| -> Vec<u8> {
            let mut r = png::Decoder::new(std::io::Cursor::new(png)).read_info().unwrap();
            let mut buf = vec![0u8; r.output_buffer_size()];
            r.next_frame(&mut buf).unwrap();
            buf
        };
        let yellow = |px: &[u8], x: usize, y: usize| px[(y * 32 + x) * 4] == 255 && px[(y * 32 + x) * 4 + 3] == 255;
        let north = decode(arrow_texture(0));
        assert!(yellow(&north, 16, 6) && !yellow(&north, 16, 27), "north arrow tip at the top");
        let east = decode(arrow_texture(2));
        assert!(yellow(&east, 25, 16) && !yellow(&east, 4, 16), "east arrow tip on the right");
        assert_eq!(north[3], 0, "transparent corner");
    }


    #[test]
    fn pack_script_gets_the_colour_tables() {
        let s = pack_script();
        assert!(s.contains("\"birch\""), "leaves/woods tables filled in");
        assert!(!s.contains("grass_block: [95, 159, 53]"), "the short default table is replaced");
    }

    /// Writes the packaged script to $DUMP_PACK_SCRIPT (for running it outside the game) and/or the .mcpack to
    /// $DUMP_PACK: `cargo test dump_pack -- --ignored`.
    #[test]
    #[ignore]
    fn dump_pack() {
        if let Ok(p) = std::env::var("DUMP_PACK_SCRIPT") {
            fs::write(p, pack_script()).unwrap();
        }
        if let Ok(p) = std::env::var("DUMP_PACK") {
            fs::write(p, build_pack().unwrap()).unwrap();
        }
    }

    #[test]
    fn fill_replaces_between_markers() {
        assert_eq!(fill("a(/*X*/old/*BM:END*/)b", "/*X*/", "new"), "a(/*X*/new/*BM:END*/)b");
        assert_eq!(fill("nothing here", "/*X*/", "new"), "nothing here");
    }
}
