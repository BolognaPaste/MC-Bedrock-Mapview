//! The in-game pack (pack/ in the source): a behavior pack for ordinary and co-op worlds. Such worlds cannot reach
//! the network from scripts, so the pack writes live link messages to Minecraft's content log, and this module
//! reads them as the log grows and hands them to live.rs. It also builds the .mcpack (with the app's own list of
//! see-through blocks filled in), and checks and turns on the game's "Content Log File" setting.
use crate::colors::{SKIP_EXACT, SKIP_PATTERN};
use crate::live::Live;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const MANIFEST: &str = include_str!("../../pack/manifest.json");
const SCRIPT: &str = include_str!("../../pack/scripts/main.js");
const ICON: &[u8] = include_bytes!("../icons/128x128.png");
/// Every pack line starts with this, followed by one JSON message.
const TAG: &str = "BMLIVE1 ";
pub const PACK_FILE: &str = "Bedrock Map Live.mcpack";

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

/// The pack's script with the app's see-through block list filled in.
pub fn pack_script() -> String {
    let s = fill(SCRIPT, "/*BM:SKIP*/", &serde_json::to_string(SKIP_EXACT).unwrap_or_else(|_| "[]".into()));
    fill(&s, "/*BM:SKIPRE*/", &serde_json::to_string(SKIP_PATTERN).unwrap_or_else(|_| "''".into()))
}

/// The .mcpack file (a zip of the behavior pack).
pub fn build_pack() -> Result<Vec<u8>, String> {
    use zip::write::SimpleFileOptions;
    let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let opts = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    for (name, bytes) in [("manifest.json", MANIFEST.as_bytes().to_vec()), ("scripts/main.js", pack_script().into_bytes()), ("pack_icon.png", ICON.to_vec())] {
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
    fn pack_is_a_zip_with_the_manifest_and_script() {
        let bytes = build_pack().unwrap();
        let mut z = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        let mut m = String::new();
        z.by_name("manifest.json").unwrap().read_to_string(&mut m).unwrap();
        let v: Value = serde_json::from_str(&m).unwrap();
        assert_eq!(v["modules"][0]["entry"], "scripts/main.js");
        assert!(z.by_name("scripts/main.js").is_ok() && z.by_name("pack_icon.png").is_ok());
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
