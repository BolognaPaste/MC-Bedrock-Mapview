//! Finding Bedrock worlds on this PC and unpacking .mcworld exports.
use serde_json::{json, Value};
use sha1::{Digest, Sha1};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone)]
pub struct WorldRef {
    pub id: String,
    pub kind: &'static str, // "folder" | "export"
    pub name: String,
    pub dir: PathBuf,
    pub mtime: f64, // ms since epoch
    pub bytes: u64,
}

impl WorldRef {
    pub fn to_json(&self) -> Value {
        json!({ "id": self.id, "kind": self.kind, "name": self.name, "dir": self.dir.to_string_lossy(),
                "mtime": self.mtime, "bytes": self.bytes })
    }
}

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|s| !s.is_empty())
}

/// Folders where Minecraft Bedrock keeps worlds on Windows (release, preview, and the newer per-user layout).
pub fn world_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(v) = std::env::var_os("BEDROCK_WORLDS_DIR") {
        roots.extend(std::env::split_paths(&v).filter(|p| !p.as_os_str().is_empty()));
    }
    if let Some(local) = env("LOCALAPPDATA") {
        for pkg in [
            "Microsoft.MinecraftUWP_8wekyb3d8bbwe",
            "Microsoft.MinecraftWindowsBeta_8wekyb3d8bbwe",
            "Microsoft.MinecraftEducationEdition_8wekyb3d8bbwe",
        ] {
            roots.push(Path::new(&local).join("Packages").join(pkg).join("LocalState").join("games").join("com.mojang").join("minecraftWorlds"));
        }
    }
    if let Some(app) = env("APPDATA") {
        for a in ["Minecraft Bedrock", "Minecraft Bedrock Preview", "Minecraft Education"] {
            let users = Path::new(&app).join(a).join("Users");
            if let Ok(rd) = fs::read_dir(&users) {
                for u in rd.flatten() {
                    roots.push(u.path().join("games").join("com.mojang").join("minecraftWorlds"));
                }
            }
        }
    }
    roots
}

pub fn is_world(dir: &Path) -> bool {
    dir.join("db").exists() && dir.join("level.dat").exists()
}

fn abs(p: &Path) -> PathBuf {
    std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf())
}

pub fn id_of(p: &Path) -> String {
    let s = abs(p).to_string_lossy().to_lowercase();
    let digest = Sha1::digest(s.as_bytes());
    let hex: String = digest.iter().map(|b| format!("{:02x}", b)).collect();
    hex[..10].to_string()
}

fn mtime_ms(p: &Path) -> f64 {
    fs::metadata(p)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs_f64() * 1000.0)
        .unwrap_or(0.0)
}

fn dir_size(dir: &Path) -> u64 {
    let mut n = 0;
    if let Ok(rd) = fs::read_dir(dir) {
        for e in rd.flatten() {
            if let Ok(m) = e.metadata() {
                n += m.len();
            }
        }
    }
    n
}

fn describe_folder(dir: &Path, fallback: Option<&str>) -> WorldRef {
    let mut name = fallback
        .map(|s| s.to_string())
        .unwrap_or_else(|| dir.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default());
    if let Ok(t) = fs::read_to_string(dir.join("levelname.txt")) {
        if !t.trim().is_empty() {
            name = t.trim().to_string();
        }
    }
    WorldRef { id: id_of(dir), kind: "folder", name, dir: dir.to_path_buf(), mtime: mtime_ms(&dir.join("level.dat")), bytes: dir_size(&dir.join("db")) }
}

fn sort_newest(v: &mut Vec<WorldRef>) {
    v.sort_by(|a, b| b.mtime.partial_cmp(&a.mtime).unwrap_or(std::cmp::Ordering::Equal));
}

pub fn list_worlds() -> Vec<WorldRef> {
    let mut out = Vec::new();
    for root in world_roots() {
        let rd = match fs::read_dir(&root) {
            Ok(r) => r,
            Err(_) => continue,
        };
        for e in rd.flatten() {
            let dir = e.path();
            if is_world(&dir) {
                out.push(describe_folder(&dir, Some(&*e.file_name().to_string_lossy())));
            }
        }
    }
    sort_newest(&mut out);
    out
}

fn export_ref(f: &Path, name: String) -> Option<WorldRef> {
    let m = fs::metadata(f).ok()?;
    if !m.is_file() {
        return None;
    }
    Some(WorldRef { id: id_of(f), kind: "export", name, dir: f.to_path_buf(), mtime: mtime_ms(f), bytes: m.len() })
}

fn strip_ext_ci(name: &str, exts: &[&str]) -> String {
    let lower = name.to_lowercase();
    for e in exts {
        if lower.ends_with(e) {
            if let Some(stem) = name.get(..name.len().saturating_sub(e.len())) {
                return stem.to_string();
            }
        }
    }
    name.to_string()
}

/// Exported worlds (.mcworld files) lying around in the usual places.
pub fn list_exports() -> Vec<WorldRef> {
    let mut bases: Vec<PathBuf> = Vec::new();
    if let Some(h) = env("USERPROFILE").or_else(|| env("HOME")) {
        bases.push(PathBuf::from(h));
    }
    if let Some(od) = env("OneDrive") {
        bases.push(PathBuf::from(od));
    }
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for b in bases {
        for d in ["Downloads", "Desktop", "Documents"] {
            let dir = b.join(d);
            let rd = match fs::read_dir(&dir) {
                Ok(r) => r,
                Err(_) => continue,
            };
            for e in rd.flatten() {
                let n = e.file_name().to_string_lossy().into_owned();
                if !n.to_lowercase().ends_with(".mcworld") {
                    continue;
                }
                let f = e.path();
                if !seen.insert(f.to_string_lossy().to_lowercase()) {
                    continue;
                }
                if let Some(w) = export_ref(&f, strip_ext_ci(&n, &[".mcworld"])) {
                    out.push(w);
                }
            }
        }
    }
    sort_newest(&mut out);
    out
}

fn find_world_dir(dir: &Path, depth: u32) -> Option<PathBuf> {
    if is_world(dir) {
        return Some(dir.to_path_buf());
    }
    if depth >= 2 {
        return None;
    }
    let rd = fs::read_dir(dir).ok()?;
    for e in rd.flatten() {
        if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            if let Some(r) = find_world_dir(&e.path(), depth + 1) {
                return Some(r);
            }
        }
    }
    None
}

/// A path the user typed or dropped in: world folder, a folder containing worlds, or a .mcworld/.zip file.
pub fn describe_path(p: &Path) -> Result<WorldRef, String> {
    let m = fs::metadata(p).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            "That path does not exist.".to_string()
        } else {
            e.to_string()
        }
    })?;
    if m.is_file() {
        let n = p.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        return export_ref(p, strip_ext_ci(&n, &[".mcworld", ".zip"])).ok_or_else(|| "Could not read that file.".to_string());
    }
    let dir = find_world_dir(p, 0).ok_or("No Bedrock world (a folder with level.dat and db) was found there.")?;
    Ok(describe_folder(&dir, None))
}

/// Deletes its folder when dropped (used for unpacked .mcworld files).
pub struct TempDir(pub PathBuf);
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Extracts a .mcworld / .zip into `dest`. Entries that would land outside it are refused.
fn extract(zip_file: &Path, dest: &Path) -> Result<(), String> {
    let f = fs::File::open(zip_file).map_err(|e| e.to_string())?;
    let mut ar = zip::ZipArchive::new(f).map_err(|_| "This does not look like a zip / .mcworld file.".to_string())?;
    for i in 0..ar.len() {
        let mut entry = ar.by_index(i).map_err(|e| e.to_string())?;
        let rel = entry.enclosed_name().ok_or_else(|| format!("Refusing unsafe path in zip: {}", entry.name()))?;
        let target = dest.join(rel);
        if entry.is_dir() {
            fs::create_dir_all(&target).map_err(|e| e.to_string())?;
            continue;
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let mut out = fs::File::create(&target).map_err(|e| e.to_string())?;
        std::io::copy(&mut entry, &mut out).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Accepts a world folder or a .mcworld/.zip file. Returns the world folder and an optional temp folder to clean up.
pub fn resolve_world(input: &Path) -> Result<(PathBuf, Option<TempDir>), String> {
    let m = fs::metadata(input).map_err(|e| e.to_string())?;
    if m.is_dir() {
        let dir = find_world_dir(input, 0).ok_or_else(|| format!("No Bedrock world (level.dat + db folder) found in {}", input.display()))?;
        return Ok((dir, None));
    }
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let temp = std::env::temp_dir().join(format!("bedrock-map-{}-{}", std::process::id(), stamp));
    fs::create_dir_all(&temp).map_err(|e| e.to_string())?;
    let guard = TempDir(temp.clone());
    extract(input, &temp)?;
    let dir = find_world_dir(&temp, 0).ok_or_else(|| format!("No Bedrock world found inside {}", input.display()))?;
    Ok((dir, Some(guard)))
}
