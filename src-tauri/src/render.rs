use crate::colors::{classify, Class, WATER};
use crate::pngio;
use crate::world::World;
use serde_json::{json, Value};
use std::collections::{BTreeSet, HashMap};
use rayon::prelude::*;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc;

pub const TILE: usize = 512;
pub const TILE_CHUNKS: i32 = 32;
/// Height tiles store (Y + HEIGHT_BIAS) as a 16-bit number in the red and green channels; alpha 255 marks columns with data.
pub const HEIGHT_BIAS: i32 = 1024;
/// Open space only counts as a cave when it lies at least this many blocks below the surface,
/// so rooms inside houses and the gaps under trees are not mistaken for caves.
const CAVE_COVER: i32 = 8;

#[derive(Clone, Copy)]
pub struct Opts {
    pub max_y: i32,
    pub skip_roof: bool,
    /// Also draw the cave layer (overworld only).
    pub caves: bool,
}

#[derive(Default, Clone, Copy)]
pub struct Stats {
    pub chunks: usize,
    pub blank: usize,
    pub partial: usize,
}

/// Colours and surface heights for the 16x16 columns of one chunk.
pub struct ChunkData {
    rgb: [u8; 768],
    h: [i16; 256],
    st: [u8; 256], // 0 none, 1 water so far, 2 done
    is_water: [u8; 256],
    empty: usize,
    /// Open blocks below the surface, and the sum of their Y values (for the average depth).
    cave_cnt: [u16; 256],
    cave_sum: [i32; 256],
}

pub fn blend(a: f64, w: [u8; 3], under: [u8; 3]) -> [u8; 3] {
    let f = |i: usize| (w[i] as f64 * a + under[i] as f64 * (1.0 - a)).round() as u8;
    [f(0), f(1), f(2)]
}

/// The block that counts for the map at storage index `i`. A see-through block (kelp, seagrass, an
/// empty palette slot...) whose second storage holds water is water: that is how Bedrock saves
/// waterlogged blocks, and without this kelp reaching the surface would show the bare seafloor.
fn block_at(cls: &[Class], idx: &[u16], wet: Option<(&[bool], &[u16])>, i: usize) -> Option<Class> {
    let c = cls.get(idx[i] as usize).copied();
    if matches!(c, Some(c) if c.kind != 0) {
        return c;
    }
    if let Some((wet, idx1)) = wet {
        if wet.get(idx1[i] as usize).copied().unwrap_or(false) {
            return Some(Class { kind: 2, rgb: WATER, known: true });
        }
    }
    c
}

fn compute_chunk(world: &World, dim: i32, cx: i32, cz: i32, opts: &Opts) -> Option<ChunkData> {
    let entry = world.chunk(dim, cx, cz)?;
    let max_y = opts.max_y;
    let mut d = ChunkData { rgb: [0; 768], h: [0; 256], st: [0; 256], is_water: [0; 256], empty: 0, cave_cnt: [0; 256], cave_sum: [0; 256] };
    let mut surf_found = [false; 256];
    let mut surf_y = [0i32; 256];
    let mut prev_sy: Option<i8> = None;
    let mut depth = [0u16; 256];
    let mut seen_air = [false; 256];
    let mut fb = [0u8; 768];
    let mut fb_h = [i16::MIN; 256];
    let mut ys: Vec<i8> = entry.subs.keys().cloned().collect();
    ys.sort_by(|a, b| b.cmp(a));
    let mut open = 256i32;
    for sy in ys {
        let above = prev_sy;
        prev_sy = Some(sy);
        if (sy as i32) * 16 > max_y {
            continue;
        }
        // Whole sub-chunks that are missing from the file are empty air. Below the surface that is cave.
        if opts.caves {
            if let Some(a) = above {
                for m in (sy as i32 + 1)..(a as i32) {
                    for col in 0..256usize {
                        if !surf_found[col] {
                            continue;
                        }
                        for ly in 0..16i32 {
                            let y = m * 16 + ly;
                            if y <= max_y && surf_y[col] - y >= CAVE_COVER {
                                d.cave_cnt[col] += 1;
                                d.cave_sum[col] += y;
                            }
                        }
                    }
                }
            }
        }
        let sc = match world.decode_sub(entry, sy) {
            Some(s) => s,
            None => continue,
        };
        let cls: Vec<Class> = sc.palette.iter().map(|n| classify(n)).collect();
        let leaf: Vec<bool> = sc.palette.iter().map(|n| n.contains("leaves")).collect();
        let wet1: Option<Vec<bool>> = sc.layer1.as_ref().map(|l| l.palette.iter().map(|n| classify(n).kind == 2).collect());
        let wet: Option<(&[bool], &[u16])> = match (&wet1, &sc.layer1) {
            (Some(w), Some(l)) if w.iter().any(|x| *x) => Some((w.as_slice(), l.idx.as_slice())),
            _ => None,
        };
        for lx in 0..16usize {
            for lz in 0..16usize {
                let col = lz * 16 + lx;
                if d.st[col] == 2 {
                    continue;
                }
                for ly in (0..16usize).rev() {
                    let y = (sy as i32) * 16 + ly as i32;
                    if y > max_y {
                        continue;
                    }
                    let c = match block_at(&cls, &sc.idx, wet, lx * 256 + lz * 16 + ly) {
                        Some(c) if c.kind != 0 => c,
                        _ => {
                            seen_air[col] = true;
                            continue;
                        }
                    };
                    if opts.skip_roof && !seen_air[col] && d.st[col] == 0 {
                        if fb_h[col] == i16::MIN {
                            fb_h[col] = y as i16;
                            fb[col * 3..col * 3 + 3].copy_from_slice(&c.rgb);
                        }
                        continue;
                    }
                    if c.kind == 2 {
                        if d.st[col] == 0 {
                            d.st[col] = 1;
                            d.h[col] = y as i16;
                            d.is_water[col] = 1;
                        }
                        depth[col] += 1;
                        continue;
                    }
                    if d.st[col] == 1 {
                        let a = (0.45 + 0.03 * depth[col] as f64).min(0.92);
                        let m = blend(a, WATER, c.rgb);
                        d.rgb[col * 3..col * 3 + 3].copy_from_slice(&m);
                    } else {
                        d.rgb[col * 3..col * 3 + 3].copy_from_slice(&c.rgb);
                        d.h[col] = y as i16;
                    }
                    d.st[col] = 2;
                    open -= 1;
                    break;
                }
            }
        }
        if opts.caves {
            for lx in 0..16usize {
                for lz in 0..16usize {
                    let col = lz * 16 + lx;
                    for ly in (0..16usize).rev() {
                        let y = (sy as i32) * 16 + ly as i32;
                        if y > max_y {
                            continue;
                        }
                        let pi = sc.idx[lx * 256 + lz * 16 + ly] as usize;
                        let c = block_at(&cls, &sc.idx, wet, lx * 256 + lz * 16 + ly);
                        if !surf_found[col] {
                            // The surface is the first solid block from the top; leaves do not count.
                            if matches!(c, Some(c) if c.kind == 1) && !leaf.get(pi).copied().unwrap_or(false) {
                                surf_found[col] = true;
                                surf_y[col] = y;
                            }
                            continue;
                        }
                        let is_open = match c {
                            Some(c) => c.kind == 0,
                            None => true,
                        };
                        if is_open && surf_y[col] - y >= CAVE_COVER {
                            d.cave_cnt[col] += 1;
                            d.cave_sum[col] += y;
                        }
                    }
                }
            }
        }
        if open == 0 && !opts.caves {
            break;
        }
    }
    for col in 0..256 {
        if d.st[col] == 1 {
            let a = (0.55 + 0.03 * depth[col] as f64).min(0.97);
            let m = blend(a, WATER, [20, 20, 20]);
            d.rgb[col * 3..col * 3 + 3].copy_from_slice(&m);
            d.st[col] = 2;
        } else if d.st[col] == 0 && opts.skip_roof && fb_h[col] != i16::MIN {
            let src = [fb[col * 3], fb[col * 3 + 1], fb[col * 3 + 2]];
            d.rgb[col * 3..col * 3 + 3].copy_from_slice(&src);
            d.h[col] = fb_h[col];
            d.st[col] = 2;
        }
    }
    d.empty = d.st.iter().filter(|s| **s == 0).count();
    Some(d)
}

type ChunkCache = HashMap<(i32, i32), Option<Rc<ChunkData>>>;

fn get_chunk(cache: &mut ChunkCache, world: &World, dim: i32, cx: i32, cz: i32, opts: &Opts) -> Option<Rc<ChunkData>> {
    cache
        .entry((cx, cz))
        .or_insert_with(|| compute_chunk(world, dim, cx, cz, opts).map(Rc::new))
        .clone()
}

struct TileOut {
    rgba: Vec<u8>,
    heights: Vec<u8>,
    caves: Option<Vec<u8>>,
}

/// Cave pixel: hue says how deep (amber near the surface, purple deep down), strength says how much open space.
fn cave_pixel(cnt: u16, sum: i32) -> [u8; 4] {
    let mean = sum as f64 / cnt as f64;
    let t = ((mean + 64.0) / 128.0).clamp(0.0, 1.0);
    let deep = [150.0, 70.0, 230.0];
    let high = [255.0, 190.0, 70.0];
    let mix = |i: usize| (deep[i] + (high[i] - deep[i]) * t).round() as u8;
    let alpha = 110 + (cnt.min(24) as u32) * 6;
    [mix(0), mix(1), mix(2), alpha.min(255) as u8]
}

/// Renders one 512x512 RGBA tile, its height tile and (optionally) its cave tile. Returns None when the tile holds no chunks.
fn render_tile(world: &World, dim: i32, tx: i32, tz: i32, opts: &Opts, stats: &mut Stats) -> Option<TileOut> {
    // Runs on a worker thread; the chunk cache (and its Rc) never leaves this call.
    let mut cache: ChunkCache = HashMap::new();
    let mut out = vec![0u8; TILE * TILE * 4];
    let mut heights = vec![0u8; TILE * TILE * 4];
    let mut caves: Option<Vec<u8>> = if opts.caves { Some(vec![0u8; TILE * TILE * 4]) } else { None };
    let mut any = false;
    for a in 0..TILE_CHUNKS {
        for b in 0..TILE_CHUNKS {
            let cx = tx * TILE_CHUNKS + b;
            let cz = tz * TILE_CHUNKS + a;
            let ch = match get_chunk(&mut cache, world, dim, cx, cz, opts) {
                Some(c) => c,
                None => continue,
            };
            any = true;
            if ch.empty == 256 {
                stats.blank += 1;
            } else if ch.empty > 0 {
                stats.partial += 1;
            }
            let north = get_chunk(&mut cache, world, dim, cx, cz - 1, opts);
            for lz in 0..16usize {
                for lx in 0..16usize {
                    let col = lz * 16 + lx;
                    if ch.st[col] != 2 {
                        continue;
                    }
                    let mut f = 1.0f64;
                    if ch.is_water[col] == 0 {
                        let mut hn: Option<i16> = None;
                        if lz > 0 {
                            if ch.st[col - 16] == 2 {
                                hn = Some(ch.h[col - 16]);
                            }
                        } else if let Some(n) = &north {
                            if n.st[240 + lx] == 2 {
                                hn = Some(n.h[240 + lx]);
                            }
                        }
                        if let Some(hn) = hn {
                            let diff = (ch.h[col] as i32 - hn as i32).clamp(-4, 4);
                            f = 1.0 + 0.07 * diff as f64;
                        }
                    }
                    let px = b as usize * 16 + lx;
                    let py = a as usize * 16 + lz;
                    let o = (py * TILE + px) * 4;
                    for i in 0..3 {
                        out[o + i] = (ch.rgb[col * 3 + i] as f64 * f).round().clamp(0.0, 255.0) as u8;
                    }
                    out[o + 3] = 255;
                    let hv = ch.h[col] as i32 + HEIGHT_BIAS;
                    heights[o] = ((hv >> 8) & 255) as u8;
                    heights[o + 1] = (hv & 255) as u8;
                    heights[o + 3] = 255;
                    if let Some(cv) = caves.as_mut() {
                        if ch.cave_cnt[col] > 0 {
                            cv[o..o + 4].copy_from_slice(&cave_pixel(ch.cave_cnt[col], ch.cave_sum[col]));
                        }
                    }
                }
            }
        }
    }
    if any {
        Some(TileOut { rgba: out, heights, caves })
    } else {
        None
    }
}

/// Half-size version of four child tiles placed in a 2x2 square (any may be missing).
pub fn combine(children: &[Option<Vec<u8>>; 4]) -> Vec<u8> {
    let mut out = vec![0u8; TILE * TILE * 4];
    let half = TILE / 2;
    for q in 0..4usize {
        let c = match &children[q] {
            Some(c) => c,
            None => continue,
        };
        let ox = (q & 1) * half;
        let oy = (q >> 1) * half;
        for y in 0..half {
            for x in 0..half {
                let (mut r, mut g, mut b, mut a) = (0u32, 0u32, 0u32, 0u32);
                for dy in 0..2 {
                    for dx in 0..2 {
                        let i = ((y * 2 + dy) * TILE + x * 2 + dx) * 4;
                        let al = c[i + 3] as u32;
                        r += c[i] as u32 * al;
                        g += c[i + 1] as u32 * al;
                        b += c[i + 2] as u32 * al;
                        a += al;
                    }
                }
                let o = ((oy + y) * TILE + ox + x) * 4;
                if a > 0 {
                    out[o] = (r as f64 / a as f64).round() as u8;
                    out[o + 1] = (g as f64 / a as f64).round() as u8;
                    out[o + 2] = (b as f64 / a as f64).round() as u8;
                    out[o + 3] = (a as f64 / 4.0).round() as u8;
                }
            }
        }
    }
    out
}

pub fn tile_path(root: &Path, group: &str, dim_name: &str, z: u32, x: i32, y: i32) -> PathBuf {
    root.join(group).join(dim_name).join(z.to_string()).join(format!("{}_{}.png", x, y))
}

/// A thread pool for drawing: one thread per CPU core, minus one so the window stays responsive.
/// The environment variable BEDROCK_MAP_THREADS overrides the count (1 = draw on a single thread).
fn worker_pool() -> Result<rayon::ThreadPool, String> {
    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
    let threads = std::env::var("BEDROCK_MAP_THREADS")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or_else(|| cores.saturating_sub(1).max(1));
    rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .thread_name(|i| format!("map-draw-{}", i))
        .build()
        .map_err(|e| e.to_string())
}

/// Draws one base tile and writes its PNGs. Returns the tile position if it had anything to draw.
fn draw_and_save_tile(
    world: &World,
    dim: i32,
    dim_name: &str,
    out_dir: &Path,
    opts: &Opts,
    tx: i32,
    tz: i32,
) -> Result<(Option<(i32, i32)>, Stats), String> {
    let mut stats = Stats::default();
    let t = match render_tile(world, dim, tx, tz, opts, &mut stats) {
        Some(t) => t,
        None => return Ok((None, stats)),
    };
    pngio::write(&tile_path(out_dir, "tiles", dim_name, 0, tx, tz), TILE as u32, TILE as u32, &t.rgba)?;
    let hf = out_dir.join("heights").join(dim_name).join(format!("{}_{}.png", tx, tz));
    pngio::write(&hf, TILE as u32, TILE as u32, &t.heights)?;
    if let Some(c) = &t.caves {
        pngio::write(&tile_path(out_dir, "caves", dim_name, 0, tx, tz), TILE as u32, TILE as u32, c)?;
    }
    Ok((Some((tx, tz)), stats))
}

/// What renderDimension reports back for meta.json, plus the stats the build notes use.
pub struct DimResult {
    pub meta: Value,
    pub stats: Stats,
}

/// Renders every tile of one dimension and its zoom pyramid.
pub fn render_dimension(
    world: &World,
    dim: i32,
    dim_name: &str,
    out_dir: &Path,
    opts: &Opts,
    on_tile: &mut dyn FnMut(usize, usize),
) -> Result<DimResult, String> {
    let d = world.dims.get(&dim).ok_or("Unknown dimension.")?;
    let mut todo: BTreeSet<(i32, i32)> = BTreeSet::new();
    for c in d.chunks.values() {
        todo.insert((c.cx.div_euclid(TILE_CHUNKS), c.cz.div_euclid(TILE_CHUNKS)));
    }
    let mut stats = Stats { chunks: d.chunks.len(), blank: 0, partial: 0 };
    let pool_owned = worker_pool()?;
    let pool = &pool_owned;
    let tiles: Vec<(i32, i32)> = todo.into_iter().collect();
    let total = tiles.len();

    // Draw and save the base tiles on the worker pool. Workers send a tick per finished tile;
    // this thread turns the ticks into progress events (the callback itself stays on this thread).
    let (tick, ticks) = mpsc::channel::<()>();
    let drawn: Vec<(Option<(i32, i32)>, Stats)> = std::thread::scope(|s| {
        let job = s.spawn(move || {
            pool.install(|| {
                tiles
                    .par_iter()
                    .map_with(tick, |tick, &(tx, tz)| {
                        let r = draw_and_save_tile(world, dim, dim_name, out_dir, opts, tx, tz);
                        let _ = tick.send(());
                        r
                    })
                    .collect::<Result<Vec<_>, String>>()
            })
        });
        // Ends once every worker's sender is dropped, i.e. when the parallel loop is over (or failed).
        let mut n = 0usize;
        for () in ticks.iter() {
            n += 1;
            on_tile(n, total);
        }
        match job.join() {
            Ok(r) => r,
            Err(p) => std::panic::resume_unwind(p),
        }
    })?;
    let mut level: BTreeSet<(i32, i32)> = BTreeSet::new();
    for (t, st) in drawn {
        stats.blank += st.blank;
        stats.partial += st.partial;
        if let Some(t) = t {
            level.insert(t);
        }
    }
    let base: Vec<(i32, i32)> = level.iter().cloned().collect();
    let mut zoom = 0u32;
    let mut have: Vec<(u32, Vec<String>)> = vec![(0, level.iter().map(|(x, y)| format!("{},{}", x, y)).collect())];
    while level.len() > 1 && zoom < 8 {
        let next: BTreeSet<(i32, i32)> = level.iter().map(|&(x, y)| (x.div_euclid(2), y.div_euclid(2))).collect();
        let groups: &[&str] = if opts.caves { &["tiles", "caves"] } else { &["tiles"] };
        let jobs: Vec<((i32, i32), &str)> = next.iter().flat_map(|&t| groups.iter().map(move |g| (t, *g))).collect();
        pool.install(|| {
            jobs.par_iter().try_for_each(|&((a, b), group)| -> Result<(), String> {
                let mut kids: [Option<Vec<u8>>; 4] = [None, None, None, None];
                for q in 0..4i32 {
                    let x = a * 2 + (q & 1);
                    let y = b * 2 + (q >> 1);
                    if level.contains(&(x, y)) {
                        kids[q as usize] = Some(pngio::read(&tile_path(out_dir, group, dim_name, zoom, x, y))?);
                    }
                }
                pngio::write(&tile_path(out_dir, group, dim_name, zoom + 1, a, b), TILE as u32, TILE as u32, &combine(&kids))
            })
        })?;
        level = next;
        zoom += 1;
        have.push((zoom, level.iter().map(|(x, y)| format!("{},{}", x, y)).collect()));
    }
    let mut have_map = serde_json::Map::new();
    for (z, ids) in have {
        have_map.insert(z.to_string(), json!(ids));
    }
    let bounds = if base.is_empty() {
        Value::Null
    } else {
        let minx = base.iter().map(|t| t.0).min().unwrap();
        let maxx = base.iter().map(|t| t.0).max().unwrap();
        let minz = base.iter().map(|t| t.1).min().unwrap();
        let maxz = base.iter().map(|t| t.1).max().unwrap();
        let t = TILE as i64;
        json!({ "minX": minx as i64 * t, "maxX": (maxx as i64 + 1) * t, "minZ": minz as i64 * t, "maxZ": (maxz as i64 + 1) * t })
    };
    let meta = json!({
        "dim": dim, "heights": true, "caves": opts.caves, "maxZoom": zoom, "tileCount": base.len(), "have": Value::Object(have_map),
        "stats": { "chunks": stats.chunks, "blank": stats.blank, "partial": stats.partial },
        "bounds": bounds
    });
    Ok(DimResult { meta, stats })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kelp_in_water_counts_as_water() {
        let kelp = Class { kind: 0, rgb: [0, 0, 0], known: true };
        let stone = Class { kind: 1, rgb: [125, 125, 125], known: true };
        let cls = [kelp, stone];
        let idx = [0u16, 1];
        let wet = [false, true]; // layer 1 palette: air, water
        let idx1 = [1u16, 1];
        // kelp with water in layer 1 -> water
        assert_eq!(block_at(&cls, &idx, Some((&wet, &idx1)), 0).unwrap().kind, 2);
        // a solid waterlogged block stays solid
        assert_eq!(block_at(&cls, &idx, Some((&wet, &idx1)), 1).unwrap().kind, 1);
        // no layer 1 -> unchanged
        assert_eq!(block_at(&cls, &idx, None, 0).unwrap().kind, 0);
    }

    #[test]
    fn cave_colours_run_from_amber_to_purple() {
        let shallow = cave_pixel(10, 10 * 64);
        let deep = cave_pixel(10, 10 * -64);
        assert_eq!(&shallow[..3], &[255, 190, 70]);
        assert_eq!(&deep[..3], &[150, 70, 230]);
        assert!(cave_pixel(30, 0)[3] > cave_pixel(1, 0)[3]);
    }
}
