//! Block name -> map colour. kind: 0 = see-through (skipped), 1 = solid, 2 = water.
pub use crate::color_tables::SKIP_EXACT;
use crate::color_tables::{DYES, EXACT, LEAVES, WOODS};
use regex::Regex;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::{LazyLock, Mutex};

pub const WATER: [u8; 3] = [47, 88, 190];

#[derive(Clone, Copy)]
pub struct Class {
    pub kind: u8,
    pub rgb: [u8; 3],
    pub known: bool,
}

fn table(t: &'static [(&'static str, [u8; 3])]) -> HashMap<&'static str, [u8; 3]> {
    t.iter().cloned().collect()
}

static DYE_MAP: LazyLock<HashMap<&'static str, [u8; 3]>> = LazyLock::new(|| table(DYES));
static WOOD_MAP: LazyLock<HashMap<&'static str, [u8; 3]>> = LazyLock::new(|| table(WOODS));
static LEAF_MAP: LazyLock<HashMap<&'static str, [u8; 3]>> = LazyLock::new(|| table(LEAVES));
static EXACT_MAP: LazyLock<HashMap<&'static str, [u8; 3]>> = LazyLock::new(|| table(EXACT));
static SKIP_SET: LazyLock<HashSet<&'static str>> = LazyLock::new(|| SKIP_EXACT.iter().cloned().collect());

/// Name endings of see-through blocks (also copied into the in-game pack, see live_game.rs).
pub const SKIP_PATTERN: &str = r"(_sapling|_torch|_tulip|_button|_pressure_plate|_sign|_hanging_sign|_stained_glass(_pane)?|_glass(_pane)?|_door|_trapdoor|_banner|_candle|_coral(_fan|_wall_fan)?|_rail)$";
static SKIP_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(SKIP_PATTERN).unwrap());
static DYED_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(light_blue|light_gray|[a-z]+)_(wool|concrete|concrete_powder|carpet|glazed_terracotta|shulker_box|bed|candle_cake)$").unwrap()
});
static TERRACOTTA_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(light_blue|light_gray|[a-z]+)_terracotta$").unwrap());
static PLAIN_CLAY_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(hardened_clay|stained_hardened_clay|terracotta)$").unwrap());
static WOOD_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:stripped_)?(dark_oak|pale_oak|oak|spruce|birch|jungle|acacia|mangrove|cherry|bamboo|crimson|warped)_").unwrap()
});
static OAK_PLAIN_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(planks|log|log2|wood|fence|wooden_slab|double_wooden_slab)$").unwrap());
static STONEISH_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(stone|slab|stairs|wall)$").unwrap());
static SANDSTONE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(^|_)sandstone").unwrap());
static BRICK_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(^|_)brick").unwrap());
static MUSHROOM_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"mushroom_block|mushroom_stem").unwrap());

static CACHE: LazyLock<Mutex<HashMap<String, Class>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

thread_local! {
    /// Per-thread copy of CACHE, so drawing threads don't queue on the shared lock for every palette entry.
    static LOCAL: RefCell<HashMap<String, Class>> = RefCell::new(HashMap::new());
}

fn hash_color(name: &str) -> [u8; 3] {
    let mut h: u32 = 7;
    for c in name.encode_utf16() {
        h = h.wrapping_mul(31).wrapping_add(c as u32);
    }
    [(90 + (h & 63)) as u8, (90 + ((h >> 6) & 63)) as u8, (90 + ((h >> 12) & 63)) as u8]
}

fn mul(c: [u8; 3], f: f64) -> [u8; 3] {
    let m = |v: u8| ((v as f64) * f).round().clamp(0.0, 255.0) as u8;
    [m(c[0]), m(c[1]), m(c[2])]
}

fn ex(name: &str) -> [u8; 3] {
    EXACT_MAP[name]
}

fn color_for(n: &str) -> Option<[u8; 3]> {
    if let Some(c) = EXACT_MAP.get(n) {
        return Some(*c);
    }
    if let Some(prefix) = n.strip_suffix("_leaves") {
        if let Some(c) = LEAF_MAP.get(prefix) {
            return Some(*c);
        }
    }
    if n == "leaves" || n == "leaves2" {
        return Some(LEAF_MAP["oak"]);
    }
    if let Some(m) = DYED_RE.captures(n) {
        if let Some(c) = DYE_MAP.get(&m[1]) {
            return Some(*c);
        }
    }
    if let Some(m) = TERRACOTTA_RE.captures(n) {
        if let Some(c) = DYE_MAP.get(&m[1]) {
            return Some(mul(*c, 0.65));
        }
    }
    if PLAIN_CLAY_RE.is_match(n) {
        return Some([152, 94, 67]);
    }
    if let Some(m) = WOOD_RE.captures(n) {
        if let Some(c) = WOOD_MAP.get(&m[1]) {
            return Some(*c);
        }
    }
    if OAK_PLAIN_RE.is_match(n) {
        return Some(WOOD_MAP["oak"]);
    }
    if n.ends_with("_ore") {
        let nether = n.starts_with("nether_")
            || ((n.contains("quartz") || n.contains("gold") || n.contains("debris")) && n.contains("nether"));
        return Some(if n.starts_with("deepslate_") {
            ex("deepslate")
        } else if nether {
            ex("netherrack")
        } else {
            ex("stone")
        });
    }
    if n.contains("copper") {
        return Some(ex("copper_block"));
    }
    if SANDSTONE_RE.is_match(n) {
        return Some(if n.contains("red_") { ex("red_sandstone") } else { ex("sandstone") });
    }
    if n.contains("quartz") {
        return Some(ex("quartz_block"));
    }
    if n.contains("prismarine") {
        return Some(ex("prismarine"));
    }
    if n.contains("deepslate") {
        return Some(ex("deepslate"));
    }
    if n.contains("blackstone") {
        return Some(ex("blackstone"));
    }
    if n.contains("nether_brick") {
        return Some([44, 21, 26]);
    }
    if BRICK_RE.is_match(n) {
        return Some(ex("bricks"));
    }
    if n.contains("cobblestone") {
        return Some(ex("cobblestone"));
    }
    if STONEISH_RE.is_match(n) {
        return Some(ex("stone"));
    }
    if MUSHROOM_RE.is_match(n) {
        return Some(if n.contains("red") {
            [200, 46, 45]
        } else if n.contains("brown") {
            [149, 111, 81]
        } else {
            [215, 205, 190]
        });
    }
    None
}

/// Returns the class for a palette name such as "minecraft:stone".
pub fn classify(full_name: &str) -> Class {
    if let Some(c) = LOCAL.with(|m| m.borrow().get(full_name).copied()) {
        return c;
    }
    let c = classify_shared(full_name);
    LOCAL.with(|m| m.borrow_mut().insert(full_name.to_string(), c));
    c
}

fn classify_shared(full_name: &str) -> Class {
    if let Some(c) = CACHE.lock().unwrap().get(full_name) {
        return *c;
    }
    let n = full_name.strip_prefix("minecraft:").unwrap_or(full_name);
    let c = if n == "water" || n == "flowing_water" {
        Class { kind: 2, rgb: WATER, known: true }
    } else if SKIP_SET.contains(n) || SKIP_RE.is_match(n) {
        Class { kind: 0, rgb: [0, 0, 0], known: true }
    } else {
        match color_for(n) {
            Some(rgb) => Class { kind: 1, rgb, known: true },
            None => Class { kind: 1, rgb: hash_color(n), known: false },
        }
    };
    CACHE.lock().unwrap().insert(full_name.to_string(), c);
    c
}

pub fn unknown_blocks() -> Vec<String> {
    let mut v: Vec<String> = CACHE
        .lock()
        .unwrap()
        .iter()
        .filter(|(_, c)| c.kind == 1 && !c.known)
        .map(|(k, _)| k.clone())
        .collect();
    v.sort();
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_the_basics() {
        assert_eq!(classify("minecraft:air").kind, 0);
        assert_eq!(classify("minecraft:short_grass").kind, 0);
        assert_eq!(classify("minecraft:oak_door").kind, 0);
        assert_eq!(classify("minecraft:water").kind, 2);
        let stone = classify("minecraft:stone");
        assert_eq!((stone.kind, stone.rgb), (1, [125, 125, 125]));
        assert_eq!(classify("minecraft:birch_leaves").rgb, [105, 130, 70]);
        assert_eq!(classify("minecraft:red_wool").rgb, [160, 39, 34]);
        assert!(classify("minecraft:totally_made_up_block").known == false);
    }
}
