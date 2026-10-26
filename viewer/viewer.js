(() => {
  'use strict';
  const TILE = 512, RULER = 22;
  const FONT = 'Bahnschrift, "Segoe UI Variable", "Segoe UI", system-ui, sans-serif';
  const STEPS = [16, 32, 64, 128, 256, 512, 1024, 2048, 4096, 8192, 16384, 32768, 65536];
  const $ = id => document.getElementById(id);
  const cv = $('map'), ctx = cv.getContext('2d'), mini = $('mini'), mctx = mini.getContext('2d');
  const reduce = window.matchMedia('(prefers-reduced-motion: reduce)');
  let safe = { top: 0, bottom: 9999 }, meta = null, dim = null, view = { x: 0, z: 0, s: 1 }, dpr = 1, W = 0, H = 0, queued = false, fly = null, pal = {}, box = null, mm = null;
  const mouse = { x: 0, y: 0, in: false };
  const tiles = new Map();
  let layer = 'surface'; // 'surface' or 'caves' (caves are drawn over a dimmed surface)
  // 3D view: the same view.x / view.z / view.s, seen at an angle. yaw 0 looks north; pitch is the angle below the horizon.
  const TOP_DOWN = 1.55, PITCH = 0.95;
  let mode = '2d', m3 = null, tween = null;
  const orbit = { yaw: 0, pitch: PITCH };

  const say = t => { const m = $('msg'); m.hidden = !t; m.textContent = t || ''; };
  const draw = () => { if (queued) return; queued = true; requestAnimationFrame(() => { queued = false; paint(); }); };
  const fmt = n => Math.round(n).toLocaleString('en-US');
  const label = n => n.charAt(0).toUpperCase() + n.slice(1);
  const esc = t => String(t).replace(/[&<>"']/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
  const MODES = { 0: 'Survival', 1: 'Creative', 2: 'Adventure', 5: 'Default', 6: 'Spectator' };
  const DIFFS = ['Peaceful', 'Easy', 'Normal', 'Hard'];
  const DIM_NAME = { 0: 'Overworld', 1: 'Nether', 2: 'The End' };
  const COMPASS = ['E', 'SE', 'S', 'SW', 'W', 'NW', 'N', 'NE']; // +X is east, +Z is south
  let last = { x: 0, z: 0 }, cursorW = null;

  // "412 blocks NE": how far, and which way, p lies from the point (x, z).
  function whereIs(p, x, z) {
    const dx = p.x - x, dz = p.z - z, d = Math.hypot(dx, dz);
    if (d < 1) return 'right here';
    return fmt(d) + ' blocks ' + COMPASS[(Math.round(Math.atan2(dz, dx) / (Math.PI / 4)) + 8) % 8];
  }

  function readPalette() {
    const cs = getComputedStyle(document.documentElement), g = n => cs.getPropertyValue(n).trim();
    pal = { void: g('--void'), deck: g('--deck'), line: g('--line'), dim: g('--dim'), acc: g('--acc') };
  }

  // Revision of every file the live link redrew (from live.json), so the newest copy is fetched.
  const fileRev = {};
  const revUrl = id => id + '.png' + (fileRev[id] ? '?v=' + fileRev[id] : '');

  function tile(d, k, x, z, group = 'tiles') {
    const id = group + '/' + d + '/' + k + '/' + x + '_' + z;
    let t = tiles.get(id);
    if (!t && dim.sets && !(dim.sets[k] && dim.sets[k].has(x + ',' + z))) return { ok: false, bad: true };
    if (!t) {
      t = { img: new Image(), ok: false, rev: fileRev[id] };
      t.img.onload = () => { t.ok = true; draw(); };
      t.img.onerror = () => { t.bad = true; };
      t.img.src = revUrl(id);
      tiles.set(id, t);
      if (tiles.size > 1200) tiles.delete(tiles.keys().next().value);
    } else if (fileRev[id] && t.rev !== fileRev[id]) {
      // Redrawn by the live link: keep showing the old picture until the new one has loaded.
      t.rev = fileRev[id];
      const img = new Image();
      img.onload = () => { t.img = img; t.ok = true; t.bad = false; if (k === 0 && m3) m3.invalidate(x, z); draw(); };
      img.src = revUrl(id);
    }
    return t;
  }

  /* ---- surface height (Y) under the cursor, read from heights/<dim>/<x>_<z>.png ---- */

  const HEIGHT_BIAS = 1024, NO_HEIGHT = -32768, hmaps = new Map();
  // One height tile as { h: Int16Array (NO_HEIGHT where nothing was drawn), min, max, bad }; h is null while it loads.
  // null when the map has no such tile (or no heights at all).
  function heightTile(tx, tz) {
    if (!dim || !dim.heights) return null; // map built before height tracking: rebuild to get it
    if (dim.sets && !(dim.sets[0] && dim.sets[0].has(tx + ',' + tz))) return null;
    const id = dim.name + '/' + tx + '_' + tz;
    let t = hmaps.get(id);
    const rev = fileRev['heights/' + id];
    if (!t) {
      t = { h: null, min: 0, max: 0, bad: false, rev };
      loadHeights(t, id, tx, tz);
      hmaps.set(id, t);
      if (hmaps.size > 160) hmaps.delete(hmaps.keys().next().value);
    } else if (rev && t.rev !== rev) {
      t.rev = rev; // redrawn by the live link: the old heights stay in use until the new ones have loaded
      loadHeights(t, id, tx, tz);
    }
    return t;
  }
  function loadHeights(t, id, tx, tz) {
    const img = new Image();
    img.onload = () => {
      try {
        const c = document.createElement('canvas'); c.width = c.height = TILE;
        const g = c.getContext('2d', { willReadFrequently: true });
        g.drawImage(img, 0, 0);
        const px = g.getImageData(0, 0, TILE, TILE).data, h = new Int16Array(TILE * TILE);
        let lo = Infinity, hi = -Infinity;
        for (let i = 0; i < h.length; i++) {
          if (px[i * 4 + 3] !== 255) { h[i] = NO_HEIGHT; continue; }
          const v = ((px[i * 4] << 8) | px[i * 4 + 1]) - HEIGHT_BIAS;
          h[i] = v; if (v < lo) lo = v; if (v > hi) hi = v;
        }
        const reload = !!t.h;
        t.min = lo === Infinity ? 0 : lo; t.max = hi === -Infinity ? 0 : hi; t.h = h; t.bad = false;
        if (reload && m3) m3.invalidate(tx, tz);
      } catch { t.bad = true; }
      draw();
    };
    img.onerror = () => { if (!t.h) t.bad = true; };
    img.src = revUrl('heights/' + id);
  }
  function heightAt(bx, bz) {
    if (!dim || !dim.heights) return undefined;
    const tx = Math.floor(bx / TILE), tz = Math.floor(bz / TILE), t = heightTile(tx, tz);
    if (t === null) return null;
    if (t.bad || !t.h) return undefined; // failed, or still loading
    const v = t.h[(bz - tz * TILE) * TILE + (bx - tx * TILE)];
    return v === NO_HEIGHT ? null : v;
  }

  function resize() {
    dpr = window.devicePixelRatio || 1;
    W = window.innerWidth; H = window.innerHeight;
    cv.width = Math.round(W * dpr); cv.height = Math.round(H * dpr);
    // free area for off-screen pins: between the top and bottom HUD rows
    const t = document.querySelector('.top').getBoundingClientRect(), b = document.querySelector('.bottom').getBoundingClientRect();
    safe = { top: t.bottom + 16, bottom: b.top - 16 };
    if (!$('info').hidden) placeInfo();
    draw();
  }

  const toScreen = (x, z) => [W / 2 + (x - view.x) * view.s, H / 2 + (z - view.z) * view.s];
  const toWorld = (px, py) => [view.x + (px - W / 2) / view.s, view.z + (py - H / 2) / view.s];
  // Mode-aware versions: in 3D a screen point is traced to the ground, and a world point needs a height.
  function screenToWorld(px, py) {
    if (mode !== '3d') return toWorld(px, py);
    const p = m3.pick(px, py);
    return p ? [p.x, p.z] : null;
  }
  function yOf(p) { // spawn can be saved with Y 32767 (unknown): then use the ground there
    if (typeof p.y === 'number' && Math.abs(p.y) < 2000) return p.y;
    const h = heightAt(Math.floor(p.x), Math.floor(p.z));
    return typeof h === 'number' ? h + 1 : 64;
  }
  const scaleLimits = () => (mode === '3d' ? [Math.max(0.01, m3.minScale(H)), m3.maxScale] : [0.01, 32]);
  const clampScale = s => { const [lo, hi] = scaleLimits(); return Math.max(lo, Math.min(hi, s)); };
  const gridStep = () => STEPS.find(v => v * view.s >= 70) || STEPS[STEPS.length - 1];

  /* ---- drawing ---- */

  function graticule(x0, z0, x1, z1) {
    const step = gridStep();
    ctx.lineWidth = 1; ctx.strokeStyle = pal.acc;
    ctx.globalAlpha = 0.06; ctx.beginPath();
    for (let x = Math.ceil(x0 / step) * step; x <= x1; x += step) { const sx = Math.round(toScreen(x, 0)[0]) + .5; ctx.moveTo(sx, 0); ctx.lineTo(sx, H); }
    for (let z = Math.ceil(z0 / step) * step; z <= z1; z += step) { const sy = Math.round(toScreen(0, z)[1]) + .5; ctx.moveTo(0, sy); ctx.lineTo(W, sy); }
    ctx.stroke();
    ctx.globalAlpha = 0.4; ctx.beginPath();
    for (let x = Math.ceil(x0 / step) * step; x <= x1; x += step) {
      for (let z = Math.ceil(z0 / step) * step; z <= z1; z += step) {
        const [sx, sy] = toScreen(x, z), px = Math.round(sx) + .5, py = Math.round(sy) + .5;
        ctx.moveTo(px - 4, py); ctx.lineTo(px + 5, py); ctx.moveTo(px, py - 4); ctx.lineTo(px, py + 5);
      }
    }
    ctx.stroke();
    // world axes
    ctx.globalAlpha = 0.28; ctx.beginPath();
    const [ax, az] = toScreen(0, 0);
    if (ax > 0 && ax < W) { ctx.moveTo(Math.round(ax) + .5, 0); ctx.lineTo(Math.round(ax) + .5, H); }
    if (az > 0 && az < H) { ctx.moveTo(0, Math.round(az) + .5); ctx.lineTo(W, Math.round(az) + .5); }
    ctx.stroke();
    ctx.globalAlpha = 1;
  }

  function drawTiles(x0, z0, x1, z1) {
    ctx.imageSmoothingEnabled = view.s < 1;
    const k = Math.max(0, Math.min(dim.maxZoom, Math.floor(Math.log2(1 / Math.min(view.s, 1)))));
    const span = TILE * 2 ** k;
    for (let tz = Math.floor(z0 / span); tz <= Math.floor(z1 / span); tz++) {
      for (let tx = Math.floor(x0 / span); tx <= Math.floor(x1 / span); tx++) {
        const t = tile(dim.name, k, tx, tz);
        if (!t.ok) continue;
        const [sx, sy] = toScreen(tx * span, tz * span), size = span * view.s;
        if (layer === 'caves') ctx.globalAlpha = 0.3;
        ctx.drawImage(t.img, Math.floor(sx), Math.floor(sy), Math.ceil(size) + 1, Math.ceil(size) + 1);
        ctx.globalAlpha = 1;
        if (layer === 'caves') {
          const c = tile(dim.name, k, tx, tz, 'caves');
          if (c.ok) ctx.drawImage(c.img, Math.floor(sx), Math.floor(sy), Math.ceil(size) + 1, Math.ceil(size) + 1);
        }
      }
    }
  }

  function chunkGrid(x0, z0, x1, z1) {
    ctx.strokeStyle = pal.acc; ctx.globalAlpha = 0.22; ctx.lineWidth = 1; ctx.beginPath();
    for (let x = Math.floor(x0 / 16) * 16; x <= x1; x += 16) { const [sx] = toScreen(x, 0); ctx.moveTo(Math.round(sx) + .5, 0); ctx.lineTo(Math.round(sx) + .5, H); }
    for (let z = Math.floor(z0 / 16) * 16; z <= z1; z += 16) { const [, sy] = toScreen(0, z); ctx.moveTo(0, Math.round(sy) + .5); ctx.lineTo(W, Math.round(sy) + .5); }
    ctx.stroke(); ctx.globalAlpha = 1;
  }

  function rulers(x0, z0, x1, z1) {
    const step = gridStep(), minor = step / 4;
    ctx.save();
    ctx.globalAlpha = 0.95; ctx.fillStyle = pal.deck;
    ctx.fillRect(0, 0, W, RULER); ctx.fillRect(0, 0, RULER, H);
    ctx.globalAlpha = 1; ctx.strokeStyle = pal.line; ctx.lineWidth = 1;
    ctx.beginPath(); ctx.moveTo(0, RULER + .5); ctx.lineTo(W, RULER + .5); ctx.moveTo(RULER + .5, 0); ctx.lineTo(RULER + .5, H); ctx.stroke();
    ctx.strokeStyle = pal.dim; ctx.fillStyle = pal.dim; ctx.font = '10.5px ' + FONT; ctx.textBaseline = 'alphabetic';
    ctx.beginPath();
    const majors = [];
    for (let x = Math.ceil(x0 / minor) * minor; x <= x1; x += minor) {
      const sx = Math.round(toScreen(x, 0)[0]) + .5;
      if (sx < RULER) continue;
      const major = x % step === 0;
      ctx.moveTo(sx, RULER); ctx.lineTo(sx, RULER - (major ? 10 : 4));
      if (major) ctx.fillText(String(x), sx + 3, 11);
    }
    for (let z = Math.ceil(z0 / minor) * minor; z <= z1; z += minor) {
      const sy = Math.round(toScreen(0, z)[1]) + .5;
      if (sy < RULER) continue;
      const major = z % step === 0;
      ctx.moveTo(RULER, sy); ctx.lineTo(RULER - (major ? 10 : 4), sy);
      if (major) majors.push([z, sy]);
    }
    ctx.stroke();
    ctx.textAlign = 'right';
    for (const [z, sy] of majors) { ctx.save(); ctx.translate(11, sy + 3); ctx.rotate(-Math.PI / 2); ctx.fillText(String(z), 0, 0); ctx.restore(); }
    ctx.textAlign = 'left';
    // corner cap
    ctx.fillStyle = pal.deck; ctx.fillRect(0, 0, RULER, RULER);
    ctx.strokeStyle = pal.acc; ctx.beginPath(); ctx.moveTo(RULER / 2, 5); ctx.lineTo(RULER / 2, RULER - 5); ctx.moveTo(5, RULER / 2); ctx.lineTo(RULER - 5, RULER / 2); ctx.stroke();
    // cursor trace
    if (mouse.in) {
      ctx.strokeStyle = pal.acc; ctx.lineWidth = 1.5; ctx.beginPath();
      ctx.moveTo(Math.round(mouse.x) + .5, 0); ctx.lineTo(Math.round(mouse.x) + .5, RULER);
      ctx.moveTo(0, Math.round(mouse.y) + .5); ctx.lineTo(RULER, Math.round(mouse.y) + .5);
      ctx.stroke();
    }
    ctx.restore();
  }

  function reticle() {
    if (!mouse.in || !$('reticle').checked) return;
    ctx.save();
    ctx.strokeStyle = pal.acc; ctx.globalAlpha = 0.28; ctx.lineWidth = 1; ctx.setLineDash([3, 5]);
    ctx.beginPath();
    ctx.moveTo(Math.round(mouse.x) + .5, RULER); ctx.lineTo(Math.round(mouse.x) + .5, H);
    ctx.moveTo(RULER, Math.round(mouse.y) + .5); ctx.lineTo(W, Math.round(mouse.y) + .5);
    ctx.stroke(); ctx.restore();
  }

  function placePin(el, p, name, active) {
    if (!p || !active) { el.hidden = true; return; }
    el.hidden = false;
    let sx, sy, behind = false;
    if (mode === '3d') { const q = m3.project(p.x, yOf(p), p.z); sx = q.x; sy = q.y; behind = q.behind; }
    else [sx, sy] = toScreen(p.x, p.z);
    const m = 30, lo = RULER + m;
    const top = Math.max(lo, safe.top), bot = Math.min(H - m, safe.bottom);
    const off = behind || sx < lo - 10 || sx > W - m || sy < top - 10 || sy > bot + 10;
    let x = sx, y = sy, text = name;
    if (off) {
      x = Math.max(lo + 6, Math.min(W - m, sx)); y = Math.max(top, Math.min(bot, sy));
      el.style.setProperty('--a', Math.atan2(sy - y, sx - x) + 'rad');
      const d = Math.hypot(p.x - view.x, p.z - view.z);
      text = name + ' ' + (d >= 1000 ? (d / 1000).toFixed(1) + 'k' : Math.round(d));
    }
    el.querySelector('.hit').title = name + ': ' + fmt(p.x) + ', ' + fmt(p.z);
    el.classList.toggle('off', off);
    el.classList.toggle('flip', x > W - 150);
    el.querySelector('.tag').textContent = text;
    el.style.transform = 'translate(' + Math.round(x) + 'px,' + Math.round(y) + 'px)';
  }

  function readout(wx, wz, fromCursor) {
    if (!dim) return;
    last = { x: Math.floor(wx), z: Math.floor(wz) };
    $('rx').textContent = last.x; $('rz').textContent = last.z;
    $('rsrc').textContent = fromCursor ? 'Cursor' : 'Centre';
    const yv = heightAt(last.x, last.z);
    last.y = typeof yv === 'number' ? yv : null;
    $('ry').textContent = !dim.heights ? 'n/a' : yv === undefined ? '…' : yv === null ? '–' : yv;
    $('ry').title = dim.heights ? 'Surface height (top block) at this spot' : 'Rebuild this map to track height';
    $('rc').textContent = Math.floor(wx / 16) + ', ' + Math.floor(wz / 16);
    $('ri').textContent = (last.x & 15) + ', ' + (last.z & 15);
    // Nether coordinates are 1/8 of the overworld's, so the same spot has a different number in the other dimension.
    const other = $('rother');
    other.hidden = dim.dim > 1;
    if (!other.hidden) {
      const f = dim.dim === 0 ? 1 / 8 : 8;
      $('rotherL').textContent = dim.dim === 0 ? 'In Nether' : 'In Overworld';
      $('rotherV').textContent = fmt(Math.floor(wx * f)) + ', ' + fmt(Math.floor(wz * f));
    }
    const sp = $('rsp'), pl = $('rpl');
    sp.hidden = !(meta.spawn && dim.dim === 0);
    if (!sp.hidden) $('rspv').textContent = whereIs(meta.spawn, wx, wz);
    pl.hidden = !(meta.player && meta.player.dim === dim.dim);
    if (!pl.hidden) $('rplv').textContent = whereIs(meta.player, wx, wz);
  }

  function scaleBar() {
    const want = 110 / view.s;
    const pow = 10 ** Math.floor(Math.log10(want)), f = want / pow;
    const blocks = (f >= 5 ? 5 : f >= 2 ? 2 : 1) * pow;
    $('scaleBar').style.width = Math.round(blocks * view.s) + 'px';
    $('scaleTxt').textContent = fmt(blocks) + ' blocks';
    const z = view.s;
    $('rs').textContent = (z >= 10 ? Math.round(z) : +z.toFixed(2)) + '×';
  }

  function paint() {
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    if (mode === '3d' && dim) { paint3d(); return; }
    ctx.fillStyle = pal.void || '#05080f';
    ctx.fillRect(0, 0, W, H);
    if (!dim) return;
    const [x0, z0] = toWorld(0, 0), [x1, z1] = toWorld(W, H);
    graticule(x0, z0, x1, z1);
    drawTiles(x0, z0, x1, z1);
    if ($('grid').checked && view.s >= 2) chunkGrid(x0, z0, x1, z1);
    reticle();
    rulers(x0, z0, x1, z1);
    placePins();
    scaleBar();
    // The readout follows the cursor and keeps its last spot when the cursor leaves the map (so Copy still has it); before any cursor it shows the view centre.
    if (mouse.in) cursorW = toWorld(mouse.x, mouse.y);
    if (cursorW) readout(cursorW[0], cursorW[1], true); else readout(view.x, view.z, false);
    paintMini();
  }

  // Spawn, the player position saved in the world file (hidden while live players are shown), and live players.
  function placePins() {
    placePin($('pinSpawn'), meta.spawn, 'Spawn', dim.dim === 0);
    const liveOn = live && live.active && live.players.length > 0;
    placePin($('pinPlayer'), meta.player, 'Player', !liveOn && meta.player && meta.player.dim === dim.dim);
    placeLivePins();
  }

  // The 3D view draws on its own WebGL canvas under this one; this canvas keeps only the crosshair.
  function paint3d() {
    ctx.clearRect(0, 0, W, H);
    const again = m3.render({
      x: view.x, z: view.z, s: view.s, yaw: orbit.yaw, pitch: orbit.pitch, W, H, dpr,
      caves: layer === 'caves', grid: $('grid').checked, gridCol: hexRgb(pal.acc)
    });
    reticle();
    placePins();
    scaleBar();
    if (mouse.in) cursorW = screenToWorld(mouse.x, mouse.y) || cursorW;
    if (cursorW) readout(cursorW[0], cursorW[1], true); else readout(view.x, view.z, false);
    $('compass').style.setProperty('--yaw', orbit.yaw + 'rad');
    paintMini();
    if (again) draw(); // tiles still uploading, or the camera is still easing to the ground height
  }
  function hexRgb(c) {
    const m = /^#?([0-9a-f]{6})$/i.exec(c || '');
    if (!m) return [1, 0.6, 0.24];
    const n = parseInt(m[1], 16);
    return [(n >> 16) / 255, ((n >> 8) & 255) / 255, (n & 255) / 255];
  }

  /* ---- overview map ---- */

  function setupMini() {
    const pts = [];
    if (dim.bounds) pts.push([dim.bounds.minX, dim.bounds.minZ], [dim.bounds.maxX, dim.bounds.maxZ]);
    if (dim.dim === 0 && meta.spawn) pts.push([meta.spawn.x, meta.spawn.z]);
    if (meta.player && meta.player.dim === dim.dim) pts.push([meta.player.x, meta.player.z]);
    if (!pts.length) { mm = null; mini.closest('.overview').hidden = true; return; }
    mini.closest('.overview').hidden = false;
    let minX = Math.min(...pts.map(p => p[0])), maxX = Math.max(...pts.map(p => p[0]));
    let minZ = Math.min(...pts.map(p => p[1])), maxZ = Math.max(...pts.map(p => p[1]));
    const w = Math.max(maxX - minX, 256), h = Math.max(maxZ - minZ, 256);
    const padX = w * 0.08, padZ = h * 0.08;
    box = { x: (minX + maxX) / 2, z: (minZ + maxZ) / 2, w: w + padX * 2, h: h + padZ * 2 };
    const maxW = window.innerWidth < 560 ? 130 : 200, maxH = window.innerWidth < 560 ? 100 : 150;
    const s = Math.min(maxW / box.w, maxH / box.h);
    const cw = Math.max(60, Math.round(box.w * s)), ch = Math.max(60, Math.round(box.h * s));
    const r = window.devicePixelRatio || 1;
    mini.width = Math.round(cw * r); mini.height = Math.round(ch * r);
    mini.style.width = cw + 'px'; mini.style.height = ch + 'px';
    mm = { s, w: cw, h: ch, r, k: Math.max(0, Math.min(dim.maxZoom, Math.floor(Math.log2(1 / s)))) };
  }

  const miniToWorld = (px, py) => [box.x + (px - mm.w / 2) / mm.s, box.z + (py - mm.h / 2) / mm.s];
  const miniToScreen = (x, z) => [mm.w / 2 + (x - box.x) * mm.s, mm.h / 2 + (z - box.z) * mm.s];

  function paintMini() {
    if (!mm) return;
    mctx.setTransform(mm.r, 0, 0, mm.r, 0, 0);
    mctx.fillStyle = pal.void; mctx.fillRect(0, 0, mm.w, mm.h);
    mctx.imageSmoothingEnabled = true;
    const span = TILE * 2 ** mm.k;
    if (dim.sets && dim.sets[mm.k]) {
      for (const id of dim.sets[mm.k]) {
        const [tx, tz] = id.split(',').map(Number), t = tile(dim.name, mm.k, tx, tz);
        if (!t.ok) continue;
        const [sx, sy] = miniToScreen(tx * span, tz * span), size = span * mm.s;
        mctx.drawImage(t.img, sx, sy, size + .5, size + .5);
      }
    }
    // viewport frame (in 3D: the patch of ground the camera sees)
    const foot = mode === '3d' && m3.footprint();
    if (foot) {
      mctx.beginPath();
      foot.forEach(([x, z], i) => { const [px, py] = miniToScreen(x, z); if (i) mctx.lineTo(px, py); else mctx.moveTo(px, py); });
      mctx.closePath();
      mctx.fillStyle = pal.acc; mctx.globalAlpha = 0.12; mctx.fill();
      mctx.globalAlpha = 1; mctx.strokeStyle = pal.acc; mctx.lineWidth = 1; mctx.stroke();
    } else {
      const [ax, ay] = miniToScreen(...toWorld(RULER, RULER)), [bx, by] = miniToScreen(...toWorld(W, H));
      mctx.fillStyle = pal.acc; mctx.globalAlpha = 0.12; mctx.fillRect(ax, ay, bx - ax, by - ay);
      mctx.globalAlpha = 1; mctx.strokeStyle = pal.acc; mctx.lineWidth = 1;
      mctx.strokeRect(Math.round(ax) + .5, Math.round(ay) + .5, Math.max(3, bx - ax), Math.max(3, by - ay));
    }
    const dot = (p, c) => { const [x, y] = miniToScreen(p.x, p.z); mctx.fillStyle = c; mctx.fillRect(Math.round(x) - 2, Math.round(y) - 2, 4, 4); };
    if (dim.dim === 0 && meta.spawn) dot(meta.spawn, '#ffc247');
    const here = liveHere();
    if (here.length) for (const p of here) dot(p, '#b8f55a');
    else if (meta.player && meta.player.dim === dim.dim) dot(meta.player, '#e6d9ff');
  }

  /* ---- movement ---- */

  function zoomAt(f, px, py) {
    fly = null;
    if (mode === '3d') {
      // Move towards (or away from) the ground under the cursor by the same factor as the zoom.
      const s = clampScale(view.s * f), k = 1 - view.s / s;
      const p = px === W / 2 && py === H / 2 ? null : m3.pick(px, py);
      if (p) { view.x += (p.x - view.x) * k; view.z += (p.z - view.z) * k; }
      view.s = s;
      draw();
      return;
    }
    const [wx, wz] = toWorld(px, py);
    view.s = clampScale(view.s * f);
    view.x = wx - (px - W / 2) / view.s; view.z = wz - (py - H / 2) / view.s;
    draw();
  }

  function flyTo(x, z, s, instant) {
    s = clampScale(s);
    if (instant || reduce.matches) { fly = null; view.x = x; view.z = z; view.s = s; draw(); return; }
    const d = Math.min(1000, 380 + Math.hypot(x - view.x, z - view.z) * view.s * 0.15 + Math.abs(Math.log2(s / view.s)) * 70);
    const f = fly = { x0: view.x, z0: view.z, s0: view.s, x1: x, z1: z, s1: s, t0: performance.now(), d };
    const step = now => {
      if (fly !== f) return;
      const t = Math.min(1, (now - f.t0) / f.d), e = 1 - Math.pow(1 - t, 3);
      view.x = f.x0 + (f.x1 - f.x0) * e; view.z = f.z0 + (f.z1 - f.z0) * e;
      view.s = Math.exp(Math.log(f.s0) + (Math.log(f.s1) - Math.log(f.s0)) * e);
      paint();
      if (t < 1) requestAnimationFrame(step); else fly = null;
    };
    requestAnimationFrame(step);
  }

  function fitTarget() {
    const b = dim.bounds;
    if (!b) return { x: 0, z: 0, s: 1 };
    return { x: (b.minX + b.maxX) / 2, z: (b.minZ + b.maxZ) / 2, s: Math.min(2, Math.min((W - RULER) / (b.maxX - b.minX), (H - RULER) / (b.maxZ - b.minZ)) * 0.8) };
  }
  const goTo = p => { if (p) flyTo(p.x, p.z, Math.max(view.s, 2)); };

  function pick(name, intro) {
    dim = Object.assign({ name }, meta.dims[name]);
    dim.sets = {};
    for (const k of Object.keys(dim.have || {})) dim.sets[k] = new Set(dim.have[k]);
    if (!dim.have) dim.sets = null;
    tiles.clear();
    if (m3) m3.reset();
    if (!dim.heights && mode === '3d') setMode('2d', true); // the 3D view is built from the height tiles
    $('mode3d').disabled = !m3 || !dim.heights;
    $('mode3d').title = !m3 ? 'This computer cannot draw the 3D view (no WebGL2)' : !dim.heights ? 'Rebuild this map to get the 3D view' : '3D view (V)';
    if (!dim.caves) layer = 'surface';
    $('caveSw').hidden = !dim.caves;
    $('caves').checked = layer === 'caves';
    cursorW = null; // coordinates differ between dimensions, so a cursor spot from the other one no longer applies
    document.documentElement.dataset.dim = name in { overworld: 1, nether: 1, end: 1 } ? name : 'overworld';
    readPalette();
    for (const b of $('dim').children) b.setAttribute('aria-pressed', String(b.dataset.dim === name));
    const n = dim.stats && dim.stats.chunks, b = dim.bounds;
    $('sub').textContent = n ? fmt(n) + ' chunks mapped' + (b ? ' · ' + fmt(b.maxX - b.minX) + ' × ' + fmt(b.maxZ - b.minZ) + ' blocks' : '') : 'Mapped';
    if (meta.dims && Object.keys(meta.dims).length > 1) history.replaceState(null, '', '#' + name);
    renderInfo();
    setupMini();
    const t = fitTarget();
    if (intro) { view.x = t.x; view.z = t.z; view.s = t.s * 0.4; flyTo(t.x, t.z, t.s); }
    else flyTo(t.x, t.z, t.s, true);
  }

  /* ---- info panel ---- */

  const row = (k, v) => '<div><dt>' + esc(k) + '</dt><dd>' + esc(v) + '</dd></div>';
  const goRow = (k, v, what) => '<div><dt>' + esc(k) + '</dt><dd>' + esc(v) + ' <button type="button" class="mini" data-go="' + what + '">Go</button></dd></div>';

  function renderInfo() {
    if (!meta || !dim) return;
    const det = meta.details || {}, b = dim.bounds, st = dim.stats || {}, bd = meta.build;
    const world = [];
    if (det.gameMode !== undefined) world.push(row('Game mode', MODES[det.gameMode] || 'Mode ' + det.gameMode));
    if (det.difficulty !== undefined) world.push(row('Difficulty', DIFFS[det.difficulty] || 'Level ' + det.difficulty));
    if (det.cheats !== undefined) world.push(row('Cheats', det.cheats ? 'On' : 'Off'));
    if (det.version) world.push(row('Saved by game version', det.version));
    if (det.lastPlayed) world.push(row('Last played', new Date(det.lastPlayed).toLocaleString()));
    if (meta.generated) world.push(row('Map built', new Date(meta.generated).toLocaleString()));
    if (bd && bd.seconds !== undefined) world.push(row('Build time', bd.seconds + ' s'));
    const here = [];
    if (st.chunks) here.push(row('Chunks mapped', fmt(st.chunks)), row('Explored area', fmt(st.chunks * 256) + ' blocks²'));
    if (b) here.push(row('Extent', fmt(b.maxX - b.minX) + ' × ' + fmt(b.maxZ - b.minZ) + ' blocks'), row('West to east', 'X ' + fmt(b.minX) + ' to ' + fmt(b.maxX)), row('North to south', 'Z ' + fmt(b.minZ) + ' to ' + fmt(b.maxZ)));
    if (dim.tileCount) here.push(row('Map tiles', fmt(dim.tileCount) + ' (' + (dim.maxZoom + 1) + ' zoom levels)'));
    if (st.blank) here.push(row('Nothing to draw', fmt(st.blank) + ' chunks'));
    if (st.partial) here.push(row('Partly drawn', fmt(st.partial) + ' chunks'));
    if (bd && bd.maxY !== null && bd.maxY !== undefined && dim.dim !== 1) here.push(row('Height limit', 'Y ' + bd.maxY));
    const marks = [];
    if (meta.spawn) marks.push(goRow('Spawn', fmt(meta.spawn.x) + ', ' + fmt(meta.spawn.z) + (meta.spawn.y !== undefined ? ' (Y ' + fmt(meta.spawn.y) + ')' : ''), 'spawn'));
    if (meta.player) marks.push(goRow('Player', fmt(meta.player.x) + ', ' + fmt(meta.player.z) + ' (Y ' + fmt(meta.player.y) + ') · ' + (DIM_NAME[meta.player.dim] || 'dimension ' + meta.player.dim), 'player'));
    const sec = (title, rows) => rows.length ? '<h2>' + esc(title) + '</h2><dl>' + rows.join('') + '</dl>' : '';
    $('info').innerHTML =
      '<div class="ihead"><h2>World details</h2><button type="button" class="mini" id="infoClose" aria-label="Close world details">Close</button></div>' +
      sec('World', world) +
      (!meta.details ? '<p class="hint">Rebuild this map in the control panel to add game mode, difficulty and game version.</p>' : '') +
      sec(label(dim.name), here) + sec('Markers', marks);
  }

  function toggleInfo(open) {
    const el = $('info');
    const want = open === undefined ? el.hidden : open;
    el.hidden = !want;
    $('infoBtn').setAttribute('aria-expanded', String(want));
    $('infoBtn').classList.toggle('on', want);
    if (want) placeInfo();
  }

  // Sits under the top bar, above the bottom row, and scrolls if the screen is short.
  function placeInfo() {
    const t = document.querySelector('.top').getBoundingClientRect(), b = document.querySelector('.bottom').getBoundingClientRect();
    const c = document.querySelector('.cluster').getBoundingClientRect(); // the panel is on the right, so only the zoom buttons and overview map are in its way
    const floor = window.innerWidth <= 560 ? b.top : c.top;
    const el = $('info');
    el.style.top = Math.round(t.bottom + 10) + 'px';
    el.style.maxHeight = Math.max(140, Math.round(floor - t.bottom - 24)) + 'px';
  }

  $('info').addEventListener('click', e => {
    if (e.target.closest('#infoClose')) { toggleInfo(false); cv.focus({ preventScroll: true }); return; }
    const g = e.target.closest('[data-go]');
    if (!g) return;
    if (g.dataset.go === 'spawn') toSpawn(); else toPlayer();
  });
  $('infoBtn').addEventListener('click', () => toggleInfo());

  // Copy what the readout shows: plain "X Z", or a teleport command that keeps the player's own height.
  async function copy(text, btn, idle) {
    try { await navigator.clipboard.writeText(text); }
    catch { const t = document.createElement('textarea'); t.value = text; document.body.appendChild(t); t.select(); try { document.execCommand('copy'); } catch {} t.remove(); }
    btn.textContent = 'Copied';
    setTimeout(() => { btn.textContent = idle; }, 1400);
  }
  $('copyXZ').addEventListener('click', e => copy(last.x + ' ' + last.z, e.currentTarget, 'Copy X Z'));
  $('copyTp').addEventListener('click', e => copy('/tp ' + last.x + ' ' + (last.y === null || last.y === undefined ? '~' : last.y) + ' ' + last.z, e.currentTarget, 'Copy /tp'));

  /* ---- right-click menu: copy the coordinates under the mouse ---- */

  const ctxMenu = document.createElement('div');
  ctxMenu.className = 'ctxmenu'; ctxMenu.hidden = true; ctxMenu.setAttribute('role', 'menu');
  document.body.appendChild(ctxMenu);
  const hideMenu = () => { ctxMenu.hidden = true; };
  function showMenu(e) {
    if (!dim) return;
    const w = screenToWorld(e.clientX, e.clientY);
    if (!w) return; // 3D: pointing at the sky
    const x = Math.floor(w[0]), z = Math.floor(w[1]), y = heightAt(x, z);
    const hasY = typeof y === 'number';
    const items = [
      ['Copy X Z', x + ' ' + z],
      hasY && ['Copy X Y Z', x + ' ' + y + ' ' + z],
      ['Copy /tp', '/tp ' + x + ' ' + (hasY ? y : '~') + ' ' + z]
    ].filter(Boolean);
    ctxMenu.innerHTML = '<div class="ctxhead">' + x + (hasY ? ', ' + y : '') + ', ' + z + '</div>';
    for (const [name, text] of items) {
      const b = document.createElement('button');
      b.type = 'button'; b.setAttribute('role', 'menuitem'); b.textContent = name;
      b.addEventListener('click', async () => { await copy(text, b, name); setTimeout(hideMenu, 450); });
      ctxMenu.appendChild(b);
    }
    ctxMenu.hidden = false;
    const r = ctxMenu.getBoundingClientRect();
    ctxMenu.style.left = Math.max(4, Math.min(e.clientX, innerWidth - r.width - 4)) + 'px';
    ctxMenu.style.top = Math.max(4, Math.min(e.clientY, innerHeight - r.height - 4)) + 'px';
    ctxMenu.querySelector('button').focus({ preventScroll: true });
  }
  cv.addEventListener('contextmenu', e => { e.preventDefault(); if (!(mode === '3d' && rightMoved)) showMenu(e); rightMoved = false; });
  document.addEventListener('pointerdown', e => { if (!ctxMenu.contains(e.target)) hideMenu(); });
  document.addEventListener('keydown', e => { if (e.key === 'Escape') hideMenu(); });
  window.addEventListener('blur', hideMenu);
  cv.addEventListener('wheel', hideMenu, { passive: true });

  /* ---- input ---- */

  // 3D: move the point looked at along the ground, relative to the way the camera faces.
  function pan3d(right, forward) {
    view.x += Math.cos(orbit.yaw) * right - Math.sin(orbit.yaw) * forward;
    view.z += -Math.sin(orbit.yaw) * right - Math.cos(orbit.yaw) * forward;
  }
  const turn = (dyaw, dpitch) => { tween = null; orbit.yaw = (orbit.yaw + dyaw) % (Math.PI * 2); orbit.pitch = Math.max(0.08, Math.min(TOP_DOWN, orbit.pitch + dpitch)); };

  // drag: left moves the map; in 3D right, middle, Shift or Ctrl + left turn and tilt the camera
  let drag = null, rightMoved = false;
  cv.addEventListener('pointerdown', e => {
    const rot = mode === '3d' && (e.button === 2 || e.button === 1 || (e.button === 0 && (e.shiftKey || e.ctrlKey)));
    if (e.button !== 0 && !rot) return;
    if (e.button === 1) e.preventDefault();
    fly = null; cv.setPointerCapture(e.pointerId);
    drag = { x: e.clientX, y: e.clientY, x0: e.clientX, y0: e.clientY, rot };
    if (e.button === 2) rightMoved = false;
  });
  cv.addEventListener('pointerup', () => { drag = null; });
  cv.addEventListener('pointerleave', () => { mouse.in = false; draw(); });
  cv.addEventListener('pointermove', e => {
    mouse.x = e.clientX; mouse.y = e.clientY; mouse.in = true;
    if (drag) {
      const dx = e.clientX - drag.x, dy = e.clientY - drag.y;
      if (Math.hypot(e.clientX - drag.x0, e.clientY - drag.y0) > 4) rightMoved = true;
      if (mode !== '3d') { view.x -= dx / view.s; view.z -= dy / view.s; }
      else if (drag.rot) turn(-dx * 0.006, dy * 0.005);
      else pan3d(-dx / view.s, dy / (view.s * Math.max(0.35, Math.sin(orbit.pitch))));
      drag.x = e.clientX; drag.y = e.clientY;
    }
    draw();
  });
  cv.addEventListener('wheel', e => { e.preventDefault(); zoomAt(e.deltaY < 0 ? 1.25 : 0.8, e.clientX, e.clientY); }, { passive: false });
  cv.addEventListener('dblclick', e => zoomAt(2, e.clientX, e.clientY));
  cv.addEventListener('keydown', e => {
    if (e.ctrlKey || e.metaKey || e.altKey) return;
    fly = null;
    const step = 80 / view.s;
    const k = e.key.length === 1 ? e.key.toLowerCase() : e.key;
    const map = {
      i: () => toggleInfo(), c: () => { if (!dim.caves) return; $('caves').checked = !$('caves').checked; setLayer(); }, g: () => { $('grid').checked = !$('grid').checked; }, ArrowLeft: () => (view.x -= step), ArrowRight: () => (view.x += step), ArrowUp: () => (view.z -= step), ArrowDown: () => (view.z += step), '+': () => zoomAt(1.25, W / 2, H / 2), '=': () => zoomAt(1.25, W / 2, H / 2), '-': () => zoomAt(0.8, W / 2, H / 2),
      v: () => { if (!$('mode3d').disabled) setMode(mode === '3d' ? '2d' : '3d'); } };
    if (mode === '3d') Object.assign(map, {
      ArrowLeft: () => pan3d(-step, 0), ArrowRight: () => pan3d(step, 0), ArrowUp: () => pan3d(0, step), ArrowDown: () => pan3d(0, -step),
      w: () => pan3d(0, step), s: () => pan3d(0, -step), a: () => pan3d(-step, 0), d: () => pan3d(step, 0),
      q: () => turn(0.15, 0), e: () => turn(-0.15, 0), r: () => turn(0, -0.08), f: () => turn(0, 0.08), PageUp: () => turn(0, -0.08), PageDown: () => turn(0, 0.08)
    });
    if (map[k]) { e.preventDefault(); map[k](); draw(); }
  });

  // overview: click or drag to move the map
  let mdrag = false;
  const miniMove = e => { if (!mm) return; const r = mini.getBoundingClientRect(), [x, z] = miniToWorld(e.clientX - r.left, e.clientY - r.top); fly = null; view.x = x; view.z = z; draw(); };
  mini.addEventListener('pointerdown', e => { mini.setPointerCapture(e.pointerId); mdrag = true; miniMove(e); });
  mini.addEventListener('pointermove', e => { if (mdrag) miniMove(e); });
  mini.addEventListener('pointerup', () => { mdrag = false; });

  /* ---- 2D / 3D ---- */

  // Animates yaw and pitch (the 2D map is the 3D view seen straight down, so switching is a tilt).
  function tilt(yaw, pitch, done) {
    if (reduce.matches) { orbit.yaw = yaw; orbit.pitch = pitch; draw(); if (done) done(); return; }
    let y0 = orbit.yaw % (Math.PI * 2);
    if (y0 - yaw > Math.PI) y0 -= Math.PI * 2; else if (yaw - y0 > Math.PI) y0 += Math.PI * 2; // the short way round
    const p0 = orbit.pitch, t0 = performance.now(), d = 650, me = tween = {};
    const step = now => {
      if (tween !== me) return;
      const t = Math.min(1, (now - t0) / d), e = t < 0.5 ? 4 * t * t * t : 1 - Math.pow(-2 * t + 2, 3) / 2;
      orbit.yaw = y0 + (yaw - y0) * e; orbit.pitch = p0 + (pitch - p0) * e;
      paint();
      if (t < 1) requestAnimationFrame(step); else { tween = null; if (done) done(); }
    };
    requestAnimationFrame(step);
  }

  function setMode(want, instant) {
    if (want === '3d' && (!m3 || (dim && !dim.heights))) return;
    if (want === mode && !instant) return;
    try { localStorage.setItem('bedrock-map-view', want); } catch {}
    const show3d = on => {
      mode = on ? '3d' : '2d';
      document.body.classList.toggle('is3d', on);
      $('map3d').hidden = !on;
      $('mode3d').setAttribute('aria-pressed', String(on));
      $('compass').hidden = !on;
      $('hints').textContent = on
        ? 'Drag to move. Right-drag (or Shift-drag) turns and tilts. Scroll to zoom. WASD/arrows move, Q/E turn, R/F tilt. V: flat map.'
        : 'Drag to move. Scroll or +/− to zoom. Arrows move. Double-click zooms in. I: info. G: chunk grid. V: 3D view.';
      view.s = clampScale(view.s);
      cursorW = null;
    };
    if (want === '3d') {
      show3d(true);
      if (instant) { orbit.pitch = PITCH; draw(); }
      else { orbit.pitch = TOP_DOWN; tilt(orbit.yaw, PITCH); }
    } else if (instant || mode !== '3d') { show3d(false); orbit.yaw = 0; draw(); }
    else tilt(0, TOP_DOWN, () => { show3d(false); draw(); });
  }
  $('mode3d').addEventListener('click', () => setMode(mode === '3d' ? '2d' : '3d'));
  $('compass').addEventListener('click', () => tilt(0, PITCH));

  $('zin').addEventListener('click', () => zoomAt(1.5, W / 2, H / 2));
  $('zout').addEventListener('click', () => zoomAt(1 / 1.5, W / 2, H / 2));
  $('fit').addEventListener('click', () => { const t = fitTarget(); flyTo(t.x, t.z, t.s); });
  const setLayer = () => { layer = $('caves').checked && dim && dim.caves ? 'caves' : 'surface'; draw(); };
  $('caves').addEventListener('change', setLayer);
  $('grid').addEventListener('change', draw);
  $('reticle').addEventListener('change', draw);
  if (window.Leaves && window.Leaves.available) {
    $('leafToggle').checked = window.Leaves.on;
    $('leafToggle').addEventListener('change', e => window.Leaves.set(e.target.checked));
  } else $('leafSw').hidden = true;
  const toSpawn = () => {
    if (dim.dim !== 0) { const d = Object.keys(meta.dims).find(n => meta.dims[n].dim === 0); if (d) pick(d); }
    goTo(meta.spawn);
  };
  $('spawn').addEventListener('click', toSpawn);
  const toPlayer = () => {
    if (!meta.player) { say('No player position was saved in this world.'); return; }
    const d = Object.keys(meta.dims).find(n => meta.dims[n].dim === meta.player.dim);
    if (d && d !== dim.name) pick(d);
    goTo(meta.player);
  };
  $('player').addEventListener('click', toPlayer);
  $('pinSpawn').querySelector('.hit').addEventListener('click', toSpawn);
  $('pinPlayer').querySelector('.hit').addEventListener('click', () => goTo(meta.player));
  $('go').addEventListener('submit', e => { e.preventDefault(); const x = parseFloat($('gx').value), z = parseFloat($('gz').value); if (Number.isFinite(x) && Number.isFinite(z)) goTo({ x, z }); });
  const hideHints = () => { const h = document.querySelector('.hints'); if (h) h.classList.add('gone'); };
  for (const ev of ['pointerdown', 'wheel', 'keydown']) cv.addEventListener(ev, hideHints, { once: true, passive: true });
  window.addEventListener('resize', () => { resize(); if (dim) { setupMini(); draw(); } });

  /* ---- live link: players and redrawn chunks from a companion plugin (live.json is served by the app) ---- */

  let live = null, liveMeta = null;
  const livePins = new Map();
  const liveHere = () => (live && live.active && dim ? live.players.filter(p => p.dim === dim.dim) : []);

  function placeLivePins() {
    const here = liveHere(), seen = new Set();
    for (const p of here) {
      let el = livePins.get(p.id);
      if (!el) {
        el = document.createElement('div');
        el.className = 'pin live';
        el.innerHTML = '<button type="button" class="hit"></button><span class="tag"></span>';
        el.querySelector('.hit').innerHTML = $('pinPlayer').querySelector('.hit').innerHTML;
        el.querySelector('.hit').addEventListener('click', () => { const q = liveHere().find(x => x.id === el.dataset.id); if (q) goTo(q); });
        el.dataset.id = p.id;
        $('pins').appendChild(el);
        livePins.set(p.id, el);
      }
      el.querySelector('.hit').setAttribute('aria-label', 'Go to ' + p.name);
      placePin(el, p, p.name, true);
      seen.add(p.id);
    }
    for (const [id, el] of livePins) if (!seen.has(id)) { el.remove(); livePins.delete(id); }
  }

  async function pollLive() {
    let next;
    try {
      const r = await fetch('live.json', { cache: 'no-store' });
      if (!r.ok) throw new Error();
      next = await r.json();
    } catch {
      if (!live) return; // a copy of the map opened without the app: no live link, stop asking
      next = { active: false, players: [], revs: {}, metaRev: liveMeta };
    }
    const first = !live;
    live = next;
    // Redrawn files: remember the revision; tile() and heightTile() fetch the new copy (now, or when next shown).
    for (const [id, rev] of Object.entries(live.revs || {})) {
      if (fileRev[id] === rev) continue;
      fileRev[id] = rev;
      if (first) continue; // this page loaded after the redraw, so it already has the new copy
      const m = /^tiles\/([^/]+)\/(\d+)\/(-?\d+)_(-?\d+)$/.exec(id) || /^heights\/([^/]+)\/(-?\d+)_(-?\d+)$/.exec(id);
      if (!m || !dim || m[1] !== dim.name) continue;
      // Only tiles already loaded need fetching again; the rest get the new copy whenever they are first shown.
      if (id.startsWith('heights/')) { if (hmaps.has(m[1] + '/' + m[2] + '_' + m[3])) heightTile(+m[2], +m[3]); }
      else if (tiles.has(id)) tile(dim.name, +m[2], +m[3], +m[4]);
    }
    // New tiles: the lists of which tiles exist live in meta.json.
    if (first) liveMeta = live.metaRev;
    else if (live.metaRev !== liveMeta) {
      liveMeta = live.metaRev;
      try {
        const fresh = await (await fetch('meta.json', { cache: 'no-store' })).json();
        for (const [name, d] of Object.entries(fresh.dims || {})) if (meta.dims[name]) Object.assign(meta.dims[name], d);
        if (dim && meta.dims[dim.name]) {
          Object.assign(dim, meta.dims[dim.name]);
          dim.sets = {};
          for (const k of Object.keys(dim.have || {})) dim.sets[k] = new Set(dim.have[k]);
          setupMini();
        }
      } catch {}
    }
    const n = live.players.length, badge = $('liveBadge');
    badge.hidden = !live.active;
    badge.classList.toggle('quiet', !!live.quiet);
    $('liveText').textContent = live.quiet ? 'Live link quiet' : 'Live · ' + n + (n === 1 ? ' player online' : ' players online');
    draw();
    setTimeout(pollLive, 1500);
  }

  m3 = window.Map3D ? window.Map3D.create($('map3d'), {
    dim: () => dim, heightAt, heightTile, draw,
    image: (tx, tz, group) => tile(dim.name, 0, tx, tz, group)
  }) : null;
  if (!m3) { $('mode3d').disabled = true; $('mode3d').title = 'This computer cannot draw the 3D view (no WebGL2)'; }

  fetch('meta.json').then(r => { if (!r.ok) throw new Error(); return r.json(); }).then(m => {
    meta = m;
    document.title = m.name + ' - world map';
    $('title').textContent = m.name;
    $('title').title = m.name;
    const dimBar = $('dim');
    for (const n of Object.keys(m.dims)) {
      const b = document.createElement('button');
      b.type = 'button'; b.dataset.dim = n; b.textContent = label(n);
      if (m.dims[n].stats) b.title = fmt(m.dims[n].stats.chunks) + ' chunks mapped'; b.setAttribute('aria-pressed', 'false');
      b.addEventListener('click', () => { if (!dim || dim.name !== n) pick(n); });
      dimBar.appendChild(b);
    }
    $('spawn').hidden = !m.spawn; $('player').hidden = !m.player;
    say('');
    resize();
    let saved = null;
    try { saved = localStorage.getItem('bedrock-map-view'); } catch {}
    if (saved === '3d') setMode('3d', true);
    const want = decodeURIComponent(location.hash.slice(1));
    pick(m.dims[want] ? want : Object.keys(m.dims)[0], true);
    document.body.classList.add('ready');
    cv.focus({ preventScroll: true });
    pollLive();
  }).catch(() => {
    readPalette(); resize();
    document.body.classList.add('ready');
    $('sub').textContent = 'No signal';
    say('Could not load meta.json. Open this page through the server (node bedrock-map.js --serve-only <folder>), not by double-clicking the file.');
  });
  readPalette();
})();
