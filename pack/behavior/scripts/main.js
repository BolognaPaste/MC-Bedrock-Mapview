// Bedrock Map Live: maps the world around players as they play, and shows it in game: a minimap in the top-right
// corner with your status effects listed down its left side, and a big map in the middle of the screen when you
// use the Map item (or after /map). It also feeds the Bedrock Map app's live map.
//
// The map data is the same live feed the app gets: each scanned chunk's top blocks. In game it is kept as a colour and
// a height per block column, in 128 x 128 block regions saved in the world (dynamic properties), so the map survives
// restarts. Add-ons cannot draw pixels, so the map is drawn with characters from the resource pack's font pages,
// one solid colour per character (256 colours on font page glyph_E2.png), with hill shading worked out from the heights,
// and sent as a title the resource pack's HUD shows.
//
// For the app: a world pack cannot use the network outside a dedicated server, so every message for the app is
// written to the game's content log instead (Settings > Creator > Content Log File), which the app reads as it
// grows. Each line is "BMLIVE1 " followed by JSON in the app's live link protocol (docs/live-protocol.md).
import { world, system, ItemStack, CommandPermissionLevel, CustomCommandStatus } from '@minecraft/server';

const TAG = 'BMLIVE1 ';
const VERSION = '1.5.6';
const DIMS = { 'minecraft:overworld': 0, 'minecraft:nether': 1, 'minecraft:the_end': 2 };
const DIM_IDS = ['minecraft:overworld', 'minecraft:nether', 'minecraft:the_end'];
const EXPLORE_RADIUS = 8; // chunks around each player that are scanned once per session, so new ground shows up
const REFRESH_RADIUS = 1; // chunks around each player scanned again every 30 s (growth, fire, water, mobs...)
const CHANGE_DELAY = 30; // ticks to wait after a block change before scanning its chunk (batches quick edits)
const MAX_WATER = 16; // deeper water looks the same on the map, so stop counting there
const MAX_QUEUE = 600;

// Blocks the map looks through (plants, torches, glass...) and the map colours: the app fills these in from its
// own colour table when it packages the add-on, so the in-game map, the app's map and the live feed agree.
const SKIP = new Set(/*BM:SKIP*/['air', 'cave_air', 'void_air', 'short_grass', 'tall_grass', 'torch', 'seagrass', 'kelp']/*BM:END*/);
const SKIP_RE = new RegExp(/*BM:SKIPRE*/'(_sapling|_torch|_button|_sign|_glass(_pane)?|_door|_trapdoor|_rail)$'/*BM:END*/);
const EXACT = /*BM:EXACT*/{ stone: [125, 125, 125], grass_block: [95, 159, 53], dirt: [134, 96, 67], sand: [219, 207, 163], gravel: [131, 127, 126], water: [47, 88, 190], deepslate: [80, 80, 82], netherrack: [111, 54, 52], cobblestone: [122, 122, 122], snow: [249, 254, 254] }/*BM:END*/;
const LEAVES = /*BM:LEAVES*/{ oak: [60, 125, 40] }/*BM:END*/;
const DYES = /*BM:DYES*/{ white: [233, 236, 236], red: [160, 39, 34] }/*BM:END*/;
const WOODS = /*BM:WOODS*/{ oak: [162, 130, 78] }/*BM:END*/;
const WATER = [47, 88, 190];

const session = Math.random().toString(36).slice(2, 10) + Date.now().toString(36).slice(-4);
const send = (t, body) => console.warn(TAG + JSON.stringify(Object.assign({ s: session, t }, body)));
const push = (...m) => send('push', { m });

/* ---- block colours (a port of the app's colors.rs) ---- */

const ex = n => EXACT[n] || [128, 128, 128];
const mulc = (c, f) => c.map(v => Math.max(0, Math.min(255, Math.round(v * f))));
const DYED_RE = /^(light_blue|light_gray|[a-z]+)_(wool|concrete|concrete_powder|carpet|glazed_terracotta|shulker_box|bed|candle_cake)$/;
const TERRACOTTA_RE = /^(light_blue|light_gray|[a-z]+)_terracotta$/;
const WOOD_RE = /^(?:stripped_)?(dark_oak|pale_oak|oak|spruce|birch|jungle|acacia|mangrove|cherry|bamboo|crimson|warped)_/;
function colorFor(n) {
  if (EXACT[n]) return EXACT[n];
  if (n.endsWith('_leaves') && LEAVES[n.slice(0, -7)]) return LEAVES[n.slice(0, -7)];
  if (n === 'leaves' || n === 'leaves2') return LEAVES.oak || [60, 125, 40];
  let m = DYED_RE.exec(n);
  if (m && DYES[m[1]]) return DYES[m[1]];
  m = TERRACOTTA_RE.exec(n);
  if (m && DYES[m[1]]) return mulc(DYES[m[1]], 0.65);
  if (/^(hardened_clay|stained_hardened_clay|terracotta)$/.test(n)) return [152, 94, 67];
  m = WOOD_RE.exec(n);
  if (m && WOODS[m[1]]) return WOODS[m[1]];
  if (/^(planks|log|log2|wood|fence|wooden_slab|double_wooden_slab)$/.test(n)) return WOODS.oak || [162, 130, 78];
  if (n.endsWith('_leaves')) return LEAVES.oak || [60, 125, 40];
  if (n.endsWith('_ore')) return n.startsWith('deepslate_') ? ex('deepslate') : n.startsWith('nether_') || ((n.includes('quartz') || n.includes('gold')) && n.includes('nether')) ? ex('netherrack') : ex('stone');
  if (n.includes('copper')) return ex('copper_block');
  if (/(^|_)sandstone/.test(n)) return n.includes('red_') ? ex('red_sandstone') : ex('sandstone');
  if (n.includes('quartz')) return ex('quartz_block');
  if (n.includes('prismarine')) return ex('prismarine');
  if (n.includes('deepslate')) return ex('deepslate');
  if (n.includes('blackstone')) return ex('blackstone');
  if (n.includes('nether_brick')) return [44, 21, 26];
  if (/(^|_)brick/.test(n)) return ex('bricks');
  if (n.includes('cobblestone')) return ex('cobblestone');
  if (/(stone|slab|stairs|wall)$/.test(n)) return ex('stone');
  if (/mushroom_block|mushroom_stem/.test(n)) return n.includes('red') ? [200, 46, 45] : n.includes('brown') ? [149, 111, 81] : [215, 205, 190];
  let h = 7;
  for (let i = 0; i < n.length; i++) h = (Math.imul(h, 31) + n.charCodeAt(i)) >>> 0;
  return [90 + (h & 63), 90 + ((h >> 6) & 63), 90 + ((h >> 12) & 63)];
}
const colorCache = new Map();
function blockColor(id) {
  let c = colorCache.get(id);
  if (!c) { c = colorFor(id.startsWith('minecraft:') ? id.slice(10) : id); colorCache.set(id, c); }
  return c;
}
const blend = (a, w, under) => [0, 1, 2].map(i => Math.round(w[i] * a + under[i] * (1 - a)));

/* ---- the in-game map: colour and height per column in 128 x 128 regions, saved in the world ---- */

// Per column: c is 0 (not mapped) or 1 + its colour as 5-bit red, green and blue (the app's colour with water blended
// in, not shaded), and hw is its height + 64, plus 512 * the water depth. Hills are shaded when the map is drawn,
// from the heights, so they come out right whichever chunk was scanned first, and across restarts.
//
// Saved in the world as dynamic properties, so the whole explored map is there again next time, on every platform:
// a region is four strips of 32 rows ('bmlive:m4:dim:rx:rz:strip'), five base64 characters per column
// (c * 16384 + hw, 30 bits). Plain ASCII keeps every strip at 20480 bytes, under the game's 32767 limit whether it
// counts characters or bytes (1.3.0 saved one character per column above U+0800, three bytes each in UTF-8, and
// those saves could be refused). Strips with nothing mapped are not saved. Changed strips are saved every 10 s, when
// a player leaves and when the world closes.
//
// Each region also has an overview: 32 x 32 cells, each the average of 4 x 4 columns ('bmlive:o4:dim:rx:rz', 5120
// characters). The zoomed-out big map (4 to 16 blocks per square) draws from overviews, so it never loads full
// regions: at 16 blocks per square it covers about 600 regions, far more than fit in memory, and loading them all
// made the game stutter every time it redrew. Overviews are worked out from the region whenever it is saved.
const R = 128, STRIP = 32, STRIPS = R / STRIP, STRIP_CHARS = STRIP * R * 5, OV = 32, OV_CHARS = OV * OV * 5;
const B64 = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/';
const B64V = new Uint8Array(128);
for (let i = 0; i < 64; i++) B64V[B64.charCodeAt(i)] = i;
const regions = new Map(); // "dim:rx:rz" -> { key, c, hw (Uint16Array R * R), dirty (bit per strip), ovDirty, used, legacy }
const overviews = new Map(); // "dim:rx:rz" -> { c, hw (Uint16Array OV * OV), stale }, least recently used first
let MAX_REGIONS = 160; // kept in memory (64 KB each); the rest stay saved in the world
let MAX_OVERVIEWS = 1024; // 4 KB each
let saveBlockedUntil = 0; // the world refused map data: try again later
let tickNow = 0;
let mapRev = 0; // goes up whenever a map column changes, so drawn maps know to redraw
try {
  const tier = system.serverSystemInfo.memoryTier; // MemoryTier: 0 super low, 1 low, 2 mid, 3 high, 4 super high
  if (typeof tier === 'number' && tier <= 1) { MAX_REGIONS = 64; MAX_OVERVIEWS = 384; }
} catch {}
const stripKey = (k, s) => 'bmlive:m4:' + k + ':' + s;
const ovKey = k => 'bmlive:o4:' + k;

// count columns from `from` in c/hw, five base64 characters each
function decodeCols(s, c, hw, from, count) {
  for (let i = 0, o = from; i < count * 5; i += 5, o++) {
    const v = (((((B64V[s.charCodeAt(i)] << 6) | B64V[s.charCodeAt(i + 1)]) << 6 | B64V[s.charCodeAt(i + 2)]) << 6 | B64V[s.charCodeAt(i + 3)]) << 6) | B64V[s.charCodeAt(i + 4)];
    c[o] = v >>> 14;
    hw[o] = v & 16383;
  }
}
// The columns as a string, or null when none of them is mapped.
const encodeBuf = new Uint8Array(STRIP_CHARS);
function encodeCols(c, hw, from, count) {
  let any = false;
  for (let i = 0, o = from; i < count * 5; i += 5, o++) {
    const v = c[o] * 16384 + hw[o];
    if (c[o]) any = true;
    encodeBuf[i] = B64.charCodeAt((v >>> 24) & 63);
    encodeBuf[i + 1] = B64.charCodeAt((v >>> 18) & 63);
    encodeBuf[i + 2] = B64.charCodeAt((v >>> 12) & 63);
    encodeBuf[i + 3] = B64.charCodeAt((v >>> 6) & 63);
    encodeBuf[i + 4] = B64.charCodeAt(v & 63);
  }
  if (!any) return null;
  let s = '';
  for (let i = 0; i < count * 5; i += 4096) s += String.fromCharCode.apply(null, encodeBuf.subarray(i, Math.min(i + 4096, count * 5)));
  return s;
}

const missing = new Map(); // regions not saved in the world -> tick last checked (drawing asks for them often)
function region(dim, rx, rz, create) {
  const k = dim + ':' + rx + ':' + rz;
  let r = regions.get(k);
  if (r) { r.used = tickNow; return r; }
  if (!create && missing.has(k) && tickNow - missing.get(k) < 200) return null;
  // Read what is saved first, and only make room for the region when there is something (or it is being created):
  // drawing checks many regions that were never mapped.
  const strips = [];
  let legacy = null;
  try {
    for (let s = 0; s < STRIPS; s++) {
      const v = world.getDynamicProperty(stripKey(k, s));
      if (typeof v === 'string' && v.length === STRIP_CHARS) strips[s] = v;
    }
    if (!strips.length) {
      // a region saved by pack 1.3.0: read it, and save it again in the new form
      const sc = world.getDynamicProperty('bmlive:c3:' + k), sh = world.getDynamicProperty('bmlive:h3:' + k);
      if (typeof sc === 'string' && typeof sh === 'string' && sc.length === R * R && sh.length === R * R) legacy = [sc, sh];
    }
  } catch {}
  if (!strips.length && !legacy && !create) { missing.set(k, tickNow); return null; }
  missing.delete(k);
  r = { key: k, c: new Uint16Array(R * R), hw: new Uint16Array(R * R), dirty: 0, ovDirty: false, used: tickNow, legacy: !!legacy };
  strips.forEach((v, s) => decodeCols(v, r.c, r.hw, s * STRIP * R, STRIP * R));
  if (legacy) {
    for (let i = 0; i < R * R; i++) { r.c[i] = legacy[0].charCodeAt(i) - 0x100; r.hw[i] = legacy[1].charCodeAt(i) - 0x100; }
    r.dirty = (1 << STRIPS) - 1;
    r.ovDirty = true;
  }
  regions.set(k, r);
  if (regions.size > MAX_REGIONS) {
    // Drop the least recently used region that is fully saved. One with unsaved changes stays (even past the limit)
    // until it can be saved: dropping it would lose them.
    let old = null;
    for (const v of regions.values()) if (v !== r && (!old || v.used < old.used) && !v.dirty) old = v;
    if (!old) {
      for (const v of regions.values()) if (v !== r && (!old || v.used < old.used) && tickNow >= saveBlockedUntil) old = v;
      if (old && !saveRegion(old)) old = null;
    }
    if (old) {
      if (old.ovDirty) saveOverview(old); // the overview can be worked out again later, so this may fail
      regions.delete(old.key);
    }
  }
  return r;
}

// A region's overview: each cell the average colour and height of the mapped columns among its 4 x 4, water when
// most of them are.
function makeOverview(r) {
  const c = new Uint16Array(OV * OV), hw = new Uint16Array(OV * OV);
  for (let cz = 0; cz < OV; cz++) {
    for (let cx = 0; cx < OV; cx++) {
      let cr = 0, cg = 0, cb = 0, h = 0, k = 0, water = 0;
      for (let dz = 0; dz < 4; dz++) {
        for (let dx = 0, i = ((cz * 4 + dz) << 7) | (cx * 4); dx < 4; dx++, i++) {
          if (!r.c[i]) continue;
          const v = r.c[i] - 1;
          cr += v >> 10; cg += (v >> 5) & 31; cb += v & 31; h += r.hw[i] & 511; k++;
          if (r.hw[i] > 511) water++;
        }
      }
      if (!k) continue;
      c[cz * OV + cx] = 1 + ((Math.round(cr / k) << 10) | (Math.round(cg / k) << 5) | Math.round(cb / k));
      hw[cz * OV + cx] = Math.round(h / k) | (water * 2 >= k ? 512 : 0);
    }
  }
  return { c, hw, stale: false };
}
function keepOverview(k, ov) {
  overviews.delete(k);
  overviews.set(k, ov);
  if (overviews.size > MAX_OVERVIEWS) overviews.delete(overviews.keys().next().value);
}
// The overview of a region, or null when nothing there is mapped. From memory when possible; else the saved
// overview; else (regions saved before overviews existed) the full region, once, and its overview is saved.
function overview(dim, rx, rz) {
  const k = dim + ':' + rx + ':' + rz, r = regions.get(k);
  let ov = overviews.get(k);
  if (ov && !(ov.stale && r)) { keepOverview(k, ov); return ov; }
  if (r) { ov = makeOverview(r); keepOverview(k, ov); return ov; }
  if (missing.has(k) && tickNow - missing.get(k) < 200) return null;
  try {
    const s = world.getDynamicProperty(ovKey(k));
    if (typeof s === 'string' && s.length === OV_CHARS) {
      ov = { c: new Uint16Array(OV * OV), hw: new Uint16Array(OV * OV), stale: false };
      decodeCols(s, ov.c, ov.hw, 0, OV * OV);
      keepOverview(k, ov);
      return ov;
    }
  } catch {}
  const full = region(dim, rx, rz, false);
  if (!full) return null;
  full.ovDirty = true;
  ov = makeOverview(full);
  keepOverview(k, ov);
  return ov;
}

// Saves one strip of a region. Returns false when the world refused (the strip stays marked as changed).
function saveStrip(r, s) {
  try {
    world.setDynamicProperty(stripKey(r.key, s), encodeCols(r.c, r.hw, s * STRIP * R, STRIP * R) ?? undefined);
    r.dirty &= ~(1 << s);
  } catch (e) {
    saveBlockedUntil = tickNow + 6000;
    console.warn('Bedrock Map Live: the world did not take more map data (' + e + '). Trying again in 5 minutes.');
    return false;
  }
  if (!r.dirty && r.legacy) {
    try { world.setDynamicProperty('bmlive:c3:' + r.key, undefined); world.setDynamicProperty('bmlive:h3:' + r.key, undefined); } catch {}
    r.legacy = false;
  }
  return true;
}
function saveOverview(r) {
  const ov = makeOverview(r);
  keepOverview(r.key, ov);
  try {
    world.setDynamicProperty(ovKey(r.key), encodeCols(ov.c, ov.hw, 0, OV * OV) ?? undefined);
    r.ovDirty = false;
    return true;
  } catch {
    return false;
  }
}
// The next unsaved piece of a region: a changed strip, then its overview. False when the world refused it.
const unsaved = r => r.dirty || r.ovDirty;
function saveNext(r) {
  for (let s = 0; s < STRIPS; s++) if (r.dirty & (1 << s)) return saveStrip(r, s);
  return saveOverview(r);
}
// Everything a region has not saved yet, at once. True when nothing is left unsaved.
function saveRegion(r) {
  while (unsaved(r)) if (!saveNext(r)) return false;
  return true;
}
// Every region, at once (the world is closing).
const saveAll = () => { for (const r of regions.values()) saveRegion(r); };
// The same, a piece per tick at most, so saving never makes the game stutter.
let saving = false;
function* saveJob() {
  try {
    for (const r of [...regions.values()]) {
      while (unsaved(r)) {
        if (tickNow < saveBlockedUntil || !saveNext(r)) return;
        yield;
      }
    }
  } finally {
    saving = false;
  }
}
function saveSoon() {
  if (saving || tickNow < saveBlockedUntil) return;
  for (const r of regions.values()) {
    if (unsaved(r)) { saving = true; system.runJob(saveJob()); return; }
  }
}
// Map data from pack 1.1 and 1.2 (one 4-bit colour per column, no heights) is not used any more: free the space.
function dropOldMaps() {
  try {
    for (const id of world.getDynamicPropertyIds()) if (id.startsWith('bmlive:r:') || id.startsWith('bmlive:r2:')) world.setDynamicProperty(id, undefined);
  } catch {}
}

// Puts one scanned column on the in-game map, with the same water blend as the app.
function mapColumn(dim, x, z, c) {
  const col = blockColor(c.block);
  let rgb = col;
  if (c.water) {
    const d = c.water;
    rgb = c.block.endsWith('water') ? blend(Math.min(0.55 + 0.03 * d, 0.97), WATER, [20, 20, 20]) : blend(Math.min(0.45 + 0.03 * d, 0.92), WATER, col);
  }
  const r = region(dim, Math.floor(x / R), Math.floor(z / R), true);
  const lz = z - Math.floor(z / R) * R, i = lz * R + (x - Math.floor(x / R) * R);
  const code = 1 + (((rgb[0] >> 3) << 10) | ((rgb[1] >> 3) << 5) | (rgb[2] >> 3));
  const hw = Math.max(0, Math.min(511, c.y + 64)) | (Math.min(c.water || 0, 31) << 9);
  if (r.c[i] !== code || r.hw[i] !== hw) {
    r.c[i] = code;
    r.hw[i] = hw;
    r.dirty |= 1 << (lz >> 5);
    r.ovDirty = true;
    const ov = overviews.get(r.key);
    if (ov) ov.stale = true;
    mapRev++;
  }
}

/* ---- the world: a random id kept in the world, so the app can remember which map it belongs to ---- */

let worldId = null;
function getWorldId() {
  if (worldId) return worldId;
  try {
    let id = world.getDynamicProperty('bmlive:world_id');
    if (typeof id !== 'string') {
      id = 'w-' + Math.random().toString(36).slice(2, 12);
      world.setDynamicProperty('bmlive:world_id', id);
    }
    worldId = id;
    return id;
  } catch {
    return 'unknown';
  }
}
const hello = () => send('hello', { d: { protocol: 1, plugin: { name: 'Bedrock Map Live', version: VERSION }, world: { id: getWorldId() }, server: { name: 'In-game pack' } } });

/* ---- players ---- */

const r1 = v => Math.round(v * 10) / 10;
function players() {
  const out = [];
  for (const p of world.getAllPlayers()) {
    try {
      const l = p.location;
      const e = { id: String(p.id), name: p.name, dim: DIMS[p.dimension.id] ?? 0, x: r1(l.x), y: r1(l.y), z: r1(l.z), yaw: Math.round(p.getRotation().y) };
      const h = p.getComponent('minecraft:health');
      if (h) e.health = Math.round(h.currentValue);
      out.push(e);
    } catch {
      // still joining
    }
  }
  return out;
}

/* ---- chunks: the live feed (for the app and the in-game map) ---- */

const queue = new Map(); // "dimension,cx,cz" -> { dim, cx, cz, at }: chunks waiting to be scanned
const sent = new Set(); // chunks already scanned this session (exploring scans each one once)
const keyOf = (dimId, cx, cz) => dimId + ',' + cx + ',' + cz;

function want(dim, cx, cz, at) {
  const k = keyOf(dim.id, cx, cz), q = queue.get(k);
  if (q) q.at = Math.max(q.at, at);
  else if (queue.size < MAX_QUEUE) queue.set(k, { dim, cx, cz, at });
}
const changed = (dim, loc) => { try { want(dim, Math.floor(loc.x / 16), Math.floor(loc.z / 16), system.currentTick + CHANGE_DELAY); } catch {} };

// Offsets around a player, nearest first.
const ring = r => {
  const o = [];
  for (let dz = -r; dz <= r; dz++) for (let dx = -r; dx <= r; dx++) o.push([dx, dz]);
  return o.sort((a, b) => a[0] * a[0] + a[1] * a[1] - (b[0] * b[0] + b[1] * b[1]));
};
const EXPLORE = ring(EXPLORE_RADIUS), REFRESH = ring(REFRESH_RADIUS);

function nearPlayers(offsets, again) {
  for (const p of world.getAllPlayers()) {
    try {
      const cx = Math.floor(p.location.x / 16), cz = Math.floor(p.location.z / 16);
      for (const [dx, dz] of offsets) {
        if (again || !sent.has(keyOf(p.dimension.id, cx + dx, cz + dz))) want(p.dimension, cx + dx, cz + dz, 0);
      }
    } catch {}
  }
}

const isWater = n => n === 'water' || n === 'flowing_water';
const skip = n => SKIP.has(n) || SKIP_RE.test(n);

// The first block a ray straight down from the sky meets, including water, leaves and plants.
// (Dimension.getTopmostBlock is no good here: it skips water and leaves, so rivers showed their beds and
// trees only their trunks.)
function topBlock(dim, x, z, minY, maxY) {
  const hit = dim.getBlockFromRay({ x: x + 0.5, y: maxY - 0.01, z: z + 0.5 }, { x: 0, y: -1, z: 0 }, { includeLiquidBlocks: true, includePassableBlocks: true, maxDistance: maxY - minY });
  return hit && hit.block;
}

// The highest block of a column that the map shows, as { y, block, water }. Null when the column is not loaded.
function column(dim, x, z, minY, maxY) {
  const b0 = topBlock(dim, x, z, minY, maxY);
  if (!b0) return null;
  let b = b0;
  let y = b.location.y, water = 0, surface = 0;
  for (let steps = 0; y >= minY && steps < 96; steps++) {
    const id = b ? b.typeId : 'minecraft:air';
    const n = id.startsWith('minecraft:') ? id.slice(10) : id;
    if (isWater(n) || (water > 0 && skip(n))) {
      // under water, plants such as kelp and seagrass count as water (they are waterlogged)
      if (!water) surface = y;
      if (++water >= MAX_WATER) return { y: surface, block: 'minecraft:water', water };
    } else if (!skip(n)) {
      return water ? { y: surface, block: id, water } : { y, block: id };
    }
    y--;
    b = y >= minY ? dim.getBlock({ x, y, z }) : undefined;
  }
  return water ? { y: surface, block: 'minecraft:water', water } : null;
}

// Scans one chunk: draws it on the in-game map and sends it to the app as a compact chunk message
// ({ blocks: names, columns: [y, name index, water?] }). Yields every 16 columns to spread the work over ticks.
function* scan(job) {
  const { dim, cx, cz } = job;
  const d = DIMS[dim.id] ?? 0;
  let minY = -64, maxY = 320;
  try { minY = dim.heightRange.min; maxY = dim.heightRange.max; } catch {}
  const blocks = [], index = new Map(), columns = new Array(256);
  let loaded = 0;
  for (let lz = 0; lz < 16; lz++) {
    for (let lx = 0; lx < 16; lx++) {
      let c = null;
      const x = cx * 16 + lx, z = cz * 16 + lz;
      try {
        c = column(dim, x, z, minY, maxY);
      } catch {
        if (!loaded && lx === 0 && lz === 0) return; // not loaded (the first block throws): don't try the other 255
        c = null;
      }
      if (c) {
        loaded++;
        mapColumn(d, x, z, c);
        let i = index.get(c.block);
        if (i === undefined) { i = blocks.length; blocks.push(c.block); index.set(c.block, i); }
        columns[lz * 16 + lx] = c.water ? [c.y, i, c.water] : [c.y, i];
      } else columns[lz * 16 + lx] = null;
    }
    yield;
  }
  if (!loaded) return; // not loaded right now; exploring asks again later
  sent.add(keyOf(dim.id, cx, cz));
  push({ type: 'chunk', dim: d, cx, cz, blocks, columns });
}

// Works through the chunks that are due, a little each tick.
let busy = false;
function* work() {
  try {
    for (;;) {
      const now = system.currentTick;
      let job = null;
      for (const [k, q] of queue) if (q.at <= now) { job = q; queue.delete(k); break; }
      if (!job) return;
      yield* scan(job);
    }
  } finally {
    busy = false;
  }
}
function kick() {
  if (!busy && queue.size) { busy = true; system.runJob(work()); }
}

/* ---- drawing the map ---- */

// Add-ons cannot draw pixels, so a map is text: one character per square, from the resource pack's font page
// font/glyph_E2.png: 256 solid colours chosen for Minecraft terrain (the app picks them from its block colours, with
// every shading step and water depth; see map_palette.rs). All of them are on one page on purpose: in game, a row
// of text drifts sideways a little every time it switches font pages, and rows with many colours on several pages
// came out sheared. PALETTE.lut maps a 5-bit red, green and blue colour (r << 10 | g << 5 | b) to the nearest cell;
// character U+E200 + cell draws it. The app fills PALETTE in when it packages the add-on.
const PALETTE = /*BM:PALETTE*/{ lut: '', unknown: 0, white: 1, red: 2 }/*BM:END*/;
const PAL = new Uint8Array(32768);
for (let i = 0, o = 0; i + 3 < PALETTE.lut.length + 3 && o < PAL.length; i += 4) {
  const s = PALETTE.lut, n = (B64V[s.charCodeAt(i)] << 18) | (B64V[s.charCodeAt(i + 1)] << 12) | (B64V[s.charCodeAt(i + 2)] << 6) | B64V[s.charCodeAt(i + 3)];
  PAL[o++] = n >> 16;
  if (o < PAL.length) PAL[o++] = (n >> 8) & 255;
  if (o < PAL.length) PAL[o++] = n & 255;
}
const UNKNOWN = 0xe200 + PALETTE.unknown; // not mapped yet
const MARK_RING = 0xe200 + PALETTE.white, MARK_DOT = 0xe200 + PALETTE.red; // another player: a white cross, red middle

// Hill shading, done while drawing: a square higher than the squares north and west of it is lighter, a lower one
// darker, so slopes face a light in the north-west. The slope is the sum of both height differences (capped), and
// the brightness 1 +- SHADE * sqrt(slope / 2): 16% for one block up, 32% for four, stronger than the app's (7% per
// block) so it still shows with 256 colours. SH[(slope + MAX_SLOPE) * 32 + v] is a 5-bit colour channel v under that
// slope, also lifted a little off black so dark blocks still show their shading (map_palette.rs does the same).
const MAX_SLOPE = 12, SHADE = 0.16;
const SH = new Uint8Array((2 * MAX_SLOPE + 1) * 32);
for (let s = -MAX_SLOPE; s <= MAX_SLOPE; s++) {
  const f = 1 + Math.sign(s) * SHADE * Math.sqrt(Math.abs(s) / 2);
  for (let v = 0; v < 32; v++) SH[(s + MAX_SLOPE) * 32 + v] = Math.round(Math.max(0, Math.min(255, (12 + 0.9 * v * 255 / 31) * f)) * 31 / 255);
}

// Draws cols x rows squares from (x0, z0), `scale` blocks per square, north up, into `out` as rows of character
// codes; yields after each row (it runs as a job). Zoomed out, a square is the average of 2 x 2 blocks spread over
// it, so the map does not flicker as you move. The regions in view are looked up once, then every block is plain
// arithmetic (regions are 128 = 2^7 blocks wide, so >> 7 and & 127 split a coordinate, negative ones too). One more
// row and column than shown are read, north and west, for the shading of the first ones.
function* drawGrid(d, x0, z0, cols, rows, scale, out) {
  const ax = x0 - scale, az = z0 - scale;
  const rx0 = ax >> 7, rz0 = az >> 7, rx1 = (x0 + cols * scale - 1) >> 7, rz1 = (z0 + rows * scale - 1) >> 7, rw = rx1 - rx0 + 1;
  // Zoomed out to 4 blocks per square or more, the regions' overviews (one cell per 4 x 4 columns) are enough, and
  // far smaller. The regions are looked up one per step of the job, so loading many never holds up a game tick.
  const coarse = scale >= 4, regs = [];
  for (let rz = rz0; rz <= rz1; rz++) {
    for (let rx = rx0; rx <= rx1; rx++) {
      regs.push(coarse ? overview(d, rx, rz) : region(d, rx, rz, false));
      yield;
    }
  }
  const half = scale >> 1, slopeDiv = Math.sqrt(scale);
  let sc = 0, sh = 0, sw = false; // the sampled square: colour (1 + 5-bit rgb, 0 = not mapped), height, water
  const sample = (x, z) => {
    const r = regs[((z >> 7) - rz0) * rw + (x >> 7) - rx0];
    if (!r) { sc = 0; return; }
    const i = coarse ? (((z & 127) >> 2) << 5) | ((x & 127) >> 2) : ((z & 127) << 7) | (x & 127);
    sc = r.c[i];
    sh = r.hw[i] & 511;
    sw = r.hw[i] > 511;
  };
  const sampleSquare = (x, z) => {
    if (scale === 1) return sample(x, z);
    let cr = 0, cg = 0, cb = 0, h = 0, k = 0, water = 0;
    for (let n = 0; n < 4; n++) {
      sample(x + (n & 1) * half, z + (n >> 1) * half);
      if (!sc) continue;
      const v = sc - 1;
      cr += v >> 10; cg += (v >> 5) & 31; cb += v & 31; h += sh; k++;
      if (sw) water++;
    }
    if (!k) { sc = 0; return; }
    sc = 1 + ((Math.round(cr / k) << 10) | (Math.round(cg / k) << 5) | Math.round(cb / k));
    sh = h / k;
    sw = water * 2 >= k;
  };
  let above = new Float64Array(cols + 1).fill(NaN), here = new Float64Array(cols + 1);
  for (let row = -1; row < rows; row++) {
    const z = z0 + row * scale, codes = row >= 0 ? new Uint16Array(cols) : null;
    for (let col = -1; col < cols; col++) {
      sampleSquare(x0 + col * scale, z);
      here[col + 1] = sc ? sh : NaN;
      if (!codes || col < 0) continue;
      if (!sc) { codes[col] = UNKNOWN; continue; }
      let s = 0;
      if (!sw) {
        const hn = above[col + 1], hw = here[col], n = hn === hn, w = hw === hw; // (NaN: not mapped)
        s = n && w ? 2 * sh - hn - hw : n ? 2 * (sh - hn) : w ? 2 * (sh - hw) : 0;
        s = Math.max(-MAX_SLOPE, Math.min(MAX_SLOPE, Math.round(s / slopeDiv)));
      }
      const base = (s + MAX_SLOPE) * 32, v = sc - 1;
      codes[col] = 0xe200 + PAL[(SH[base + (v >> 10)] << 10) | (SH[base + ((v >> 5) & 31)] << 5) | SH[base + (v & 31)]];
    }
    if (codes) out.push(codes);
    const t = above; above = here; here = t;
    yield;
  }
}

const ZOOMS = [1, 2, 4, 8, 16]; // blocks per map square
const MAP_ITEM = 'bmlive:map';
const MINI = 64; // minimap: 64 x 64 squares, one block each
const BIG_COLS = 256, BIG_ROWS = 144; // big map
// player id -> { mini, zoom (big map), big (opened with /map), open (opened with the Map item), lastUse, cleared, wasBig }
const opts = new Map();
function optsOf(p) {
  let o = opts.get(p.id);
  if (!o) {
    o = { mini: true, zoom: 0, big: false, open: false, lastUse: -100, cleared: false, wasBig: false };
    try { const s = p.getDynamicProperty('bmlive:opts'); if (typeof s === 'string') Object.assign(o, JSON.parse(s)); } catch {}
    o.big = false;
    opts.set(p.id, o);
  }
  return o;
}
function saveOpts(p, o) {
  try { p.setDynamicProperty('bmlive:opts', JSON.stringify({ mini: o.mini, zoom: o.zoom })); } catch {}
}
const dirOf = yaw => (Math.round((((yaw + 180) % 360) + 360) % 360 / 45) % 8); // 0 = north, clockwise
const zoomOf = o => ZOOMS[Math.max(0, Math.min(ZOOMS.length - 1, o.zoom | 0))];
const perSquare = s => s + (s === 1 ? ' block' : ' blocks') + ' per square';

// What a map shows right now: cols x rows squares centred on the player, and the other players on it. `key` changes
// when the drawing would.
function frame(p, cols, rows, scale) {
  const d = DIMS[p.dimension.id] ?? 0;
  const x0 = Math.floor(p.location.x) - (cols >> 1) * scale, z0 = Math.floor(p.location.z) - (rows >> 1) * scale;
  const marks = [];
  for (const o of world.getAllPlayers()) {
    try { if (o.id !== p.id && o.dimension.id === p.dimension.id) marks.push([Math.floor((o.location.x - x0) / scale), Math.floor((o.location.z - z0) / scale)]); } catch {}
  }
  return { d, x0, z0, cols, rows, scale, marks, key: [d, x0, z0, cols, rows, scale, mapRev, marks.join(';')].join('|') };
}
function* render(f) {
  const grid = [];
  yield* drawGrid(f.d, f.x0, f.z0, f.cols, f.rows, f.scale, grid);
  for (const [c, r] of f.marks) {
    for (const [dc, dr, ch] of [[0, -1, MARK_RING], [-1, 0, MARK_RING], [0, 0, MARK_DOT], [1, 0, MARK_RING], [0, 1, MARK_RING]]) {
      if (c + dc >= 0 && c + dc < f.cols && r + dr >= 0 && r + dr < f.rows) grid[r + dr][c + dc] = ch;
    }
  }
  return grid.map(row => String.fromCharCode.apply(null, row)).join('\n');
}

function holdingMap(p) {
  try {
    const it = p.getComponent('minecraft:equippable').getEquipment('Mainhand');
    return !!it && it.typeId === MAP_ITEM;
  } catch {
    return false;
  }
}

/* ---- status effects: a list down the minimap's left side ---- */

// The game's own effect icons cannot be laid out as a list (its renderer places them itself), so the pack lists the
// effects: a line each, with a bottle of the effect's colour (character U+E400 + its place in EFFECTS, from font page
// glyph_E4.png; the cell after the last is a grey bottle for effects missing there), its name and level, and the
// time left. The app fills EFFECTS in when it packages the add-on: [id, name, 1 if harmful].
const EFFECTS = /*BM:EFFECTS*/[['speed', 'Speed', 0], ['poison', 'Poison', 1]]/*BM:END*/;
const EFFECT_AT = new Map(EFFECTS.map((e, i) => [e[0], i]));
const ROMAN = ['I', 'II', 'III', 'IV', 'V', 'VI', 'VII', 'VIII', 'IX', 'X'];
const MAX_EFFECT_LINES = 12;
// One effect's line. The HUD puts the list on inventory grey, so: the bottle in its own colours (white text, §f,
// leaves a glyph's colours as they are), the name black (dark red when harmful), the time dark grey.
function effectLine(typeId, amplifier, duration) {
  const id = typeId.startsWith('minecraft:') ? typeId.slice(10) : typeId, i = EFFECT_AT.get(id);
  const name = i === undefined ? id.split('_').map((w, n) => (n && /^(of|the|and)$/.test(w) ? w : w.charAt(0).toUpperCase() + w.slice(1))).join(' ') : EFFECTS[i][1];
  const level = amplifier > 0 ? ' ' + (ROMAN[amplifier] || amplifier + 1) : '';
  const s = Math.ceil(duration / 20);
  const time = duration < 0 || s >= 6000 ? '**:**' : Math.floor(s / 60) + ':' + String(s % 60).padStart(2, '0'); // 100 min or more: as good as endless
  return '§f' + String.fromCharCode(0xe400 + (i ?? EFFECTS.length)) + ' ' + (i !== undefined && EFFECTS[i][2] ? '§4' : '§0') + name + level + ' §8' + time;
}
// A player's lines, or '' without effects.
function effectLines(p) {
  let list;
  try { list = p.getEffects(); } catch { return ''; }
  const lines = [];
  for (const e of list) {
    try { lines.push(effectLine(e.typeId, e.amplifier, e.duration)); } catch {}
  }
  if (lines.length > MAX_EFFECT_LINES) lines.splice(MAX_EFFECT_LINES - 1, lines.length, '§8+' + (lines.length - MAX_EFFECT_LINES + 1) + ' more');
  return lines.join('\n');
}
// The content log gets a line whenever a player's effects change (names and levels, not every tick of the timers),
// so a report of a missing list shows whether the game gave the script any effects. The app ignores these lines.
function logEffects(p, v, fx) {
  const now = fx.replace(/ §8[^\n]*/g, '');
  if (now === v.fxLogged) return;
  v.fxLogged = now;
  send('fx', { player: p.name, effects: now ? now.split('\n').map(l => l.replace(/§./g, '').slice(2)) : [] });
}
// The effect lines travel in the minimap's title, in a block of exactly FX_BYTES bytes (UTF-8) right after "bm:m:":
// the HUD cuts the title after 5 + FX_BYTES bytes ('%.605s' in hud_screen.json; format lengths in JSON UI count
// bytes). That is the short cut other packs make; cutting after the 12 KB minimap instead showed nothing in game.
// The block is padded with "§r" (3 bytes, not drawn), plus a space or two at the end of the first line (where they do
// not show) when the bytes do not come out even. Lines that do not fit become "+N more".
const FX_BYTES = 600;
const utf8Bytes = s => {
  let n = 0;
  for (let i = 0; i < s.length; i++) { const c = s.charCodeAt(i); n += c < 0x80 ? 1 : c < 0x800 ? 2 : 3; }
  return n;
};
function effectBlock(fx) {
  const lines = fx ? fx.split('\n') : [];
  let text = '';
  for (let i = 0; i < lines.length; i++) {
    const next = (text ? text + '\n' : '') + lines[i];
    if (utf8Bytes(next) + (i < lines.length - 1 ? 20 : 0) > FX_BYTES) { text += '\n§8+' + (lines.length - i) + ' more'; break; }
    text = next;
  }
  let pad = FX_BYTES - utf8Bytes(text);
  const odd = pad % 3, nl = text.indexOf('\n');
  if (odd) {
    text = nl < 0 ? text + ' '.repeat(odd) : text.slice(0, nl) + ' '.repeat(odd) + text.slice(nl);
    pad -= odd;
  }
  return text + '§r'.repeat(pad / 3);
}

// Both maps go out as one title, which the resource pack's HUD shows instead of a normal title: the title is "bm:"
// then "m:" + the effect block and the minimap (top-right corner) or "x", and the subtitle "bm:f:" + the big map
// (middle). Each map ends with "§#§" + the direction you face (0..7), which picks the arrow the HUD puts over the
// middle of the map (formatting codes are not drawn). Drawing runs as a job, one per player at a time; standing
// still costs nothing, and a title that has not changed is only sent again to keep it on screen.
const views = new Map(); // player id -> { busy, mini: { key, text }, big: { key, text }, sent, sentAt }
function viewOf(p) {
  let v = views.get(p.id);
  if (!v) views.set(p.id, (v = { busy: false, mini: { key: '', text: '' }, big: { key: '', text: '' }, sent: '', sentAt: 0, fxLogged: '' }));
  return v;
}
function show(p, v, mf, bf, dir, fx) {
  const want = (mf ? v.mini.key + '#' + fx : '-') + '#' + (bf ? v.big.key : '-') + '#' + dir;
  if (want === v.sent && tickNow - v.sentAt < 30) return;
  const mark = '§#§' + dir;
  p.onScreenDisplay.setTitle('bm:' + (mf ? 'm:' + effectBlock(fx) + v.mini.text + mark : 'x'), { subtitle: bf ? 'bm:f:' + v.big.text + mark : '', fadeInDuration: 0, stayDuration: 60, fadeOutDuration: 0 });
  v.sent = want;
  v.sentAt = tickNow;
}
function* redraw(p, v, mf, bf, fx) {
  try {
    if (mf && mf.key !== v.mini.key) v.mini = { key: mf.key, text: yield* render(mf) };
    if (bf && bf.key !== v.big.key) v.big = { key: bf.key, text: yield* render(bf) };
    show(p, v, mf, bf, dirOf(p.getRotation().y), fx);
  } catch {
    // the player left
  } finally {
    v.busy = false;
  }
}
// /maptest: a test pattern in place of the big map. First the 256 map colours, 16 rows of 16 (each twice); then
// three rows of 32 characters that should end in the same place: one colour, two colours from the map page, and the
// map page alternating with a second font page (magenta). If that last row ends somewhere else, switching pages
// moves text, which is why the map uses one page. The minimap stays (a grass square if it was off), with three
// made-up effects listed beside it, so the effect list can be checked without drinking anything.
const TEST_GRASS = 0xe200 + PAL[(SH[MAX_SLOPE * 32 + 13] << 10) | (SH[MAX_SLOPE * 32 + 19] << 5) | SH[MAX_SLOPE * 32 + 7]];
const TEST_TEXT = (() => {
  const row = codes => String.fromCharCode(...codes);
  const palette = Array.from({ length: 16 }, (_, r) => row(Array.from({ length: 32 }, (_, i) => 0xe200 + r * 16 + (i >> 1))));
  const alt = (a, b) => row(Array.from({ length: 32 }, (_, i) => (i & 1 ? b : a)));
  return [...palette, alt(UNKNOWN, UNKNOWN), alt(TEST_GRASS, TEST_GRASS), alt(TEST_GRASS, MARK_RING), alt(TEST_GRASS, 0xe300)].join('\n');
})();
const TEST_MINI = Array.from({ length: MINI }, () => String.fromCharCode(TEST_GRASS).repeat(MINI)).join('\n');
const TEST_EFFECTS = [effectLine('minecraft:speed', 1, 1660), effectLine('minecraft:poison', 0, 900), effectLine('minecraft:village_hero', 0, -1)].join('\n');
function drawMaps() {
  for (const p of world.getAllPlayers()) {
    try {
      const o = optsOf(p), v = viewOf(p);
      if (o.open && !holdingMap(p)) o.open = false; // put away: closed, and scrolling back over it leaves it closed
      const big = o.big || o.open;
      if (o.test) {
        if (tickNow - v.sentAt >= 20) {
          p.onScreenDisplay.setTitle('bm:m:' + effectBlock(TEST_EFFECTS) + (v.mini.text || TEST_MINI) + '§#§0', { subtitle: 'bm:f:' + TEST_TEXT, fadeInDuration: 0, stayDuration: 60, fadeOutDuration: 0 });
          v.sent = '';
          v.sentAt = tickNow;
        }
        continue;
      }
      if (big && !o.wasBig) p.onScreenDisplay.setActionBar('Map: ' + perSquare(zoomOf(o)) + ' · ' + (USE[platformOf(p)] || USE.Desktop));
      o.wasBig = big;
      if (!o.mini && !big) {
        if (!o.cleared) { p.onScreenDisplay.clearTitle(); o.cleared = true; v.sent = ''; }
        continue;
      }
      o.cleared = false;
      if (v.busy) continue;
      const mf = o.mini ? frame(p, MINI, MINI, 1) : null;
      const bf = big ? frame(p, BIG_COLS, BIG_ROWS, zoomOf(o)) : null;
      const fx = mf ? effectLines(p) : '';
      logEffects(p, v, fx);
      if ((mf && mf.key !== v.mini.key) || (bf && bf.key !== v.big.key)) {
        v.busy = true;
        system.runJob(redraw(p, v, mf, bf, fx));
      } else show(p, v, mf, bf, dirOf(p.getRotation().y), fx);
    } catch {}
  }
}

function setMini(p, on) {
  const o = optsOf(p);
  o.mini = on;
  saveOpts(p, o);
}
function zoomOut(p) {
  const o = optsOf(p);
  o.zoom = (o.zoom + 1) % ZOOMS.length; // 1, 2, 4, 8, 16 blocks per square, then back to 1
  saveOpts(p, o);
  try { p.onScreenDisplay.setActionBar('Map: ' + perSquare(zoomOf(o))); } catch {}
}
// Using the Map opens the big map and using it again closes it (however it was opened, /map too); using it while
// crouching zooms out (opening the map if it was closed). Holding the button down can repeat the use every few
// ticks, so a use within 6 ticks of the last one is the same press.
function useMap(p) {
  const o = optsOf(p), now = system.currentTick, again = now - o.lastUse < 6;
  o.lastUse = now;
  if (again) return;
  let crouching = false;
  try { crouching = !!p.isSneaking; } catch {}
  if (crouching) {
    zoomOut(p);
    o.open = true;
    o.wasBig = true; // the zoom message is enough; no "Map: ..." hint on top of it
  } else if (o.open || o.big) o.open = o.big = false;
  else o.open = true;
}

// How to pick the Map and use it, in the words of the player's device (consoles have no number keys, phones no
// right click). PlatformType is "Console", "Desktop" or "Mobile".
function platformOf(p) {
  try { return String(p.clientSystemInfo.platformType); } catch { return 'Desktop'; }
}
const PICK = { Desktop: ' It is in hotbar slot 9.', Console: ' It is in the last hotbar slot.', Mobile: ' It is in the last hotbar slot.' };
const GUIDE = {
  Desktop: 'right-click the Map to open the big map, right-click again to close it, and crouch + right-click to zoom out',
  Console: 'use the Map (left trigger) to open the big map, use it again to close it, and crouch while using it to zoom out',
  Mobile: 'use the Map to open the big map, use it again to close it, and crouch while using it to zoom out'
};
const USE = { Desktop: 'right-click to close, crouch + right-click to zoom out', Console: 'use again to close, crouch + use to zoom out', Mobile: 'use again to close, crouch + use to zoom out' };

// Everyone gets one Map item, once, in the last hotbar slot when it is free. Using it shows the big map (add-ons
// cannot bind keys), so picking it in the hotbar shows nothing on its own. It can also be crafted from a compass and
// paper.
function giveMap(p) {
  try {
    if (p.getDynamicProperty('bmlive:gave_map')) return;
    const inv = p.getComponent('minecraft:inventory').container;
    for (let i = 0; i < inv.size; i++) if (inv.getItem(i)?.typeId === MAP_ITEM) { p.setDynamicProperty('bmlive:gave_map', true); return; }
    let where = '';
    if (!inv.getItem(8) && p.selectedSlotIndex !== 8) { inv.setItem(8, new ItemStack(MAP_ITEM, 1)); where = PICK[platformOf(p)] || PICK.Desktop; }
    else if (inv.emptySlotsCount > 0) inv.addItem(new ItemStack(MAP_ITEM, 1));
    else return;
    p.setDynamicProperty('bmlive:gave_map', true);
    p.sendMessage('§eBedrock Map Live:§r ' + (GUIDE[platformOf(p)] || GUIDE.Desktop) + '.' + where + ' Typing /map and /minimap in chat works too.');
  } catch {}
}

/* ---- commands: /map and /minimap (typed in chat, any platform) ---- */

system.beforeEvents.startup.subscribe(init => {
  const reg = init.customCommandRegistry;
  const player = origin => {
    const e = origin.sourceEntity || origin.initiator;
    return e && e.typeId === 'minecraft:player' ? e : null;
  };
  reg.registerCommand({ name: 'bmlive:map', description: 'Show or hide the big map', permissionLevel: CommandPermissionLevel.Any, cheatsRequired: false }, origin => {
    const p = player(origin);
    if (!p) return { status: CustomCommandStatus.Failure, message: 'Only players have a map.' };
    system.run(() => { const o = optsOf(p); o.big = !o.big; drawMaps(); });
    return { status: CustomCommandStatus.Success };
  });
  reg.registerCommand({ name: 'bmlive:maptest', description: 'Show or hide a test pattern for the map font', permissionLevel: CommandPermissionLevel.Any, cheatsRequired: false }, origin => {
    const p = player(origin);
    if (!p) return { status: CustomCommandStatus.Failure, message: 'Only players have a map.' };
    system.run(() => { const o = optsOf(p); o.test = !o.test; viewOf(p).sentAt = -100; drawMaps(); });
    return { status: CustomCommandStatus.Success };
  });
  reg.registerCommand({ name: 'bmlive:minimap', description: 'Show or hide the minimap', permissionLevel: CommandPermissionLevel.Any, cheatsRequired: false }, origin => {
    const p = player(origin);
    if (!p) return { status: CustomCommandStatus.Failure, message: 'Only players have a minimap.' };
    system.run(() => { setMini(p, !optsOf(p).mini); drawMaps(); });
    return { status: CustomCommandStatus.Success };
  });
});

/* ---- events ---- */

function on(signal, fn) {
  try { if (signal) signal.subscribe(fn); } catch {}
}
const after = world.afterEvents;
on(after.playerBreakBlock, e => changed(e.block.dimension, e.block.location));
on(after.playerPlaceBlock, e => changed(e.block.dimension, e.block.location));
on(after.blockExplode, e => changed(e.dimension, e.block.location));
on(after.pistonActivate, e => changed(e.dimension, e.block.location));
// Using the Map (right click, the left trigger, or a tap) opens or closes the big map, or zooms out while crouching.
on(after.itemUse, e => {
  if (e.itemStack && e.itemStack.typeId === MAP_ITEM && e.source && e.source.typeId === 'minecraft:player') system.run(() => { useMap(e.source); drawMaps(); });
});
on(after.playerSpawn, e => { if (e.initialSpawn) system.runTimeout(() => giveMap(e.player), 40); });
on(after.entityDie, e => {
  try {
    if (e.deadEntity.typeId !== 'minecraft:player') return;
    const p = e.deadEntity, l = p.location, cause = e.damageSource && e.damageSource.cause;
    push({ type: 'event', kind: 'death', player: p.name, text: 'died' + (cause ? ' (' + cause + ')' : ''), dim: DIMS[p.dimension.id] ?? 0, x: Math.floor(l.x), y: Math.floor(l.y), z: Math.floor(l.z) });
  } catch {}
});
on(after.playerLeave, e => {
  opts.delete(e.playerId);
  views.delete(e.playerId);
  saveSoon(); // what they explored is kept even if the host closes the world right after
});
on(system.beforeEvents && system.beforeEvents.shutdown, () => {
  saveAll(); // the world is closing: keep everything not saved yet
  send('bye', {});
});

/* ---- timers (started once the world has loaded) ---- */

let started = false;
function start() {
  if (started) return;
  started = true;
  system.runTimeout(hello, 20);
  system.runInterval(hello, 600); // again every 30 s, so an app started mid-game still learns about this world
  system.runInterval(() => push({ type: 'players', players: players() }), 20);
  system.runInterval(() => nearPlayers(EXPLORE, false), 40);
  system.runInterval(() => nearPlayers(REFRESH, true), 600);
  system.runInterval(() => {
    try { push({ type: 'world', time: world.getTimeOfDay(), day: world.getDay() }); } catch {}
  }, 200);
  system.runInterval(kick, 10);
  system.runTimeout(dropOldMaps, 60);
  system.runInterval(() => { tickNow = system.currentTick; drawMaps(); }, 4);
  system.runInterval(saveSoon, 200);
}
on(after.worldLoad, start);
system.runTimeout(start, 100); // in case the world had already loaded (for example after /reload)
