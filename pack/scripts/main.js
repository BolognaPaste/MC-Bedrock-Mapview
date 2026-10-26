// Bedrock Map Live: sends online players and changed chunks to the Bedrock Map app on this PC.
//
// A world pack cannot use the network outside a dedicated server, so every message is written to the game's
// content log instead (Settings > Creator > Content Log File), which the app reads as it grows. Each line is
// "BMLIVE1 " followed by JSON in the app's live link protocol (docs/live-protocol.md in the app's source).
import { world, system } from '@minecraft/server';

const TAG = 'BMLIVE1 ';
const VERSION = '1.0.1';
const DIMS = { 'minecraft:overworld': 0, 'minecraft:nether': 1, 'minecraft:the_end': 2 };
const EXPLORE_RADIUS = 5; // chunks around each player that are sent once per session, so new ground shows up
const REFRESH_RADIUS = 1; // chunks around each player sent again every 30 s (growth, fire, water, mobs...)
const CHANGE_DELAY = 30; // ticks to wait after a block change before sending its chunk (batches quick edits)
const MAX_WATER = 16; // deeper water looks the same on the map, so stop counting there
const MAX_QUEUE = 600;

// Blocks the map looks through (plants, torches, glass...). The app fills these in from its own colour table
// when it packages the pack, so the pack and the map agree.
const SKIP = new Set(/*BM:SKIP*/['air', 'cave_air', 'void_air', 'short_grass', 'tall_grass', 'torch', 'seagrass', 'kelp']/*BM:END*/);
const SKIP_RE = new RegExp(/*BM:SKIPRE*/'(_sapling|_torch|_button|_sign|_glass(_pane)?|_door|_trapdoor|_rail)$'/*BM:END*/);

const session = Math.random().toString(36).slice(2, 10) + Date.now().toString(36).slice(-4);
const send = (t, body) => console.warn(TAG + JSON.stringify(Object.assign({ s: session, t }, body)));
const push = (...m) => send('push', { m });

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

/* ---- chunks ---- */

const queue = new Map(); // "dimension,cx,cz" -> { dim, cx, cz, at }: chunks waiting to be scanned
const sent = new Set(); // chunks already sent this session (exploring sends each one once)
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
  // Nothing hit: an empty column (the End's void) if the chunk is loaded, otherwise unknown.
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

// Scans one chunk into a compact chunk message ({ blocks: names, columns: [y, name index, water?] }).
// Yields every 16 columns so the work is spread over several ticks.
function* scan(job) {
  const { dim, cx, cz } = job;
  let minY = -64, maxY = 320;
  try { minY = dim.heightRange.min; maxY = dim.heightRange.max; } catch {}
  const blocks = [], index = new Map(), columns = new Array(256);
  let loaded = 0;
  for (let lz = 0; lz < 16; lz++) {
    for (let lx = 0; lx < 16; lx++) {
      let c = null;
      try { c = column(dim, cx * 16 + lx, cz * 16 + lz, minY, maxY); } catch { c = null; }
      if (c) {
        loaded++;
        let i = index.get(c.block);
        if (i === undefined) { i = blocks.length; blocks.push(c.block); index.set(c.block, i); }
        columns[lz * 16 + lx] = c.water ? [c.y, i, c.water] : [c.y, i];
      } else columns[lz * 16 + lx] = null;
    }
    yield;
  }
  if (!loaded) return; // not loaded right now; exploring asks again later
  sent.add(keyOf(dim.id, cx, cz));
  push({ type: 'chunk', dim: DIMS[dim.id] ?? 0, cx, cz, blocks, columns });
}

// Works through the chunks that are due, a little each tick (system.runJob), or a few per run on older games.
let busy = false, current = null;
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
  if (system.runJob) {
    if (!busy && queue.size) { busy = true; system.runJob(work()); }
    return;
  }
  if (!current && queue.size) current = work();
  if (current) for (let i = 0; i < 32; i++) if (current.next().done) { current = null; break; }
}

/* ---- events ---- */

function on(signal, fn) {
  try { if (signal) signal.subscribe(fn); } catch {}
}
const after = world.afterEvents;
on(after.playerBreakBlock, e => changed(e.block.dimension, e.block.location));
on(after.playerPlaceBlock, e => changed(e.block.dimension, e.block.location));
on(after.blockExplode, e => changed(e.dimension, e.block.location));
on(after.pistonActivate, e => changed(e.dimension, e.block.location));
on(after.entityDie, e => {
  try {
    if (e.deadEntity.typeId !== 'minecraft:player') return;
    const p = e.deadEntity, l = p.location, cause = e.damageSource && e.damageSource.cause;
    push({ type: 'event', kind: 'death', player: p.name, text: 'died' + (cause ? ' (' + cause + ')' : ''), dim: DIMS[p.dimension.id] ?? 0, x: Math.floor(l.x), y: Math.floor(l.y), z: Math.floor(l.z) });
  } catch {}
});
on(system.beforeEvents && system.beforeEvents.shutdown, () => send('bye', {}));

/* ---- timers ---- */

system.runTimeout(hello, 20);
system.runInterval(hello, 600); // again every 30 s, so an app started mid-game still learns about this world
system.runInterval(() => push({ type: 'players', players: players() }), 20);
system.runInterval(() => nearPlayers(EXPLORE, false), 40);
system.runInterval(() => nearPlayers(REFRESH, true), 600);
system.runInterval(() => {
  try { push({ type: 'world', time: world.getTimeOfDay(), day: world.getDay() }); } catch {}
}, 200);
system.runInterval(kick, 10);
