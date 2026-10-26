//! Draws live chunk updates into a built map: the colour and height tiles, then the zoomed-out tiles above them.
//! Colours, water and hill shading follow render.rs, so a redrawn chunk looks like a freshly built one.
use crate::colors::{classify, WATER};
use crate::pngio;
use crate::render::{blend, combine, tile_path, HEIGHT_BIAS, TILE, TILE_CHUNKS};
use crate::world::dim_name;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

/// The top of one column, as a plugin sends it.
#[derive(Clone, Debug, PartialEq)]
pub struct Column {
    pub y: i32,
    pub block: String,
    /// Water blocks on top of `block` (0: none). `y` is then the water surface.
    pub water: u32,
}

#[derive(Clone, Debug)]
pub struct Chunk {
    pub dim: i32,
    pub cx: i32,
    pub cz: i32,
    /// columns[z * 16 + x]; None leaves the column as it is.
    pub columns: Vec<Option<Column>>,
}

impl Chunk {
    /// Parses a `chunk` message (see docs/live-protocol.md): columns as objects `{y, block, water}`, or in the
    /// compact form as arrays `[y, index into blocks, water]` with a `blocks` list of names.
    pub fn from_json(m: &Value) -> Result<Chunk, String> {
        let int = |k: &str| m.get(k).and_then(|v| v.as_i64()).ok_or(format!("{} must be a whole number", k));
        let (dim, cx, cz) = (int("dim")?, int("cx")?, int("cz")?);
        if !(0..=2).contains(&dim) {
            return Err("dim must be 0, 1 or 2".into());
        }
        let cols = m.get("columns").and_then(|v| v.as_array()).ok_or("columns must be a list")?;
        if cols.len() != 256 {
            return Err("columns must have 256 entries".into());
        }
        let names: Vec<&str> = m.get("blocks").and_then(|v| v.as_array()).map(|a| a.iter().map(|n| n.as_str().unwrap_or("")).collect()).unwrap_or_default();
        let mut columns = Vec::with_capacity(256);
        for c in cols {
            if c.is_null() {
                columns.push(None);
                continue;
            }
            let (y, block, water) = match c.as_array() {
                Some(a) => {
                    let i = a.get(1).and_then(|v| v.as_u64()).ok_or("compact columns are [y, block index, water]")? as usize;
                    let block = *names.get(i).filter(|n| !n.is_empty()).ok_or("a column's block index is not in blocks")?;
                    (a.first().and_then(|v| v.as_i64()), block, a.get(2).and_then(|v| v.as_u64()))
                }
                None => (c.get("y").and_then(|v| v.as_i64()), c.get("block").and_then(|v| v.as_str()).ok_or("every column needs a block")?, c.get("water").and_then(|v| v.as_u64())),
            };
            let y = y.ok_or("every column needs a whole-number y")?;
            let water = water.unwrap_or(0).min(400) as u32;
            if !(-2048..=2048).contains(&y) {
                return Err("y is out of range".into());
            }
            columns.push(Some(Column { y: y as i32, block: block.to_string(), water }));
        }
        Ok(Chunk { dim: dim as i32, cx: cx as i32, cz: cz as i32, columns })
    }
}

/// What changed on disk: revision keys for the viewer ("tiles/overworld/0/-3_4", "heights/overworld/-3_4"),
/// and whether meta.json changed (new tiles).
#[derive(Default, Debug)]
pub struct Applied {
    pub files: Vec<String>,
    pub meta: bool,
}

/// Colour and height of one column, or None for a see-through block (the plugin should not send those).
fn paint(col: &Column, north: Option<i32>) -> Option<[u8; 3]> {
    let c = classify(&col.block);
    if c.kind == 2 || col.water > 0 {
        let depth = col.water.max(1) as f64;
        // Same blend as render.rs: water over the block below, or deep water over a dark bed.
        return Some(if c.kind == 2 { blend((0.55 + 0.03 * depth).min(0.97), WATER, [20, 20, 20]) } else { blend((0.45 + 0.03 * depth).min(0.92), WATER, c.rgb) });
    }
    if c.kind == 0 {
        return None;
    }
    let f = north.map_or(1.0, |hn| 1.0 + 0.07 * (col.y - hn).clamp(-4, 4) as f64);
    Some(c.rgb.map(|v| (v as f64 * f).round().clamp(0.0, 255.0) as u8))
}

fn height_at(h: &[u8], o: usize) -> Option<i32> {
    (h[o + 3] == 255).then(|| (((h[o] as i32) << 8) | h[o + 1] as i32) - HEIGHT_BIAS)
}

fn read_or_blank(p: &Path) -> (Vec<u8>, bool) {
    match pngio::read(p) {
        Ok(v) if v.len() == TILE * TILE * 4 => (v, false),
        _ => (vec![0u8; TILE * TILE * 4], true),
    }
}

fn have_list<'a>(dm: &'a mut Value, z: u32) -> &'a mut Vec<Value> {
    if !dm["have"].is_object() {
        dm["have"] = json!({});
    }
    let have = dm["have"].as_object_mut().unwrap();
    have.entry(z.to_string()).or_insert_with(|| json!([])).as_array_mut().unwrap()
}

/// Draws the chunks into the map folder `map_dir`.
pub fn apply(map_dir: &Path, chunks: &[Chunk]) -> Result<Applied, String> {
    let meta_path = map_dir.join("meta.json");
    let mut meta: Value = serde_json::from_str(&fs::read_to_string(&meta_path).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    let mut out = Applied::default();
    let mut by_tile: BTreeMap<(i32, i32, i32), Vec<&Chunk>> = BTreeMap::new();
    for c in chunks {
        by_tile.entry((c.dim, c.cx.div_euclid(TILE_CHUNKS), c.cz.div_euclid(TILE_CHUNKS))).or_default().push(c);
    }
    let mut dirty: BTreeMap<String, BTreeSet<(i32, i32)>> = BTreeMap::new();
    for ((dim, tx, tz), list) in by_tile {
        let name = dim_name(dim);
        if !meta["dims"].is_object() {
            meta["dims"] = json!({});
        }
        if meta["dims"].get(&name).is_none() {
            meta["dims"][&name] = json!({ "dim": dim, "heights": true, "caves": false, "maxZoom": 0, "tileCount": 0, "have": { "0": [] }, "stats": { "chunks": 0, "blank": 0, "partial": 0 }, "bounds": null });
            out.meta = true;
        }
        let tp = tile_path(map_dir, "tiles", &name, 0, tx, tz);
        let hp = map_dir.join("heights").join(&name).join(format!("{}_{}.png", tx, tz));
        let (mut rgba, new) = read_or_blank(&tp);
        let (mut hts, _) = if new { (vec![0u8; TILE * TILE * 4], true) } else { read_or_blank(&hp) };
        for c in list {
            let ox = (c.cx.rem_euclid(TILE_CHUNKS) * 16) as usize;
            let oz = (c.cz.rem_euclid(TILE_CHUNKS) * 16) as usize;
            // Row by row from the north, so each column's north neighbour is already up to date (hill shading).
            for lz in 0..16usize {
                for lx in 0..16usize {
                    let col = match &c.columns[lz * 16 + lx] {
                        Some(col) => col,
                        None => continue,
                    };
                    let (px, py) = (ox + lx, oz + lz);
                    let o = (py * TILE + px) * 4;
                    let north = if py > 0 { height_at(&hts, o - TILE * 4) } else { None };
                    let rgb = match paint(col, north) {
                        Some(rgb) => rgb,
                        None => continue,
                    };
                    rgba[o..o + 3].copy_from_slice(&rgb);
                    rgba[o + 3] = 255;
                    let hv = col.y + HEIGHT_BIAS;
                    hts[o] = ((hv >> 8) & 255) as u8;
                    hts[o + 1] = (hv & 255) as u8;
                    hts[o + 2] = 0;
                    hts[o + 3] = 255;
                }
            }
        }
        pngio::write(&tp, TILE as u32, TILE as u32, &rgba)?;
        pngio::write(&hp, TILE as u32, TILE as u32, &hts)?;
        out.files.push(format!("tiles/{}/0/{}_{}", name, tx, tz));
        out.files.push(format!("heights/{}/{}_{}", name, tx, tz));
        let dm = &mut meta["dims"][&name];
        let id = format!("{},{}", tx, tz);
        let level0 = have_list(dm, 0);
        if !level0.iter().any(|v| v.as_str() == Some(&id)) {
            level0.push(json!(id));
            dm["tileCount"] = json!(dm["tileCount"].as_u64().unwrap_or(0) + 1);
            let t = TILE as i64;
            let (x0, z0, x1, z1) = (tx as i64 * t, tz as i64 * t, (tx as i64 + 1) * t, (tz as i64 + 1) * t);
            let b = &dm["bounds"];
            dm["bounds"] = if b.is_object() {
                let g = |k: &str| b[k].as_i64().unwrap_or(0);
                json!({ "minX": g("minX").min(x0), "maxX": g("maxX").max(x1), "minZ": g("minZ").min(z0), "maxZ": g("maxZ").max(z1) })
            } else {
                json!({ "minX": x0, "maxX": x1, "minZ": z0, "maxZ": z1 })
            };
            out.meta = true;
        }
        dirty.entry(name).or_default().insert((tx, tz));
    }

    // Rebuild the zoomed-out tiles above every changed tile, one level at a time.
    for (name, mut level) in dirty {
        let dm = &mut meta["dims"][&name];
        let max_zoom = dm["maxZoom"].as_u64().unwrap_or(0) as u32;
        for z in 0..max_zoom {
            let parents: BTreeSet<(i32, i32)> = level.iter().map(|&(x, y)| (x.div_euclid(2), y.div_euclid(2))).collect();
            for &(a, b) in &parents {
                let mut kids: [Option<Vec<u8>>; 4] = [None, None, None, None];
                for q in 0..4i32 {
                    let p = tile_path(map_dir, "tiles", &name, z, a * 2 + (q & 1), b * 2 + (q >> 1));
                    if p.exists() {
                        kids[q as usize] = pngio::read(&p).ok();
                    }
                }
                pngio::write(&tile_path(map_dir, "tiles", &name, z + 1, a, b), TILE as u32, TILE as u32, &combine(&kids))?;
                out.files.push(format!("tiles/{}/{}/{}_{}", name, z + 1, a, b));
                let id = format!("{},{}", a, b);
                let list = have_list(dm, z + 1);
                if !list.iter().any(|v| v.as_str() == Some(&id)) {
                    list.push(json!(id));
                    out.meta = true;
                }
            }
            level = parents;
        }
    }
    if out.meta {
        fs::write(&meta_path, serde_json::to_string_pretty(&meta).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(dim: i32, cx: i32, cz: i32, col: Column) -> Chunk {
        Chunk { dim, cx, cz, columns: vec![Some(col); 256] }
    }

    #[test]
    fn parses_a_chunk_message() {
        let mut cols: Vec<Value> = (0..256).map(|_| json!({ "y": 64, "block": "minecraft:stone" })).collect();
        cols[1] = Value::Null;
        cols[2] = json!({ "y": 62, "block": "minecraft:sand", "water": 3 });
        let c = Chunk::from_json(&json!({ "type": "chunk", "dim": 0, "cx": -3, "cz": 5, "columns": cols })).unwrap();
        assert_eq!((c.cx, c.cz), (-3, 5));
        assert!(c.columns[1].is_none());
        assert_eq!(c.columns[2], Some(Column { y: 62, block: "minecraft:sand".into(), water: 3 }));
        assert!(Chunk::from_json(&json!({ "dim": 0, "cx": 0, "cz": 0, "columns": [] })).is_err());
        // compact form, as the in-game pack sends it
        let mut compact: Vec<Value> = (0..256).map(|_| json!([70, 0])).collect();
        compact[5] = json!([61, 1, 4]);
        compact[6] = Value::Null;
        let c = Chunk::from_json(&json!({ "dim": 1, "cx": 2, "cz": -1, "blocks": ["minecraft:netherrack", "minecraft:gravel"], "columns": compact })).unwrap();
        assert_eq!(c.columns[0], Some(Column { y: 70, block: "minecraft:netherrack".into(), water: 0 }));
        assert_eq!(c.columns[5], Some(Column { y: 61, block: "minecraft:gravel".into(), water: 4 }));
        assert!(c.columns[6].is_none());
        let mut bad = compact.clone();
        bad[0] = json!([70, 9]);
        assert!(Chunk::from_json(&json!({ "dim": 0, "cx": 0, "cz": 0, "blocks": ["minecraft:stone"], "columns": bad })).is_err());
        assert!(Chunk::from_json(&json!({ "dim": 7, "cx": 0, "cz": 0, "columns": vec![Value::Null; 256] })).is_err());
    }

    #[test]
    fn draws_into_tiles_and_the_zoom_pyramid() {
        let dir = std::env::temp_dir().join(format!("bedrock-map-live-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let meta = json!({ "name": "t", "dims": { "overworld": { "dim": 0, "heights": true, "maxZoom": 1, "tileCount": 1, "have": { "0": ["0,0"], "1": ["0,0"] }, "bounds": { "minX": 0, "maxX": 512, "minZ": 0, "maxZ": 512 } } } });
        fs::write(dir.join("meta.json"), meta.to_string()).unwrap();
        // chunk (1, 0) inside the existing tile, and chunk (-1, 0) which starts a new tile to the west
        let stone = Column { y: 70, block: "minecraft:stone".into(), water: 0 };
        let r = apply(&dir, &[chunk(0, 1, 0, stone.clone()), chunk(0, -1, 0, stone)]).unwrap();
        assert!(r.meta);
        assert!(r.files.contains(&"tiles/overworld/0/0_0".to_string()));
        assert!(r.files.contains(&"tiles/overworld/1/-1_0".to_string()));
        let t = pngio::read(&tile_path(&dir, "tiles", "overworld", 0, 0, 0)).unwrap();
        let o = (5 * TILE + 16 + 3) * 4; // block (19, 5) is in chunk (1, 0)
        assert_eq!(&t[o..o + 4], &[125, 125, 125, 255]);
        assert_eq!(t[3], 0, "chunk (0, 0) was not sent and stays empty");
        let h = pngio::read(&dir.join("heights/overworld/0_0.png")).unwrap();
        assert_eq!(height_at(&h, o), Some(70));
        let m: Value = serde_json::from_str(&fs::read_to_string(dir.join("meta.json")).unwrap()).unwrap();
        let d = &m["dims"]["overworld"];
        assert_eq!(d["tileCount"], 2);
        assert_eq!(d["bounds"]["minX"], -512);
        assert!(d["have"]["0"].as_array().unwrap().contains(&json!("-1,0")));
        assert!(d["have"]["1"].as_array().unwrap().contains(&json!("-1,0")));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn water_columns_are_blended_and_not_shaded() {
        let shallow = paint(&Column { y: 62, block: "minecraft:sand".into(), water: 2 }, Some(10)).unwrap();
        assert_eq!(shallow, blend(0.51, WATER, classify("minecraft:sand").rgb));
        assert!(paint(&Column { y: 5, block: "minecraft:torch".into(), water: 0 }, None).is_none());
    }
}
