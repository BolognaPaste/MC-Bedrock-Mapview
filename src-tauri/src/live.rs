//! Live link: a small HTTP server that companion plugins push live data to (online players, changed chunks,
//! events). The protocol is described in docs/live-protocol.md. The control panel's Live link tab shows and
//! configures it; map windows read `live.json` (served by protocol.rs) to show players and redraw changed tiles.
use crate::live_game;
use crate::live_tiles::{self, Chunk};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::fs;
use std::hash::{BuildHasher, Hasher};
use std::io::Read;
use std::net::UdpSocket;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Manager, State};
use tiny_http::{Header, Method, Request, Response, Server};

pub const PROTOCOL: u64 = 1;
pub const DEFAULT_PORT: u16 = 8130;
const MAX_BODY: usize = 4 << 20;
const MAX_MESSAGES: usize = 2000;
const QUIET: Duration = Duration::from_secs(30);
const DROP: Duration = Duration::from_secs(180);
const LOG_LINES: usize = 300;

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Config {
    pub enabled: bool,
    pub port: u16,
    /// Listen on every network interface instead of only this computer.
    pub allow_lan: bool,
    pub key: String,
    /// Remembered links: plugin world id or name -> map slug.
    pub links: HashMap<String, String>,
    /// When the in-game pack reports a world: build its map if there is none, and open the map window.
    pub auto_open: bool,
}

impl Default for Config {
    fn default() -> Config {
        Config { enabled: true, port: DEFAULT_PORT, allow_lan: false, key: new_key(), links: HashMap::new(), auto_open: true }
    }
}

/// 32 random hex characters. RandomState is seeded from the operating system's random source.
fn new_key() -> String {
    let mut s = String::new();
    for i in 0..2u64 {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u64(i);
        h.write_u128(SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos());
        s += &format!("{:016x}", h.finish());
    }
    s
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
}

#[derive(Clone, Serialize)]
pub struct Player {
    pub id: String,
    pub name: String,
    pub dim: i64,
    pub x: f64,
    pub y: f64,
    pub z: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub yaw: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub health: Option<f64>,
}

impl Player {
    fn from_json(v: &Value) -> Option<Player> {
        let f = |k: &str| v.get(k).and_then(|x| x.as_f64()).filter(|x| x.is_finite());
        let s = |k: &str| v.get(k).and_then(|x| x.as_str().map(|s| s.to_string()).or_else(|| x.as_i64().map(|n| n.to_string())));
        Some(Player {
            id: s("id")?,
            name: s("name")?.chars().take(64).collect(),
            dim: v.get("dim")?.as_i64()?,
            x: f("x")?,
            y: f("y")?,
            z: f("z")?,
            yaw: f("yaw"),
            health: f("health"),
        })
    }
}

#[derive(Clone, Serialize)]
struct LogLine {
    t: u64,
    kind: String,
    text: String,
}

struct Session {
    id: String,
    plugin: String,
    version: String,
    world: String,
    world_key: String,
    server: String,
    addr: String,
    started: u64,
    last: Instant,
    last_ms: u64,
    link: Option<String>,
    players: Vec<Player>,
    world_state: Value,
    messages: u64,
    chunks: u64,
    dropped_chunks: u64,
    /// The automatic build-and-open for this session's world has been done (or started).
    auto_done: bool,
}

impl Session {
    fn to_json(&self) -> Value {
        let quiet = self.last.elapsed() > QUIET;
        json!({
            "id": self.id, "plugin": self.plugin, "version": self.version, "world": self.world, "server": self.server,
            "addr": self.addr, "started": self.started, "last": self.last_ms, "state": if quiet { "quiet" } else { "live" },
            "link": self.link, "players": self.players.len(), "messages": self.messages, "chunks": self.chunks,
            "droppedChunks": self.dropped_chunks, "worldState": self.world_state
        })
    }
}

#[derive(Default)]
struct Inner {
    config: Config,
    status: String, // off | listening | error
    detail: String,
    sessions: HashMap<String, Session>,
    log: VecDeque<LogLine>,
    /// Chunks waiting to be drawn, per map slug (a newer copy of a chunk replaces the older one).
    queue: HashMap<String, HashMap<(i32, i32, i32), Chunk>>,
    /// Per map slug: revision of every file live updates rewrote, and of meta.json.
    revs: HashMap<String, HashMap<String, u64>>,
    meta_rev: HashMap<String, u64>,
    /// The automatic map build in progress or last finished, for the Live link tab.
    auto: Option<Value>,
    rev: u64,
    seq: u64,
    totals: (u64, u64, u64), // requests, chunks drawn, rejected requests
}

impl Inner {
    fn say(&mut self, kind: &str, text: String) {
        self.log.push_back(LogLine { t: now_ms(), kind: kind.into(), text });
        while self.log.len() > LOG_LINES {
            self.log.pop_front();
        }
    }
}

pub struct Live {
    inner: Mutex<Inner>,
    maps_dir: PathBuf,
    data_dir: PathBuf,
    config_path: PathBuf,
    /// The in-game pack's content-log reader (live_game.rs).
    game: Arc<Mutex<live_game::GameState>>,
    /// Set once the app is running; needed to start builds and open map windows.
    app: OnceLock<AppHandle>,
    /// The map builder's "a build is running" flag; live drawing waits while a build rewrites the tiles.
    building: Arc<Mutex<bool>>,
    server: Mutex<Option<(Arc<Server>, JoinHandle<()>)>>,
}

/// (slug, world name) of every built map.
fn built_maps(maps_dir: &std::path::Path) -> Vec<(String, String)> {
    let mut out = Vec::new();
    if let Ok(rd) = fs::read_dir(maps_dir) {
        for e in rd.flatten() {
            if let Ok(s) = fs::read_to_string(e.path().join("meta.json")) {
                let name = serde_json::from_str::<Value>(&s).ok().and_then(|m| m["name"].as_str().map(|s| s.to_string())).unwrap_or_default();
                out.push((e.file_name().to_string_lossy().into_owned(), name));
            }
        }
    }
    out.sort_by(|a, b| a.1.to_lowercase().cmp(&b.1.to_lowercase()));
    out
}

/// The address other devices on the network would use (the interface that routes outwards). No packet is sent.
fn lan_ip() -> Option<String> {
    let s = UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("8.8.8.8:80").ok()?;
    let ip = s.local_addr().ok()?.ip();
    (!ip.is_loopback() && !ip.is_unspecified()).then(|| ip.to_string())
}

fn same_key(a: &str, b: &str) -> bool {
    // Compares every byte so the time taken does not hint at how much of the key was right.
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

type Reply = (u16, Value);
fn err(code: u16, msg: &str) -> Reply {
    (code, json!({ "ok": false, "error": msg }))
}

impl Live {
    pub fn new(data_dir: PathBuf, maps_dir: PathBuf, building: Arc<Mutex<bool>>) -> Arc<Live> {
        let config_path = data_dir.join("live-link.json");
        // (a byte-order mark, as Notepad may add, is skipped)
        let config = fs::read_to_string(&config_path).ok().and_then(|s| serde_json::from_str::<Config>(s.trim_start_matches('\u{feff}')).ok());
        let fresh = config.is_none();
        let mut config = config.unwrap_or_default();
        if config.key.len() < 16 {
            config.key = new_key();
        }
        if config.port == 0 {
            config.port = DEFAULT_PORT;
        }
        let live = Arc::new(Live {
            inner: Mutex::new(Inner { config, status: "off".into(), ..Default::default() }),
            maps_dir,
            data_dir,
            config_path,
            game: Arc::new(Mutex::new(live_game::GameState::default())),
            app: OnceLock::new(),
            building,
            server: Mutex::new(None),
        });
        if fresh {
            live.save();
        }
        live
    }

    fn save(&self) {
        let c = self.inner.lock().unwrap().config.clone();
        if let Some(d) = self.config_path.parent() {
            let _ = fs::create_dir_all(d);
        }
        let _ = fs::write(&self.config_path, serde_json::to_string_pretty(&c).unwrap_or_default());
    }

    /// Starts the server (if enabled) and the background worker that draws chunks and ends silent sessions.
    pub fn start(self: &Arc<Self>) {
        self.restart();
        live_game::start(self.clone(), self.game.clone());
        let me = self.clone();
        std::thread::Builder::new()
            .name("live-worker".into())
            .spawn(move || loop {
                std::thread::sleep(Duration::from_millis(1000));
                me.tick();
            })
            .ok();
    }

    /// (Re)binds the server to the configured address.
    fn restart(self: &Arc<Self>) {
        if let Some((srv, handle)) = self.server.lock().unwrap().take() {
            srv.unblock();
            let _ = handle.join();
        }
        let (enabled, port, lan) = {
            let i = self.inner.lock().unwrap();
            (i.config.enabled, i.config.port, i.config.allow_lan)
        };
        let mut i = self.inner.lock().unwrap();
        if !enabled {
            i.status = "off".into();
            i.detail = String::new();
            i.say("info", "Live link turned off.".into());
            return;
        }
        let addr = format!("{}:{}", if lan { "0.0.0.0" } else { "127.0.0.1" }, port);
        match Server::http(&addr) {
            Ok(srv) => {
                let srv = Arc::new(srv);
                let (me, s2) = (self.clone(), srv.clone());
                let handle = std::thread::Builder::new()
                    .name("live-http".into())
                    .spawn(move || {
                        for rq in s2.incoming_requests() {
                            me.handle(rq);
                        }
                    })
                    .expect("live-http thread");
                *self.server.lock().unwrap() = Some((srv, handle));
                i.status = "listening".into();
                i.detail = addr.clone();
                i.say("info", format!("Listening for plugins on {}.", addr));
            }
            Err(e) => {
                i.status = "error".into();
                i.detail = format!("Could not listen on {}: {}", addr, e);
                let d = i.detail.clone();
                i.say("error", d);
            }
        }
    }

    fn handle(self: &Arc<Self>, mut rq: Request) {
        let path = rq.url().split('?').next().unwrap_or("").trim_end_matches('/').to_string();
        let addr = rq.remote_addr().map(|a| a.ip().to_string()).unwrap_or_default();
        let method = rq.method().clone();
        let (code, body) = match (&method, path.as_str()) {
            (Method::Get, "/v1/ping") | (Method::Get, "/v1") | (Method::Get, "") => (200, json!({ "ok": true, "app": "bedrock-map", "protocol": PROTOCOL })),
            (Method::Post, p) if p.starts_with("/v1/") => {
                let auth = rq.headers().iter().find(|h| h.field.equiv("Authorization")).map(|h| h.value.as_str().to_string()).unwrap_or_default();
                let key = self.inner.lock().unwrap().config.key.clone();
                if !same_key(auth.strip_prefix("Bearer ").unwrap_or("").trim(), &key) {
                    self.inner.lock().unwrap().totals.2 += 1;
                    err(401, "Missing or wrong key. Copy the plugin key from the Live link tab.")
                } else {
                    let mut buf = Vec::new();
                    let read = rq.as_reader().take(MAX_BODY as u64 + 1).read_to_end(&mut buf);
                    if read.is_err() {
                        err(400, "Could not read the request body.")
                    } else if buf.len() > MAX_BODY {
                        err(413, "Request body is over 4 MB. Send fewer messages per request.")
                    } else {
                        match serde_json::from_slice::<Value>(&buf) {
                            Err(e) => err(400, &format!("Body is not valid JSON: {}", e)),
                            Ok(v) => match p {
                                "/v1/hello" => self.hello(&v, addr),
                                "/v1/push" => self.push(&v),
                                "/v1/bye" => self.bye(&v),
                                _ => err(404, "Unknown endpoint."),
                            },
                        }
                    }
                }
            }
            _ => err(404, "Unknown endpoint. The plugin address should end in /v1."),
        };
        self.inner.lock().unwrap().totals.0 += 1;
        let resp = Response::from_string(body.to_string())
            .with_status_code(code)
            .with_header(Header::from_bytes("Content-Type", "application/json").unwrap());
        let _ = rq.respond(resp);
    }

    fn hello(&self, v: &Value, addr: String) -> Reply {
        self.hello_with(v, addr, None)
    }

    /// Starts a session. `fixed` is the id of an in-game pack session (it says hello again every 30 s: then only
    /// its details are refreshed).
    fn hello_with(&self, v: &Value, addr: String, fixed: Option<String>) -> Reply {
        let s = |p: &str| v.pointer(p).and_then(|x| x.as_str()).unwrap_or("").chars().take(120).collect::<String>();
        let (plugin, version, mut world, world_id, server) = (s("/plugin/name"), s("/plugin/version"), s("/world/name"), s("/world/id"), s("/server/name"));
        let world_key = if !world_id.is_empty() { world_id.clone() } else if !world.is_empty() { world.clone() } else { "default".into() };
        if let Some(id) = &fixed {
            let mut i = self.inner.lock().unwrap();
            if let Some(sess) = i.sessions.get_mut(id) {
                sess.last = Instant::now();
                sess.last_ms = now_ms();
                if !plugin.is_empty() {
                    (sess.plugin, sess.version, sess.server) = (plugin.clone(), version.clone(), server.clone());
                }
                let relink = sess.world_key != world_key && !world_id.is_empty();
                sess.world_key = world_key.clone();
                if !relink {
                    return (200, json!({ "ok": true, "session": id }));
                }
            }
        }
        let maps = built_maps(&self.maps_dir);
        // The in-game pack runs in the world being played on this PC: the world whose files changed last.
        let playing = fixed.as_ref().and_then(|_| self.playing()).filter(|(_, slug, built)| *built && maps.iter().any(|(m, _)| m == slug)).map(|(_, slug, _)| slug);
        let mut i = self.inner.lock().unwrap();
        // Same world as before: the link the user chose last time. Otherwise a map whose world has the same name,
        // or (in-game pack) the map of the world being played.
        let link = i
            .config
            .links
            .get(&world_key)
            .filter(|slug| maps.iter().any(|(m, _)| m == *slug))
            .cloned()
            .or_else(|| (!world.is_empty()).then(|| maps.iter().find(|(_, n)| n.eq_ignore_ascii_case(&world)).map(|(m, _)| m.clone())).flatten())
            .or(playing);
        if fixed.is_some() && world.is_empty() {
            if let Some(slug) = &link {
                world = maps.iter().find(|(m, _)| m == slug).map(|(_, n)| n.clone()).unwrap_or_default();
                if world_key != "unknown" && world_key != "default" {
                    i.config.links.insert(world_key.clone(), slug.clone());
                }
            }
        }
        if fixed.as_ref().map_or(false, |id| i.sessions.contains_key(id)) {
            let id = fixed.unwrap();
            let sess = i.sessions.get_mut(&id).unwrap();
            (sess.link, sess.world) = (link, world);
            return (200, json!({ "ok": true, "session": id }));
        }
        i.seq += 1;
        let id = fixed.unwrap_or_else(|| format!("s-{}{:04x}", &new_key()[..12], i.seq & 0xffff));
        let label = if plugin.is_empty() { "A plugin".to_string() } else { format!("{} {}", plugin, version).trim().to_string() };
        let wl = if world.is_empty() { String::new() } else { format!(" for world \"{}\"", world) };
        i.say("connect", format!("{} connected from {}{}.", label, addr, wl));
        if link.is_none() {
            i.say("info", "No map is linked to it yet. Pick one on the Live link tab so chunk updates can be drawn.".into());
        }
        let link_json = link.as_ref().map(|slug| json!({ "slug": slug, "name": maps.iter().find(|(m, _)| m == slug).map(|(_, n)| n.clone()).unwrap_or_default() }));
        let t = now_ms();
        i.sessions.insert(
            id.clone(),
            Session { id: id.clone(), plugin, version, world, world_key, server, addr, started: t, last: Instant::now(), last_ms: t, link, players: vec![], world_state: json!({}), messages: 0, chunks: 0, dropped_chunks: 0, auto_done: false },
        );
        (200, json!({ "ok": true, "session": id, "link": link_json, "settings": { "playersEveryMs": 1000, "maxMessages": MAX_MESSAGES } }))
    }

    fn push(&self, v: &Value) -> Reply {
        let sid = v.get("session").and_then(|s| s.as_str()).unwrap_or("");
        match v.get("messages").and_then(|m| m.as_array()) {
            Some(m) => self.push_to(sid, m),
            None => err(400, "messages must be a list."),
        }
    }

    fn push_to(&self, sid: &str, msgs: &[Value]) -> Reply {
        if msgs.len() > MAX_MESSAGES {
            return err(413, "Too many messages in one request.");
        }
        let mut i = self.inner.lock().unwrap();
        let Inner { sessions, queue, log, .. } = &mut *i;
        let s = match sessions.get_mut(sid) {
            Some(s) => s,
            None => return err(410, "Unknown session. Send hello again."),
        };
        s.last = Instant::now();
        s.last_ms = now_ms();
        let (mut accepted, mut ignored, mut errors) = (0usize, 0usize, Vec::<String>::new());
        let mut lines: Vec<(String, String)> = Vec::new();
        for (n, m) in msgs.iter().enumerate() {
            let ty = m.get("type").and_then(|t| t.as_str()).unwrap_or("");
            let r: Result<(), String> = match ty {
                "players" => match m.get("players").and_then(|p| p.as_array()) {
                    None => Err("players must be a list".into()),
                    Some(list) => {
                        let fresh: Vec<Player> = list.iter().filter_map(Player::from_json).collect();
                        if fresh.len() != list.len() {
                            errors.push(format!("message {} (players): {} players were missing id, name, dim, x, y or z", n, list.len() - fresh.len()));
                        }
                        for p in &fresh {
                            if !s.players.iter().any(|q| q.id == p.id) {
                                lines.push(("join".into(), format!("{} is online.", p.name)));
                            }
                        }
                        for q in &s.players {
                            if !fresh.iter().any(|p| p.id == q.id) {
                                lines.push(("leave".into(), format!("{} went offline.", q.name)));
                            }
                        }
                        s.players = fresh;
                        Ok(())
                    }
                },
                "chunk" => Chunk::from_json(m).map(|c| match &s.link {
                    Some(slug) => {
                        s.chunks += 1;
                        queue.entry(slug.clone()).or_default().insert((c.dim, c.cx, c.cz), c);
                    }
                    None => s.dropped_chunks += 1,
                }),
                "event" => {
                    let g = |k: &str| m.get(k).and_then(|x| x.as_str()).unwrap_or("").chars().take(300).collect::<String>();
                    let (kind, player, text) = (g("kind"), g("player"), g("text"));
                    let line = match kind.as_str() {
                        "chat" => format!("<{}> {}", player, text),
                        _ if !player.is_empty() && !text.is_empty() => format!("{}: {}", player, text),
                        _ if !text.is_empty() => text,
                        _ => format!("{} {}", player, kind).trim().to_string(),
                    };
                    lines.push((if kind.is_empty() { "event".into() } else { kind }, line));
                    Ok(())
                }
                "world" => {
                    if let (Some(o), Some(cur)) = (m.as_object(), s.world_state.as_object_mut()) {
                        for (k, val) in o.iter().filter(|(k, _)| k.as_str() != "type") {
                            cur.insert(k.clone(), val.clone());
                        }
                    }
                    Ok(())
                }
                _ => {
                    ignored += 1;
                    continue;
                }
            };
            match r {
                Ok(()) => accepted += 1,
                Err(e) => {
                    if errors.len() < 20 {
                        errors.push(format!("message {} ({}): {}", n, ty, e));
                    }
                }
            }
        }
        s.messages += msgs.len() as u64;
        for (kind, text) in lines {
            log.push_back(LogLine { t: now_ms(), kind, text });
        }
        while log.len() > LOG_LINES {
            log.pop_front();
        }
        (200, json!({ "ok": true, "accepted": accepted, "ignored": ignored, "errors": errors }))
    }

    pub fn set_app(&self, app: AppHandle) {
        let _ = self.app.set(app);
    }

    /// The world being played on this PC, its map slug, and whether that map is built.
    fn playing(&self) -> Option<(crate::worlds::WorldRef, String, bool)> {
        let app = self.app.get()?;
        crate::panel::playing_world(&app.state::<AppState>())
    }

    fn set_auto(&self, v: Value) {
        self.inner.lock().unwrap().auto = Some(v);
    }

    /// Links a session to a map and remembers it for the session's world.
    fn link_session(&self, sid: &str, slug: &str, world_name: &str) {
        {
            let mut i = self.inner.lock().unwrap();
            let Some(s) = i.sessions.get_mut(sid) else { return };
            s.link = Some(slug.to_string());
            if s.world.is_empty() {
                s.world = world_name.to_string();
            }
            let key = s.world_key.clone();
            if key != "unknown" && key != "default" {
                i.config.links.insert(key, slug.to_string());
            }
        }
        self.save();
    }

    /// The in-game pack reported a world: open its map window, building the map first when there is none.
    /// Done once per session (the pack says hello every 30 s); a build that has to wait for another one is retried.
    fn world_opened(self: &Arc<Self>, sid: &str) {
        let link = {
            let i = self.inner.lock().unwrap();
            match i.sessions.get(sid) {
                Some(s) if i.config.auto_open && !s.auto_done => s.link.clone(),
                _ => return,
            }
        };
        let Some(app) = self.app.get().cloned() else { return };
        let done = |me: &Live| {
            if let Some(s) = me.inner.lock().unwrap().sessions.get_mut(sid) {
                s.auto_done = true;
            }
        };
        let open = |me: &Live, slug: &str| match crate::panel::open_map_window(&app, &me.maps_dir, slug, false) {
            Ok(()) => me.inner.lock().unwrap().say("info", "Opened the map of the world you are playing.".into()),
            Err(e) => me.inner.lock().unwrap().say("error", format!("Could not open the map: {}", e)),
        };
        if let Some(slug) = link {
            done(self);
            open(self, &slug);
            return;
        }
        let Some((world, slug, built)) = self.playing() else {
            done(self);
            self.inner.lock().unwrap().say("info", "A world with the pack is open, but the app could not tell which one. Pick its map under Connected plugins.".into());
            return;
        };
        if built {
            done(self);
            self.link_session(sid, &slug, &world.name);
            open(self, &slug);
            return;
        }
        let st = app.state::<AppState>();
        let job = match crate::panel::begin_build(&st, world.id.clone(), None, Some(true)) {
            Ok(j) => j,
            Err(e) if e.contains("already being built") => {
                self.set_auto(json!({ "world": world.name, "status": "waiting", "stage": "Waiting for another map build to finish" }));
                return; // try again at the pack's next hello
            }
            Err(e) => {
                done(self);
                self.set_auto(json!({ "world": world.name, "status": "error", "error": e }));
                self.inner.lock().unwrap().say("error", format!("Could not build the map of \"{}\": {}", world.name, e));
                return;
            }
        };
        done(self);
        self.inner.lock().unwrap().say("info", format!("Building the map of \"{}\" (the world you opened). It opens when it is ready.", world.name));
        let (me, sid, name) = (self.clone(), sid.to_string(), world.name.clone());
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_millis(500));
            let Some(j) = crate::panel::job(&app.state::<AppState>(), &job.id) else { return };
            me.set_auto(json!({ "world": name, "slug": slug, "status": j.status, "stage": j.stage, "n": j.n, "total": j.total, "dimIndex": j.dim_index, "dimCount": j.dim_count, "error": j.error }));
            match j.status.as_str() {
                "done" => {
                    me.inner.lock().unwrap().say("info", format!("The map of \"{}\" is built.", name));
                    me.link_session(&sid, &slug, &name);
                    match crate::panel::open_map_window(&app, &me.maps_dir, &slug, false) {
                        Ok(()) => me.inner.lock().unwrap().say("info", "Opened the map of the world you are playing.".into()),
                        Err(e) => me.inner.lock().unwrap().say("error", format!("Could not open the map: {}", e)),
                    }
                    return;
                }
                "error" => {
                    let e = j.error.unwrap_or_default();
                    me.inner.lock().unwrap().say("error", format!("Building the map of \"{}\" failed: {}", name, e));
                    return;
                }
                _ => {}
            }
        });
    }

    /// One line from the in-game pack (read from the content log by live_game.rs):
    /// `{ "s": pack session, "t": "hello" | "push" | "bye", "d": hello body, "m": messages }`.
    pub fn game_line(self: &Arc<Self>, v: &Value) {
        let Some(s) = v.get("s").and_then(|s| s.as_str()).filter(|s| !s.is_empty() && s.len() < 64) else { return };
        let id = format!("g-{}", s);
        let known = self.inner.lock().unwrap().sessions.contains_key(&id);
        match v.get("t").and_then(|t| t.as_str()).unwrap_or("") {
            "hello" => {
                let empty = json!({});
                self.hello_with(v.get("d").unwrap_or(&empty), "this PC (in-game pack)".into(), Some(id.clone()));
                self.save();
                self.world_opened(&id);
            }
            "push" => {
                if !known {
                    // The app started after the world: carry on until the pack's next hello fills in the details.
                    self.hello_with(&json!({ "plugin": { "name": "Bedrock Map Live" }, "server": { "name": "In-game pack" } }), "this PC (in-game pack)".into(), Some(id.clone()));
                }
                if let Some(m) = v.get("m").and_then(|m| m.as_array()) {
                    let _ = self.push_to(&id, m);
                }
            }
            "bye" => {
                let _ = self.bye(&json!({ "session": id }));
            }
            _ => {}
        }
    }

    fn bye(&self, v: &Value) -> Reply {
        let sid = v.get("session").and_then(|s| s.as_str()).unwrap_or("");
        let mut i = self.inner.lock().unwrap();
        match i.sessions.remove(sid) {
            Some(s) => {
                i.say("disconnect", format!("{} disconnected.", if s.plugin.is_empty() { "A plugin" } else { s.plugin.as_str() }));
                (200, json!({ "ok": true }))
            }
            None => err(410, "Unknown session."),
        }
    }

    /// Once a second: end silent sessions, and draw queued chunks into their maps (unless a build is running).
    fn tick(&self) {
        let work: Vec<(String, Vec<Chunk>)> = {
            let mut i = self.inner.lock().unwrap();
            let gone: Vec<String> = i.sessions.values().filter(|s| s.last.elapsed() > DROP).map(|s| s.id.clone()).collect();
            for id in gone {
                if let Some(s) = i.sessions.remove(&id) {
                    i.say("disconnect", format!("{} stopped sending and was disconnected.", if s.plugin.is_empty() { "A plugin" } else { s.plugin.as_str() }));
                }
            }
            if *self.building.lock().unwrap() {
                return;
            }
            i.queue.drain().map(|(slug, m)| (slug, m.into_values().collect())).filter(|(_, v): &(String, Vec<Chunk>)| !v.is_empty()).collect()
        };
        for (slug, chunks) in work {
            let r = live_tiles::apply(&self.maps_dir.join(&slug), &chunks);
            let mut i = self.inner.lock().unwrap();
            match r {
                Ok(done) => {
                    i.totals.1 += chunks.len() as u64;
                    i.rev += 1;
                    let rev = i.rev;
                    let revs = i.revs.entry(slug.clone()).or_default();
                    for f in done.files {
                        revs.insert(f, rev);
                    }
                    if done.meta {
                        i.meta_rev.insert(slug.clone(), rev);
                    }
                }
                Err(e) => i.say("error", format!("Could not draw {} chunks into {}: {}", chunks.len(), slug, e)),
            }
        }
    }

    /// What a map window needs: players of sessions linked to this map, and revisions of redrawn files.
    pub fn map_json(&self, slug: &str) -> Value {
        let i = self.inner.lock().unwrap();
        let linked: Vec<&Session> = i.sessions.values().filter(|s| s.link.as_deref() == Some(slug)).collect();
        let players: Vec<&Player> = linked.iter().flat_map(|s| s.players.iter()).collect();
        let world = linked.iter().map(|s| s.world_state.clone()).find(|w| w.as_object().map_or(false, |o| !o.is_empty()));
        json!({
            "active": !linked.is_empty(),
            "quiet": !linked.is_empty() && linked.iter().all(|s| s.last.elapsed() > QUIET),
            "players": players,
            "world": world,
            "revs": i.revs.get(slug).cloned().unwrap_or_default(),
            "metaRev": i.meta_rev.get(slug).copied().unwrap_or(0)
        })
    }

    /// Everything the Live link tab shows.
    fn status_json(&self) -> Value {
        let maps = built_maps(&self.maps_dir);
        let game = live_game::state_json(&self.game.lock().unwrap(), &self.data_dir);
        let i = self.inner.lock().unwrap();
        let c = &i.config;
        let mut urls = vec![format!("http://127.0.0.1:{}/v1", c.port)];
        if c.allow_lan {
            if let Some(ip) = lan_ip() {
                urls.push(format!("http://{}:{}/v1", ip, c.port));
            }
        }
        let mut sessions: Vec<&Session> = i.sessions.values().collect();
        sessions.sort_by_key(|s| s.started);
        let players: Vec<Value> = sessions
            .iter()
            .flat_map(|s| s.players.iter().map(move |p| json!({ "session": s.id, "name": p.name, "dim": p.dim, "x": p.x, "y": p.y, "z": p.z, "health": p.health })))
            .collect();
        json!({
            "config": { "enabled": c.enabled, "port": c.port, "allowLan": c.allow_lan, "key": c.key, "autoOpen": c.auto_open },
            "auto": i.auto,
            "status": i.status, "detail": i.detail, "urls": urls, "protocol": PROTOCOL,
            "sessions": sessions.iter().map(|s| s.to_json()).collect::<Vec<_>>(),
            "players": players,
            "log": i.log.iter().rev().take(120).cloned().collect::<Vec<_>>(),
            "maps": maps.iter().map(|(slug, name)| json!({ "slug": slug, "name": name })).collect::<Vec<_>>(),
            "totals": { "requests": i.totals.0, "chunksDrawn": i.totals.1, "rejected": i.totals.2 },
            "game": game,
            "queued": i.queue.values().map(|q| q.len()).sum::<usize>()
        })
    }
}

/* ---- commands for the control panel ---- */

use crate::panel::AppState;

#[tauri::command]
pub fn live_status(state: State<AppState>) -> Value {
    state.live.status_json()
}

#[tauri::command]
pub fn live_configure(state: State<AppState>, enabled: bool, port: u32, allow_lan: bool) -> Result<Value, String> {
    if !(1024..=65535).contains(&port) {
        return Err("The port must be a number from 1024 to 65535.".into());
    }
    let live = &state.live;
    {
        let mut i = live.inner.lock().unwrap();
        i.config.enabled = enabled;
        i.config.port = port as u16;
        i.config.allow_lan = allow_lan;
    }
    live.save();
    live.restart();
    Ok(live.status_json())
}

#[tauri::command]
pub fn live_new_key(state: State<AppState>) -> Value {
    {
        let mut i = state.live.inner.lock().unwrap();
        i.config.key = new_key();
        i.say("info", "Made a new plugin key. Plugins using the old key are refused until they get the new one.".into());
    }
    state.live.save();
    state.live.status_json()
}

/// Links a connected plugin's world to a built map (or unlinks it with slug = null), and remembers the choice.
#[tauri::command]
pub fn live_link(state: State<AppState>, session: String, slug: Option<String>) -> Result<Value, String> {
    let live = &state.live;
    if let Some(s) = &slug {
        if !built_maps(&live.maps_dir).iter().any(|(m, _)| m == s) {
            return Err("That map no longer exists. Refresh the list.".into());
        }
    }
    {
        let mut i = live.inner.lock().unwrap();
        let s = i.sessions.get_mut(&session).ok_or("That plugin is no longer connected.")?;
        s.link = slug.clone();
        let key = s.world_key.clone();
        match &slug {
            Some(m) => {
                i.config.links.insert(key, m.clone());
            }
            None => {
                i.config.links.remove(&key);
            }
        }
        let msg = match &slug {
            Some(m) => format!("Linked to map {}. New chunk updates will be drawn into it.", m),
            None => "Unlinked. Chunk updates are dropped until a map is picked.".into(),
        };
        i.say("info", msg);
    }
    live.save();
    Ok(live.status_json())
}

/// Writes the in-game pack and opens it, so Minecraft imports it (or shows it in the file manager with reveal).
#[tauri::command]
pub fn live_pack(state: State<AppState>, reveal: bool) -> Result<Value, String> {
    let p = live_game::write_pack(&state.live.data_dir)?;
    live_game::open_path(&p, reveal)?;
    let msg = if reveal { format!("Saved the pack as {}.", p.display()) } else { "Opened the pack: Minecraft imports it (it starts if it is closed).".to_string() };
    state.live.inner.lock().unwrap().say("info", msg);
    Ok(state.live.status_json())
}

/// Turns the automatic build-and-open (when the in-game pack reports a world) on or off.
#[tauri::command]
pub fn live_set_auto_open(state: State<AppState>, on: bool) -> Value {
    state.live.inner.lock().unwrap().config.auto_open = on;
    state.live.save();
    state.live.status_json()
}

#[tauri::command]
pub fn live_enable_content_log(state: State<AppState>) -> Result<Value, String> {
    let n = live_game::enable_content_log()?;
    state.live.inner.lock().unwrap().say("info", if n > 0 { "Turned on Content Log File in Minecraft's settings.".into() } else { "Content Log File was already on.".to_string() });
    Ok(state.live.status_json())
}

#[tauri::command]
pub fn live_disconnect(state: State<AppState>, session: String) -> Value {
    {
        let mut i = state.live.inner.lock().unwrap();
        if i.sessions.remove(&session).is_some() {
            i.say("disconnect", "Disconnected a plugin. It will reconnect if it is still running.".into());
        }
    }
    state.live.status_json()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_random_hex() {
        let (a, b) = (new_key(), new_key());
        assert_eq!(a.len(), 32);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
        assert!(same_key(&a, &a) && !same_key(&a, &b) && !same_key(&a, ""));
    }

    #[test]
    fn players_need_the_required_fields() {
        let ok = Player::from_json(&json!({ "id": 7, "name": "Steve", "dim": 0, "x": 1.5, "y": 64, "z": -2 })).unwrap();
        assert_eq!((ok.id.as_str(), ok.y), ("7", 64.0));
        assert!(Player::from_json(&json!({ "id": "a", "name": "Alex", "dim": 0, "x": 1 })).is_none());
    }
}
