//! The control panel's backend: the commands the page calls (they replace the old local web server).
use crate::builder::{build_map, copy_viewer, safe_name, BuildArgs, Event};
use crate::live::Live;
use crate::worlds::{describe_path, list_exports, list_worlds as find_worlds, world_roots, WorldRef};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Manager, State, WebviewUrl, WebviewWindowBuilder};

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Job {
    pub id: String,
    pub world: String,
    pub slug: String,
    pub status: String, // running | done | error
    pub stage: String,
    pub dim: Option<String>,
    pub dim_index: usize,
    pub dim_count: usize,
    pub n: usize,
    pub total: usize,
    pub notes: Vec<String>,
    pub error: Option<String>,
}

pub struct AppState {
    pub maps_dir: PathBuf,
    pub live: Arc<Live>,
    jobs: Arc<Mutex<HashMap<String, Job>>>,
    seq: Mutex<u64>,
    running: Arc<Mutex<bool>>,
}

/// Where maps live: the program's own data folder (copy an old `maps` folder in here to keep earlier maps).
fn data_dir() -> PathBuf {
    let base = if cfg!(target_os = "windows") {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library").join("Application Support"))
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local").join("share")))
    };
    base.unwrap_or_else(std::env::temp_dir).join("Bedrock Map")
}

impl AppState {
    pub fn new() -> AppState {
        let maps_dir = data_dir().join("maps");
        let _ = fs::create_dir_all(&maps_dir);
        // Bring maps built earlier up to date with the current map page.
        if let Ok(rd) = fs::read_dir(&maps_dir) {
            for e in rd.flatten() {
                if e.path().join("meta.json").exists() {
                    let _ = copy_viewer(&e.path());
                }
            }
        }
        let running = Arc::new(Mutex::new(false));
        let live = Live::new(data_dir(), maps_dir.clone(), running.clone());
        live.start();
        AppState { maps_dir, live, jobs: Arc::new(Mutex::new(HashMap::new())), seq: Mutex::new(0), running }
    }

    fn extra_file(&self) -> PathBuf {
        self.maps_dir.join("extra-worlds.json")
    }
    fn read_extra(&self) -> Vec<String> {
        fs::read_to_string(self.extra_file())
            .ok()
            .and_then(|s| serde_json::from_str::<Value>(&s).ok())
            .and_then(|v| v.get("paths").and_then(|p| p.as_array()).cloned())
            .map(|a| a.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect())
            .unwrap_or_default()
    }
    fn write_extra(&self, list: &[String]) {
        let _ = fs::create_dir_all(&self.maps_dir);
        let _ = fs::write(self.extra_file(), serde_json::to_string_pretty(&json!({ "paths": list })).unwrap_or_default());
    }
}

pub fn slug_of(w: &WorldRef) -> String {
    let s: String = safe_name(&w.name).split_whitespace().collect::<Vec<_>>().join("-");
    format!("{}-{}", s, &w.id[..6])
}

/// What the control panel shows about a built map (meta.json minus the big tile lists).
fn summarise(meta: &Value) -> Value {
    let dims: Vec<Value> = meta
        .get("dims")
        .and_then(|d| d.as_object())
        .map(|o| {
            o.iter()
                .map(|(name, d)| {
                    json!({
                        "name": name,
                        "dim": d.get("dim"),
                        "chunks": d.pointer("/stats/chunks").cloned().unwrap_or(json!(0)),
                        "tiles": d.get("tileCount").cloned().unwrap_or(json!(0)),
                        "bounds": d.get("bounds").cloned().unwrap_or(Value::Null)
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    json!({
        "generated": meta.get("generated"),
        "spawn": meta.get("spawn").cloned().unwrap_or(Value::Null),
        "player": meta.get("player").cloned().unwrap_or(Value::Null),
        "details": meta.get("details").cloned().unwrap_or(Value::Null),
        "build": meta.get("build").cloned().unwrap_or(Value::Null),
        "dims": dims
    })
}

fn all_worlds(state: &AppState) -> Vec<(WorldRef, String, Value)> {
    let mut seen = std::collections::HashSet::new();
    let mut out: Vec<WorldRef> = Vec::new();
    let mut add = |w: WorldRef| {
        if seen.insert(w.id.clone()) {
            out.push(w);
        }
    };
    find_worlds().into_iter().for_each(&mut add);
    for p in state.read_extra() {
        if let Ok(w) = describe_path(Path::new(&p)) {
            add(w);
        }
    }
    list_exports().into_iter().for_each(&mut add);
    out.into_iter()
        .map(|w| {
            let slug = slug_of(&w);
            let map = fs::read_to_string(state.maps_dir.join(&slug).join("meta.json"))
                .ok()
                .and_then(|s| serde_json::from_str::<Value>(&s).ok())
                .map(|m| summarise(&m))
                .unwrap_or(Value::Null);
            (w, slug, map)
        })
        .collect()
}

fn world_json(w: &WorldRef, slug: &str, map: &Value) -> Value {
    let mut v = w.to_json();
    v["slug"] = json!(slug);
    v["map"] = map.clone();
    v
}

#[tauri::command]
pub fn list_worlds(state: State<AppState>) -> Value {
    let worlds: Vec<Value> = all_worlds(&state).iter().map(|(w, s, m)| world_json(w, s, m)).collect();
    let searched: Vec<String> = world_roots().iter().map(|p| p.to_string_lossy().into_owned()).collect();
    let busy = *state.running.lock().unwrap();
    json!({ "worlds": worlds, "searched": searched, "busy": busy })
}

#[tauri::command]
pub fn add_world(state: State<AppState>, path: String) -> Result<Value, String> {
    let t = path.trim();
    if t.is_empty() {
        return Err("Type or paste a path first.".into());
    }
    let clean = t.trim_matches('"').to_string();
    let w = describe_path(Path::new(&clean))?;
    let mut list = state.read_extra();
    if !list.contains(&clean) {
        list.push(clean);
        state.write_extra(&list);
    }
    Ok(json!({ "world": w.to_json() }))
}

/// The world being played on this PC: the folder world whose files changed most recently, within 15 minutes.
/// Returns it with its map slug and whether that map has been built.
pub fn playing_world(state: &AppState) -> Option<(WorldRef, String, bool)> {
    let newest = |dir: &Path| -> Option<std::time::SystemTime> {
        let mut t = fs::metadata(dir.join("level.dat")).and_then(|m| m.modified()).ok();
        if let Ok(rd) = fs::read_dir(dir.join("db")) {
            for e in rd.flatten() {
                if let Ok(m) = e.metadata().and_then(|m| m.modified()) {
                    t = Some(t.map_or(m, |x| x.max(m)));
                }
            }
        }
        t
    };
    all_worlds(state)
        .into_iter()
        .filter(|(w, _, _)| w.kind == "folder")
        .filter_map(|(w, slug, map)| {
            let t = newest(&w.dir)?;
            (t.elapsed().map_or(false, |d| d.as_secs() < 15 * 60)).then_some((t, w, slug, !map.is_null()))
        })
        .max_by_key(|(t, ..)| *t)
        .map(|(_, w, slug, built)| (w, slug, built))
}

pub fn job(state: &AppState, id: &str) -> Option<Job> {
    state.jobs.lock().unwrap().get(id).cloned()
}

#[tauri::command]
pub fn start_build(state: State<AppState>, id: String, max_y: Option<String>, caves: Option<bool>) -> Result<Value, String> {
    Ok(serde_json::to_value(begin_build(&state, id, max_y, caves)?).unwrap())
}

/// Starts building a world's map on a background thread (one build at a time).
pub fn begin_build(state: &AppState, id: String, max_y: Option<String>, caves: Option<bool>) -> Result<Job, String> {
    let (world, slug, _) = all_worlds(state)
        .into_iter()
        .find(|(w, _, _)| w.id == id)
        .ok_or("That world is no longer in the list. Refresh the page.")?;
    let max_y = match max_y.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(s) => {
            let n: f64 = s.parse().map_err(|_| "The height limit must be a number.".to_string())?;
            if !n.is_finite() {
                return Err("The height limit must be a number.".into());
            }
            Some(n as i32)
        }
    };
    {
        let mut running = state.running.lock().unwrap();
        if *running {
            return Err("A map is already being built. Wait for it to finish.".into());
        }
        *running = true;
    }
    let job_id = {
        let mut s = state.seq.lock().unwrap();
        *s += 1;
        format!("j{}", *s)
    };
    let job = Job {
        id: job_id.clone(),
        world: world.name.clone(),
        slug: slug.clone(),
        status: "running".into(),
        stage: "Starting".into(),
        dim: None,
        dim_index: 0,
        dim_count: 0,
        n: 0,
        total: 0,
        notes: vec![],
        error: None,
    };
    state.jobs.lock().unwrap().insert(job_id.clone(), job.clone());

    let caves = caves.unwrap_or(true);
    let jobs = state.jobs.clone();
    let running = state.running.clone();
    let out = state.maps_dir.join(&slug);
    let input = world.dir.clone();
    std::thread::spawn(move || {
        let update = |f: &mut dyn FnMut(&mut Job)| {
            if let Some(j) = jobs.lock().unwrap().get_mut(&job_id) {
                f(j);
            }
        };
        let result = catch_unwind(AssertUnwindSafe(|| {
            let mut emit = |ev: Event| {
                update(&mut |j: &mut Job| match &ev {
                    Event::Stage(m) => j.stage = m.clone(),
                    Event::Dim { name, index, count, .. } => {
                        j.dim = Some(name.clone());
                        j.dim_index = *index;
                        j.dim_count = *count;
                        j.n = 0;
                        j.total = 0;
                        j.stage = format!("Drawing {}", name);
                    }
                    Event::Tile { n, total } => {
                        j.n = *n;
                        j.total = *total;
                    }
                    Event::Note(m) => j.notes.push(m.clone()),
                });
            };
            build_map(BuildArgs { input, out, dims: None, max_y, nether_roof: "skip".into(), caves }, &mut emit)
        }));
        update(&mut |j: &mut Job| match &result {
            Ok(Ok(_)) => j.status = "done".into(),
            Ok(Err(e)) => {
                j.status = "error".into();
                j.error = Some(e.clone());
            }
            Err(_) => {
                j.status = "error".into();
                j.error = Some("The map builder stopped unexpectedly.".into());
            }
        });
        *running.lock().unwrap() = false;
    });
    Ok(job)
}

#[tauri::command]
pub fn job_status(state: State<AppState>, id: String) -> Result<Value, String> {
    state
        .jobs
        .lock()
        .unwrap()
        .get(&id)
        .map(|j| serde_json::to_value(j).unwrap())
        .ok_or_else(|| "Unknown job".to_string())
}

/// Opens a built map in its own window.
#[tauri::command]
pub async fn open_map(app: AppHandle, state: State<'_, AppState>, slug: String) -> Result<(), String> {
    open_map_window(&app, &state.maps_dir, &slug, true)
}

/// Opens (or, with `focus`, brings forward) the window of a built map. Without `focus` a new window opens behind
/// whatever has the keyboard, so a game being played keeps it.
pub fn open_map_window(app: &AppHandle, maps_dir: &Path, slug: &str, focus: bool) -> Result<(), String> {
    if slug.is_empty() || !maps_dir.join(slug).join("meta.json").exists() {
        return Err("That map has not been built yet.".into());
    }
    let label: String = format!("map-{}", slug.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect::<String>());
    if let Some(w) = app.get_webview_window(&label) {
        if focus {
            let _ = w.set_focus();
        } else {
            // Already open: reload it so it picks up a rebuilt map.
            let _ = w.eval("location.reload()");
        }
        return Ok(());
    }
    // Windows serves custom schemes over http://<scheme>.localhost, other systems use <scheme>://localhost.
    let base = if cfg!(target_os = "windows") { "http://maps.localhost" } else { "maps://localhost" };
    let enc: String = slug.bytes().map(|b| if b.is_ascii_alphanumeric() || b == b'-' || b == b'_' { (b as char).to_string() } else { format!("%{:02X}", b) }).collect();
    let url = tauri::Url::parse(&format!("{}/{}/", base, enc)).map_err(|e| e.to_string())?;
    WebviewWindowBuilder::new(app, label, WebviewUrl::External(url))
        .title("Bedrock map")
        .inner_size(1280.0, 860.0)
        .focused(focus)
        .build()
        .map_err(|e| e.to_string())?;
    Ok(())
}
