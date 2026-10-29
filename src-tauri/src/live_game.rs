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
    let effects: Vec<Value> = EFFECTS.iter().map(|(id, name, _, harmful)| json!([id, name, *harmful as u8])).collect();
    s = fill(&s, "/*BM:EFFECTS*/", &Value::Array(effects).to_string());
    fill(&s, "/*BM:PALETTE*/", &crate::map_palette::script_json())
}

/// Number of map font pages: U+E200 .. U+E2FF holds every map colour (see map_palette.rs), and U+E300 .. is a test
/// page for /maptest (a row that switches pages at every character, to show the drift that made the map one page).
pub const GLYPH_PAGES: usize = 2;

/// One of the map's font pages, U+E200 + 256 * page onwards: 16 x 16 cells of 16 px. Page 0 holds the 256 map
/// colours in cell order; page 1 is the /maptest page, every cell magenta. Every cell is fully opaque, so all
/// characters are equally wide.
pub fn glyph_page(page: usize) -> Vec<u8> {
    let p = crate::map_palette::palette();
    let mut px = vec![0u8; 256 * 256 * 4];
    for cell in 0..256 {
        let (row, col) = (cell / 16, cell % 16);
        let rgb = if page == 0 { p.cells[cell] } else { [255, 0, 255] };
        let c = [rgb[0], rgb[1], rgb[2], 255];
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

/// The status effects the pack lists down the minimap's left side: the game's id, the name shown, the potion colour,
/// and whether it is harmful (its name is red). Effect i's icon is character U+E400 + i, a bottle of its colour on
/// font page E4; the cell after the last is a grey bottle for effects missing here.
pub const EFFECTS: &[(&str, &str, u32, bool)] = &[
    ("speed", "Speed", 0x33ebff, false),
    ("slowness", "Slowness", 0x8bafe0, true),
    ("haste", "Haste", 0xd9c043, false),
    ("mining_fatigue", "Mining Fatigue", 0x4a4217, true),
    ("strength", "Strength", 0xffc700, false),
    ("instant_health", "Instant Health", 0xf82423, false),
    ("instant_damage", "Instant Damage", 0xa9656a, true),
    ("jump_boost", "Jump Boost", 0xfdff84, false),
    ("nausea", "Nausea", 0x551d4a, true),
    ("regeneration", "Regeneration", 0xcd5cab, false),
    ("resistance", "Resistance", 0x9146f0, false),
    ("fire_resistance", "Fire Resistance", 0xff9900, false),
    ("water_breathing", "Water Breathing", 0x98dac0, false),
    ("invisibility", "Invisibility", 0xf6f6f6, false),
    ("blindness", "Blindness", 0x1f1f23, true),
    ("night_vision", "Night Vision", 0xc2ff66, false),
    ("hunger", "Hunger", 0x587653, true),
    ("weakness", "Weakness", 0x484d48, true),
    ("poison", "Poison", 0x87a363, true),
    ("fatal_poison", "Fatal Poison", 0x4e9331, true),
    ("wither", "Wither", 0x736156, true),
    ("health_boost", "Health Boost", 0xf87d23, false),
    ("absorption", "Absorption", 0x2552a5, false),
    ("saturation", "Saturation", 0xf82423, false),
    ("levitation", "Levitation", 0xceffff, true),
    ("slow_falling", "Slow Falling", 0xf3cfb9, false),
    ("conduit_power", "Conduit Power", 0x1dc2d1, false),
    ("bad_omen", "Bad Omen", 0x0b6138, false),
    ("village_hero", "Hero of the Village", 0x44ff44, false),
    ("darkness", "Darkness", 0x292721, true),
    ("trial_omen", "Trial Omen", 0x16a6a6, false),
    ("raid_omen", "Raid Omen", 0xde4058, false),
    ("wind_charged", "Wind Charged", 0xbdc9ff, true),
    ("weaving", "Weaving", 0x78695a, true),
    ("oozing", "Oozing", 0x99ffa3, true),
    ("infested", "Infested", 0x8c9b8c, true),
];

/// The colour of an effect not in EFFECTS.
const OTHER_EFFECT: u32 = 0x8a8a8a;

/// A 16 x 16 potion bottle: O outline, c cork, G glass, L the potion, D its shadow, H a highlight.
const BOTTLE: [&str; 16] = [
    "................",
    "......OOOO......",
    "......OccO......",
    "......OccO......",
    ".....OOOOOO.....",
    "......OGGO......",
    ".....OGGGGO.....",
    "....OGGGGGGO....",
    "...OLLLLLLLLO...",
    "..OLHHLLLLLLLO..",
    "..OLHLLLLLLLLO..",
    "..OLLLLLLLLLLO..",
    "..OLLLLLLLLLDO..",
    "...OLLLLLLLDO...",
    "....OOOOOOOO....",
    "................",
];

/// Font page E4: the effect icons, a bottle of each effect's colour in cell order, then the grey one. The rest of
/// the page is empty.
pub fn effect_icon_page() -> Vec<u8> {
    let mut px = vec![0u8; 256 * 256 * 4];
    let colours = EFFECTS.iter().map(|e| e.2).chain([OTHER_EFFECT]);
    for (cell, rgb) in colours.enumerate() {
        let l = [(rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8];
        let mix = |c: [u8; 3], f: f64, to: f64| c.map(|v| (v as f64 * f + to * (1.0 - f)).round() as u8);
        let (row, col) = (cell / 16, cell % 16);
        for (y, line) in BOTTLE.iter().enumerate() {
            for (x, ch) in line.bytes().enumerate() {
                let rgb = match ch {
                    b'O' => [36, 30, 38],
                    b'c' => [150, 102, 58],
                    b'G' => [214, 230, 236],
                    b'L' => l,
                    b'D' => mix(l, 0.7, 0.0),
                    b'H' => mix(l, 0.4, 255.0),
                    _ => continue,
                };
                let o = ((row * 16 + y) * 256 + col * 16 + x) * 4;
                px[o..o + 4].copy_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
            }
        }
    }
    png_bytes(256, 256, &px)
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
        ("Bedrock Map Live RP/font/glyph_E4.png".into(), effect_icon_page()),
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
            "Bedrock Map Live RP/font/glyph_E3.png",
            "Bedrock Map Live RP/textures/bmlive/arrow_0.png",
            "Bedrock Map Live RP/textures/bmlive/arrow_7.png",
            "Bedrock Map Live RP/font/glyph_E4.png",
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
        // the map's colour in cell i of the palette is character U+E200 + i, drawn from font page E2
        let decode = |png: Vec<u8>| -> Vec<u8> {
            let mut r = png::Decoder::new(std::io::Cursor::new(png)).read_info().unwrap();
            let mut buf = vec![0u8; r.output_buffer_size()];
            r.next_frame(&mut buf).unwrap();
            buf
        };
        let pal = crate::map_palette::palette();
        let page = decode(glyph_page(0));
        for cell in 0..256 {
            let (row, col) = (cell / 16, cell % 16);
            for (dx, dy) in [(0, 0), (15, 0), (0, 15), (15, 15), (8, 8)] {
                let o = ((row * 16 + dy) * 256 + col * 16 + dx) * 4;
                assert_eq!(&page[o..o + 4], &[pal.cells[cell][0], pal.cells[cell][1], pal.cells[cell][2], 255], "cell {} not solid", cell);
            }
        }
        // the script gets the lookup table and the reserved cells
        let s = pack_script();
        assert!(s.contains(&format!("unknown: {}, white: {}, red: {} }}", pal.unknown, pal.white, pal.red)));
        assert!(s.contains(&crate::map_palette::base64(&pal.lut[..30])));
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


    /// hud_screen.json without its // comments (the game allows them, serde_json does not).
    fn hud_json() -> Value {
        let mut out = String::new();
        let mut in_str = false;
        let mut chars = RP_HUD.chars().peekable();
        while let Some(c) = chars.next() {
            if in_str {
                if c == '\\' {
                    out.push(c);
                    out.extend(chars.next());
                    continue;
                }
                in_str = c != '"';
            } else if c == '"' {
                in_str = true;
            } else if c == '/' && chars.peek() == Some(&'/') {
                while chars.peek().map_or(false, |n| *n != '\n') {
                    chars.next();
                }
                continue;
            }
            out.push(c);
        }
        serde_json::from_str(&out).expect("hud_screen.json is valid JSON once comments are removed")
    }

    /// Every value of `key` anywhere in `v`, and every object key, depth first.
    fn walk<'a>(v: &'a Value, key: &str, values: &mut Vec<&'a Value>, names: &mut Vec<&'a str>) {
        match v {
            Value::Object(m) => {
                for (k, x) in m {
                    names.push(k);
                    if k == key {
                        values.push(x);
                    }
                    walk(x, key, values, names);
                }
            }
            Value::Array(a) => a.iter().for_each(|x| walk(x, key, values, names)),
            _ => {}
        }
    }

    #[test]
    fn hud_splits_the_title_where_the_script_puts_the_effects() {
        let hud = hud_json();
        let mut z = zip::ZipArchive::new(std::io::Cursor::new(build_pack().unwrap())).unwrap();
        let (mut textures, mut names) = (Vec::new(), Vec::new());
        walk(&hud, "texture", &mut textures, &mut names);
        for t in textures.iter().filter_map(|t| t.as_str()).filter(|t| t.starts_with("textures/bmlive/")) {
            assert!(z.by_name(&format!("Bedrock Map Live RP/{}.png", t)).is_ok(), "{} is not in the pack", t);
        }
        // The minimap title is "bm:m:", the effect block (FX_BYTES bytes in UTF-8), then the map and its direction
        // mark. The HUD cuts it after "bm:m:" + the block; format lengths in JSON UI are bytes.
        let fx: usize = SCRIPT.split("const FX_BYTES = ").nth(1).and_then(|s| s.split(';').next()).and_then(|n| n.trim().parse().ok()).expect("FX_BYTES in main.js");
        assert_eq!(fx % 3, 0, "an empty block is all \"§r\", 3 bytes each");
        let cut = format!("('%.{}s' * #hud_title_text_string)", "bm:m:".len() + fx);
        let (mut exprs, mut _n) = (Vec::new(), Vec::new());
        walk(&hud, "source_property_name", &mut exprs, &mut _n);
        let exprs: Vec<&str> = exprs.iter().filter_map(|e| e.as_str()).collect();
        let maps = exprs.iter().filter(|e| **e == format!("(#hud_title_text_string - {})", cut)).count();
        assert_eq!(maps, 2, "both minimap labels show what follows the block ({})", cut);
        assert!(exprs.contains(&format!("({} - 'bm:m:')", cut).as_str()), "the effect list shows the block");
        assert!(exprs.iter().any(|e| e.starts_with("(not ((") && e.contains(&format!("{} - '§f')", cut))), "the list shows while the block has a line");
        assert_eq!(exprs.iter().filter(|e| e.contains("%.")).count(), exprs.iter().filter(|e| e.contains(&cut)).count(), "every cut is at the same place");
        // the list hangs off the minimap; the game's own effect icons only show while there is no minimap
        assert!(names.iter().any(|n| n.starts_with("bm_effects@hud.bm_effects")));
        assert!(!RP_HUD.contains("stack_panel"));
        let swap = hud["root_panel"]["modifications"].as_array().unwrap().iter().find(|m| m["control_name"] == "mob_effects_renderer" && m["operation"] == "replace").expect("the game's effect icons are replaced");
        let own = &swap["value"][0]["mob_effects_renderer@hud.mob_effects_renderer"];
        assert!(own["bindings"].as_array().unwrap().iter().any(|b| b["target_property_name"] == "#visible" && b["source_property_name"].as_str().unwrap().contains("'bm:m:'")));
    }

    #[test]
    fn effect_icons_are_bottles_of_each_colour() {
        let mut r = png::Decoder::new(std::io::Cursor::new(effect_icon_page())).read_info().unwrap();
        let mut px = vec![0u8; r.output_buffer_size()];
        r.next_frame(&mut px).unwrap();
        let at = |cell: usize, x: usize, y: usize| {
            let o = (((cell / 16) * 16 + y) * 256 + (cell % 16) * 16 + x) * 4;
            [px[o], px[o + 1], px[o + 2], px[o + 3]]
        };
        for (i, e) in EFFECTS.iter().enumerate() {
            assert_eq!(at(i, 7, 11), [(e.2 >> 16) as u8, (e.2 >> 8) as u8, e.2 as u8, 255], "{} bottle", e.0);
            assert_eq!(at(i, 0, 0)[3], 0, "transparent around the bottle");
        }
        assert_eq!(at(EFFECTS.len(), 7, 11), [0x8a, 0x8a, 0x8a, 255], "the grey bottle for other effects");
        assert_eq!(at(EFFECTS.len() + 1, 7, 11)[3], 0, "nothing after it");
        assert!(EFFECTS.len() < 255);
        // the script gets the names and which are harmful
        let s = pack_script();
        assert!(s.contains(r#"["village_hero","Hero of the Village",0]"#) && s.contains(r#"["poison","Poison",1]"#));
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
