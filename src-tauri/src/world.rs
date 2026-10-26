use crate::chunk::{decode_subchunk, SubChunk};
use crate::leveldb::{LevelDb, Loc};
use crate::nbt;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Mutex;
use std::fs;
use std::path::{Path, PathBuf};

const SUBCHUNK_TAG: u8 = 47;
const LOCAL_PLAYER: &[u8] = b"~local_player";

pub fn dim_name(id: i32) -> String {
    match id {
        0 => "overworld".into(),
        1 => "nether".into(),
        2 => "end".into(),
        n => format!("dim{}", n),
    }
}

fn is_wanted(k: &[u8]) -> bool {
    (k.len() == 10 && k[8] == SUBCHUNK_TAG) || (k.len() == 14 && k[12] == SUBCHUNK_TAG) || k == LOCAL_PLAYER
}

pub struct ChunkEntry {
    pub cx: i32,
    pub cz: i32,
    pub subs: HashMap<i8, Loc>,
}

pub struct Dim {
    pub chunks: HashMap<(i32, i32), ChunkEntry>,
}

pub struct World {
    pub dir: PathBuf,
    pub db: LevelDb,
    pub dims: HashMap<i32, Dim>,
    pub warnings: Vec<String>,
    pub bad_subchunks: AtomicU32,
    pub bad_reasons: Mutex<Vec<(String, u32)>>,
    pub subchunks_read: AtomicU64,
}

pub struct Info {
    pub name: String,
    pub spawn: Value,
    pub player: Value,
    pub details: Value,
}

impl World {
    pub fn open(dir: &Path) -> Result<World, String> {
        let db = LevelDb::open(&dir.join("db"), is_wanted)?;
        let mut dims: HashMap<i32, Dim> = HashMap::new();
        for loc in db.entries() {
            let k = &loc.key;
            if k.as_slice() == LOCAL_PLAYER || k.len() < 10 {
                continue;
            }
            let cx = i32::from_le_bytes(k[0..4].try_into().unwrap());
            let cz = i32::from_le_bytes(k[4..8].try_into().unwrap());
            let dim = if k.len() == 14 { i32::from_le_bytes(k[8..12].try_into().unwrap()) } else { 0 };
            let y = k[k.len() - 1] as i8;
            let d = dims.entry(dim).or_insert_with(|| Dim { chunks: HashMap::new() });
            let c = d.chunks.entry((cx, cz)).or_insert_with(|| ChunkEntry { cx, cz, subs: HashMap::new() });
            c.subs.insert(y, loc.clone());
        }
        let warnings = db.warnings.clone();
        Ok(World {
            dir: dir.to_path_buf(),
            db,
            dims,
            warnings,
            bad_subchunks: AtomicU32::new(0),
            bad_reasons: Mutex::new(Vec::new()),
            subchunks_read: AtomicU64::new(0),
        })
    }

    pub fn chunk(&self, dim: i32, cx: i32, cz: i32) -> Option<&ChunkEntry> {
        self.dims.get(&dim).and_then(|d| d.chunks.get(&(cx, cz)))
    }

    pub fn decode_sub(&self, chunk: &ChunkEntry, y: i8) -> Option<SubChunk> {
        let loc = chunk.subs.get(&y)?;
        match self.db.read(loc).and_then(|b| decode_subchunk(&b)) {
            Ok(r) => {
                self.subchunks_read.fetch_add(1, Ordering::Relaxed);
                r
            }
            Err(e) => {
                self.bad_subchunks.fetch_add(1, Ordering::Relaxed);
                let mut reasons = self.bad_reasons.lock().unwrap();
                match reasons.iter_mut().find(|(r, _)| *r == e) {
                    Some(entry) => entry.1 += 1,
                    None => reasons.push((e, 1)),
                }
                None
            }
        }
    }

    pub fn info(&self) -> Info {
        let mut name = self.dir.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let mut spawn = Value::Null;
        let mut details = Map::new();
        if let Ok(t) = fs::read_to_string(self.dir.join("levelname.txt")) {
            let t = t.trim();
            if !t.is_empty() {
                name = t.to_string();
            }
        }
        if let Ok(b) = fs::read(self.dir.join("level.dat")) {
            if let Ok(Some(t)) = nbt::parse(&b, 8) {
                let v = &t.value;
                let num = |k: &str| v.get(k).and_then(|x| x.as_f64()).filter(|x| x.is_finite());
                if let Some(n) = v.get("LevelName").and_then(|x| x.as_str()) {
                    if !n.is_empty() {
                        name = n.to_string();
                    }
                }
                if let (Some(x), Some(z)) = (num("SpawnX"), num("SpawnZ")) {
                    let mut s = Map::new();
                    s.insert("x".into(), json!(x));
                    s.insert("z".into(), json!(z));
                    if let Some(y) = num("SpawnY") {
                        s.insert("y".into(), json!(y));
                    }
                    spawn = Value::Object(s);
                }
                if let Some(g) = num("GameType") {
                    details.insert("gameMode".into(), json!(g));
                }
                if let Some(d) = num("Difficulty") {
                    details.insert("difficulty".into(), json!(d));
                }
                if let Some(lp) = num("LastPlayed") {
                    if lp > 0.0 {
                        details.insert("lastPlayed".into(), json!(if lp > 1e11 { lp } else { lp * 1000.0 }));
                    }
                }
                if let Some(c) = num("commandsEnabled") {
                    details.insert("cheats".into(), json!(c != 0.0));
                }
                if let Some(list) = v.get("lastOpenedWithVersion").and_then(|x| x.as_list()) {
                    let nums: Vec<f64> = list.iter().filter_map(|x| x.as_f64()).collect();
                    if list.len() >= 3 && nums.len() == list.len() {
                        let s: Vec<String> = nums.iter().map(|n| format!("{}", *n as i64)).collect();
                        details.insert("version".into(), json!(s.join(".")));
                    }
                }
            }
        }
        let mut player = Value::Null;
        if let Some(loc) = self.db.lookup(LOCAL_PLAYER) {
            if let Ok(b) = self.db.read(loc) {
                if let Ok(Some(t)) = nbt::parse(&b, 0) {
                    if let Some(pos) = t.value.get("Pos").and_then(|p| p.as_list()) {
                        if pos.len() == 3 {
                            let c: Vec<f64> = pos.iter().filter_map(|x| x.as_f64()).collect();
                            if c.len() == 3 {
                                let dim = t.value.get("DimensionId").and_then(|d| d.as_f64()).unwrap_or(0.0);
                                player = json!({ "x": c[0], "y": c[1], "z": c[2], "dim": dim });
                            }
                        }
                    }
                }
            }
        }
        Info { name, spawn, player, details: Value::Object(details) }
    }
}
