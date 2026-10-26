//! The in-game map's colours: 256 colours on one font page (U+E200 .. U+E2FF), chosen for Minecraft terrain.
//!
//! The pack draws its maps as text, one coloured character per square. In game, a row of text that keeps switching
//! between font pages drifts sideways a little at every switch, so map rows with many colours ended in different
//! places and the map looked sheared. With every colour on one page there is nothing to switch. The 256 colours
//! (like the 248 of Minecraft's own maps) are picked by k-means from the app's block colours, as the pack draws
//! them: every hill-shading step and every water depth, with the most common ground (grass, leaves, water, stone,
//! sand ...) weighted up. Three cells are reserved: "not mapped yet" and the white and red of player markers.
//!
//! The pack gets a 32768-entry table from 5-bit red, green and blue (the shaded colour it wants) to the nearest
//! cell, so drawing is one lookup per square.
use crate::color_tables::{DYES, EXACT, LEAVES, WOODS};
use crate::colors::WATER;
use rayon::prelude::*;
use std::sync::OnceLock;

/// Hill shading as the pack does it (main.js): MAX_SLOPE steps either way, brightness 1 +- SHADE * sqrt(slope / 2),
/// on a colour lifted a little off black.
pub const MAX_SLOPE: i32 = 12;
pub const SHADE: f64 = 0.16;

const UNKNOWN_RGB: [u8; 3] = [30, 30, 36];
const WHITE_RGB: [u8; 3] = [255, 255, 255];
const RED_RGB: [u8; 3] = [220, 30, 30];

pub struct Palette {
    /// The colour of each cell of the font page, in cell order.
    pub cells: [[u8; 3]; 256],
    /// 5-bit r, g, b (r << 10 | g << 5 | b) -> cell
    pub lut: Vec<u8>,
    pub unknown: u8,
    pub white: u8,
    pub red: u8,
}

/// The 5-bit value the pack draws for 5-bit channel `v` at `slope` (main.js builds the same table).
pub fn shaded5(v: u8, slope: i32) -> u8 {
    let f = 1.0 + (slope as f64).signum() * SHADE * ((slope.abs() as f64) / 2.0).sqrt();
    let x = ((12.0 + 0.9 * v as f64 * 255.0 / 31.0) * f).clamp(0.0, 255.0);
    (x * 31.0 / 255.0).round() as u8
}

fn to5(c: [u8; 3]) -> [u8; 3] {
    [c[0] >> 3, c[1] >> 3, c[2] >> 3]
}
fn from5(c: [u8; 3]) -> [u8; 3] {
    c.map(|v| ((v as u32 * 255 + 15) / 31) as u8)
}

/// sRGB -> Oklab, so "nearest colour" means nearest to the eye.
fn oklab(c: [u8; 3]) -> [f32; 3] {
    let lin = |v: u8| {
        let v = v as f32 / 255.0;
        if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
    };
    let (r, g, b) = (lin(c[0]), lin(c[1]), lin(c[2]));
    let l = (0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b).cbrt();
    let m = (0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b).cbrt();
    let s = (0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b).cbrt();
    [
        0.2104542553 * l + 0.7936177850 * m - 0.0040720468 * s,
        1.9779984951 * l - 2.4285922050 * m + 0.4505937099 * s,
        0.0259040371 * l + 0.7827717662 * m - 0.8086757660 * s,
    ]
}
fn dist(a: &[f32; 3], b: &[f32; 3]) -> f32 {
    (a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)
}
fn blend(a: f64, w: [u8; 3], under: [u8; 3]) -> [u8; 3] {
    [0, 1, 2].map(|i| (w[i] as f64 * a + under[i] as f64 * (1.0 - a)).round() as u8)
}

/// The colours the pack can draw, with how common each is likely to be.
fn samples() -> Vec<([u8; 3], f32)> {
    let common = ["grass_block", "stone", "sand", "dirt", "gravel", "snow", "snow_layer", "deepslate", "sandstone", "red_sand", "podzol", "coarse_dirt", "clay", "mud", "moss_block", "netherrack", "end_stone", "andesite", "diorite", "granite", "tuff", "calcite", "terracotta", "ice", "packed_ice", "mycelium"];
    let mut base: Vec<([u8; 3], f32)> = Vec::new();
    for (name, c) in EXACT {
        if *name != "water" {
            base.push((*c, if common.contains(name) { 8.0 } else { 1.0 }));
        }
    }
    for (_, c) in LEAVES {
        base.push((*c, 6.0));
    }
    for (_, c) in DYES {
        base.push((*c, 0.5));
        base.push((c.map(|v| (v as f64 * 0.65).round() as u8), 0.5)); // terracotta
    }
    for (_, c) in WOODS {
        base.push((*c, 1.0));
    }
    for c in [[152, 94, 67], [44, 21, 26], [200, 46, 45], [149, 111, 81], [215, 205, 190], [60, 125, 40], [162, 130, 78]] {
        base.push((c, 1.0));
    }
    let mut out = Vec::new();
    for (c, w) in base {
        let c5 = to5(c);
        for s in -MAX_SLOPE..=MAX_SLOPE {
            let shaded = c5.map(|v| shaded5(v, s));
            out.push((from5(shaded), w * (-(s.abs() as f32) / 3.0).exp()));
        }
    }
    // water is not shaded: open water over the dark deep, and shallow water over common sea and river floors
    let floor = |n: &str| EXACT.iter().find(|(k, _)| *k == n).map(|(_, c)| *c).unwrap_or([128, 128, 128]);
    for d in 1..=16 {
        let mut under = vec![([20, 20, 20], 1.0), (floor("sand"), 0.6), (floor("gravel"), 0.4), (floor("dirt"), 0.3), (floor("clay"), 0.2), (floor("stone"), 0.2)];
        let deep = under.remove(0);
        let water = |a: f64, u: [u8; 3]| to5(blend(a, WATER, u)).map(|v| shaded5(v, 0));
        out.push((from5(water((0.55 + 0.03 * d as f64).min(0.97), deep.0)), 6.0));
        for (u, w) in under {
            out.push((from5(water((0.45 + 0.03 * d as f64).min(0.92), u)), w * 2.0));
        }
    }
    out
}

/// Weighted k-means in Oklab, started from the heaviest samples spread apart (k-means++ with a fixed seed, so the
/// palette is the same every time).
fn kmeans(samples: &[([u8; 3], f32)], k: usize) -> Vec<[u8; 3]> {
    let pts: Vec<[f32; 3]> = samples.iter().map(|(c, _)| oklab(*c)).collect();
    let w: Vec<f32> = samples.iter().map(|(_, w)| *w).collect();
    let mut seed: u64 = 0x9E3779B97F4A7C15;
    let mut rand = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed >> 11) as f64 / (1u64 << 53) as f64
    };
    let first = (0..pts.len()).max_by(|&a, &b| w[a].total_cmp(&w[b])).unwrap();
    let mut centres = vec![pts[first]];
    let mut near: Vec<f32> = pts.iter().map(|p| dist(p, &pts[first])).collect();
    while centres.len() < k {
        let total: f64 = near.iter().zip(&w).map(|(d, w)| (*d * *w) as f64).sum();
        let mut pick = rand() * total;
        let mut i = 0;
        while i + 1 < pts.len() {
            pick -= (near[i] * w[i]) as f64;
            if pick <= 0.0 {
                break;
            }
            i += 1;
        }
        centres.push(pts[i]);
        for (j, p) in pts.iter().enumerate() {
            near[j] = near[j].min(dist(p, &pts[i]));
        }
    }
    for _ in 0..30 {
        let mut sum = vec![[0f64; 4]; k];
        for (j, p) in pts.iter().enumerate() {
            let c = (0..k).min_by(|&a, &b| dist(p, &centres[a]).total_cmp(&dist(p, &centres[b]))).unwrap();
            for d in 0..3 {
                sum[c][d] += (p[d] * w[j]) as f64;
            }
            sum[c][3] += w[j] as f64;
        }
        for c in 0..k {
            if sum[c][3] > 0.0 {
                centres[c] = [0, 1, 2].map(|d| (sum[c][d] / sum[c][3]) as f32);
            }
        }
    }
    // back to sRGB: the sample nearest each centre (so every palette colour is one the map really draws)
    centres
        .iter()
        .map(|c| samples[(0..pts.len()).min_by(|&a, &b| dist(&pts[a], c).total_cmp(&dist(&pts[b], c))).unwrap()].0)
        .collect()
}

fn build() -> Palette {
    let mut colours = kmeans(&samples(), 253);
    colours.sort();
    colours.dedup();
    // fill up to 253 with greys if k-means put two centres on one colour
    let mut g = 0u8;
    while colours.len() < 253 {
        let c = [g, g, g];
        if !colours.contains(&c) {
            colours.push(c);
        }
        g = g.wrapping_add(37);
    }
    colours.extend([UNKNOWN_RGB, WHITE_RGB, RED_RGB]);
    // Cell order: 16 rows from dark to light, each row by hue, so neighbouring cells are alike (the game blends a
    // cell's edges with its neighbours when it shrinks the font).
    let lab: Vec<[f32; 3]> = colours.iter().map(|c| oklab(*c)).collect();
    let mut order: Vec<usize> = (0..256).collect();
    order.sort_by(|&a, &b| lab[a][0].total_cmp(&lab[b][0]));
    for row in order.chunks_mut(16) {
        row.sort_by(|&a, &b| lab[a][2].atan2(lab[a][1]).total_cmp(&lab[b][2].atan2(lab[b][1])));
    }
    let mut cells = [[0u8; 3]; 256];
    for (cell, &i) in order.iter().enumerate() {
        cells[cell] = colours[i];
    }
    let cell_of = |c: [u8; 3]| cells.iter().rposition(|x| *x == c).unwrap() as u8;
    let (unknown, white, red) = (cell_of(UNKNOWN_RGB), cell_of(WHITE_RGB), cell_of(RED_RGB));
    // nearest drawable cell (not the reserved ones) for every 5-bit colour
    let drawable: Vec<(u8, [f32; 3])> = (0..256u32).map(|i| i as u8).filter(|i| ![unknown, white, red].contains(i)).map(|i| (i, oklab(cells[i as usize]))).collect();
    let lut: Vec<u8> = (0..32768u32)
        .into_par_iter()
        .map(|v| {
            let p = oklab(from5([(v >> 10) as u8, ((v >> 5) & 31) as u8, (v & 31) as u8]));
            drawable.iter().min_by(|a, b| dist(&a.1, &p).total_cmp(&dist(&b.1, &p))).unwrap().0
        })
        .collect();
    Palette { cells, lut, unknown, white, red }
}

pub fn palette() -> &'static Palette {
    static P: OnceLock<Palette> = OnceLock::new();
    P.get_or_init(build)
}

pub fn base64(data: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::with_capacity(data.len() / 3 * 4 + 4);
    for ch in data.chunks(3) {
        let n = (ch[0] as u32) << 16 | (*ch.get(1).unwrap_or(&0) as u32) << 8 | *ch.get(2).unwrap_or(&0) as u32;
        s.push(A[(n >> 18) as usize & 63] as char);
        s.push(A[(n >> 12) as usize & 63] as char);
        s.push(if ch.len() > 1 { A[(n >> 6) as usize & 63] as char } else { '=' });
        s.push(if ch.len() > 2 { A[n as usize & 63] as char } else { '=' });
    }
    s
}

/// What main.js gets in place of its PALETTE placeholder.
pub fn script_json() -> String {
    let p = palette();
    format!("{{ lut: '{}', unknown: {}, white: {}, red: {} }}", base64(&p.lut), p.unknown, p.white, p.red)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_has_the_reserved_cells_and_maps_terrain_closely() {
        let p = palette();
        assert_eq!(p.cells[p.unknown as usize], UNKNOWN_RGB);
        assert_eq!(p.cells[p.white as usize], WHITE_RGB);
        assert_eq!(p.cells[p.red as usize], RED_RGB);
        assert_eq!(p.lut.len(), 32768);
        assert!(p.lut.iter().all(|c| ![p.unknown, p.white, p.red].contains(c)), "map colours never use the reserved cells");
        // flat grass, water and oak leaves come out close to their colour (Oklab distance; 0.03 is barely visible)
        for c in [[104u8, 156, 56], [47, 88, 190], [60, 125, 40], [125, 125, 125], [219, 207, 163]] {
            let want = to5(c).map(|v| shaded5(v, 0));
            let got = p.cells[p.lut[(want[0] as usize) << 10 | (want[1] as usize) << 5 | want[2] as usize] as usize];
            let d = dist(&oklab(from5(want)), &oklab(got)).sqrt();
            assert!(d < 0.03, "{:?} drawn as {:?} (distance {:.3})", c, got, d);
        }
        // one step of shading on grass still changes the colour
        let g = to5([104, 156, 56]);
        let cell = |s: i32| {
            let v = g.map(|v| shaded5(v, s));
            p.lut[(v[0] as usize) << 10 | (v[1] as usize) << 5 | v[2] as usize]
        };
        assert_ne!(cell(0), cell(1));
        assert_ne!(cell(0), cell(-1));
    }

    #[test]
    fn base64_round_numbers() {
        assert_eq!(base64(b"Man"), "TWFu");
        assert_eq!(base64(b"Ma"), "TWE=");
        assert_eq!(base64(b"M"), "TQ==");
    }
}
