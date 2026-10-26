//! Renders a world into a map folder (tiles, meta.json and a copy of the map page).
use crate::colors::unknown_blocks;
use crate::render::{render_dimension, Opts};
use crate::viewer;
use crate::world::{dim_name, World};
use crate::worlds::resolve_world;
use serde_json::{json, Map, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// Progress events, same as the old JSON lines.
pub enum Event {
    Stage(String),
    Dim { name: String, chunks: usize, index: usize, count: usize },
    Tile { n: usize, total: usize },
    Note(String),
}

pub fn safe_name(s: &str) -> String {
    let mut out = String::new();
    let mut in_bad = false;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == ' ' {
            out.push(c);
            in_bad = false;
        } else if !in_bad {
            out.push('_');
            in_bad = true;
        }
    }
    let t = out.trim().to_string();
    if t.is_empty() {
        "world".into()
    } else {
        t
    }
}

/// Current time as an ISO 8601 UTC string, e.g. 2026-10-21T14:50:46.736Z.
pub fn iso_now() -> String {
    let d = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = d.as_secs() as i64;
    let ms = d.subsec_millis();
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    // civil-from-days (Howard Hinnant)
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { y + 1 } else { y };
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z", year, month, day, rem / 3600, (rem % 3600) / 60, rem % 60, ms)
}

pub fn copy_viewer(out: &Path) -> Result<(), String> {
    fs::create_dir_all(out).map_err(|e| e.to_string())?;
    for (name, bytes) in viewer::FILES {
        fs::write(out.join(name), bytes).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn say(ev: Event, emit: &mut dyn FnMut(Event), notes: &mut Vec<String>) {
    if let Event::Note(m) = &ev {
        notes.push(m.clone());
    }
    emit(ev);
}

pub struct BuildArgs {
    pub input: PathBuf,
    pub out: PathBuf,
    pub dims: Option<Vec<String>>,
    pub max_y: Option<i32>,
    /// "skip" or a number as text
    pub nether_roof: String,
    /// Also draw the cave layer for the overworld.
    pub caves: bool,
}

pub fn build_map(args: BuildArgs, emit: &mut dyn FnMut(Event)) -> Result<(PathBuf, Value), String> {
    let started = Instant::now();
    let mut notes: Vec<String> = Vec::new();
    let (dir, _temp) = resolve_world(&args.input)?; // _temp deletes the unpacked files when we return
    say(Event::Stage("Reading the world database".into()), emit, &mut notes);
    let world = World::open(&dir)?;
    let info = world.info();
    let out = args.out.clone();
    let want: Option<Vec<String>> = args.dims.as_ref().map(|d| d.iter().map(|s| s.trim().to_lowercase()).collect());

    let mut meta = Map::new();
    meta.insert("name".into(), json!(info.name));
    meta.insert("generated".into(), json!(iso_now()));
    meta.insert("spawn".into(), info.spawn.clone());
    meta.insert("player".into(), info.player.clone());
    meta.insert("details".into(), info.details.clone());
    let mut dims_meta = Map::new();
    let mut dim_stats: Vec<(String, crate::render::Stats)> = Vec::new();

    let mut ids: Vec<i32> = world.dims.keys().cloned().collect();
    ids.sort();
    let list: Vec<i32> = ids.into_iter().filter(|id| want.as_ref().map_or(true, |w| w.contains(&dim_name(*id)))).collect();
    let count = list.len();
    for (i, id) in list.iter().enumerate() {
        let name = dim_name(*id);
        let chunks = world.dims[id].chunks.len();
        say(Event::Dim { name: name.clone(), chunks, index: i + 1, count }, emit, &mut notes);
        let opts = if *id == 1 {
            match args.nether_roof.as_str() {
                "skip" => Opts { max_y: 127, skip_roof: true, caves: false },
                n => Opts { max_y: n.parse::<f64>().map_err(|_| "The height limit must be a number.".to_string())? as i32, skip_roof: false, caves: false },
            }
        } else {
            Opts { max_y: args.max_y.unwrap_or(319), skip_roof: false, caves: args.caves && *id == 0 }
        };
        let mut on_tile = |n: usize, total: usize| emit(Event::Tile { n, total });
        let r = render_dimension(&world, *id, &name, &out, &opts, &mut on_tile)?;
        dims_meta.insert(name.clone(), r.meta);
        dim_stats.push((name, r.stats));
    }
    if dims_meta.is_empty() {
        return Err("No chunks found to draw. Is this the right world, and has it been explored?".into());
    }
    meta.insert("dims".into(), Value::Object(dims_meta));
    fs::create_dir_all(&out).map_err(|e| e.to_string())?;
    copy_viewer(&out)?;

    let st = &world.db.stats;
    let mut line = format!("Read {} table files and {} log files", st.tables, st.logs);
    if st.tables_failed > 0 || st.blocks_failed > 0 {
        line += &format!(" ({} files and {} data blocks could not be read)", st.tables_failed, st.blocks_failed);
    }
    line += &format!("; {} chunk pieces found.", world.db.len());
    say(Event::Note(line), emit, &mut notes);
    for (name, x) in &dim_stats {
        let mut m = format!("{}: {} chunks in the world file, {} had nothing readable to draw", name, x.chunks, x.blank);
        if x.partial > 0 {
            m += &format!(", {} only partly", x.partial);
        }
        m.push('.');
        say(Event::Note(m), emit, &mut notes);
    }
    for w in world.warnings.iter().take(6) {
        say(Event::Note(w.clone()), emit, &mut notes);
    }
    let bad = world.bad_subchunks.load(Ordering::Relaxed);
    if bad > 0 {
        let reasons: Vec<String> = world.bad_reasons.lock().unwrap().iter().take(4).map(|(r, n)| format!("{} ({})", r, n)).collect();
        say(Event::Note(format!("{} subchunks could not be read: {}", bad, reasons.join("; "))), emit, &mut notes);
    }
    let unk = unknown_blocks();
    if !unk.is_empty() {
        let shown: Vec<&str> = unk.iter().take(8).map(|s| s.as_str()).collect();
        say(
            Event::Note(format!(
                "{} block types have no set colour and use a made-up one ({}{}).",
                unk.len(),
                shown.join(", "),
                if unk.len() > 8 { ", ..." } else { "" }
            )),
            emit,
            &mut notes,
        );
    }
    // The build report travels with the map so the control panel and map page can show it later.
    meta.insert(
        "build".into(),
        json!({
            "seconds": (started.elapsed().as_secs_f64() * 10.0).round() / 10.0,
            "maxY": args.max_y,
            "netherRoof": args.nether_roof,
            "caves": args.caves,
            "subchunks": world.subchunks_read.load(Ordering::Relaxed),
            "unknownBlocks": unk.len(),
            "notes": notes.iter().take(20).collect::<Vec<_>>()
        }),
    );
    let meta = Value::Object(meta);
    fs::write(out.join("meta.json"), serde_json::to_string_pretty(&meta).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    Ok((out, meta))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_names() {
        assert_eq!(safe_name("Hayden and boss"), "Hayden and boss");
        assert_eq!(safe_name("a:b?c"), "a_b_c");
        assert_eq!(safe_name("???"), "_");
        assert_eq!(safe_name(""), "world");
    }

    #[test]
    fn iso_time_has_the_right_shape() {
        let t = iso_now();
        assert_eq!(t.len(), 24);
        assert!(t.ends_with('Z') && &t[10..11] == "T");
    }
}
