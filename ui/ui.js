(() => {
  'use strict';
  const $ = id => document.getElementById(id);
  const list = $('worlds');
  const TILE = 512, SCOPE = 176;
  let busyJob = null;

  const esc = s => String(s).replace(/[&<>"']/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
  const size = b => (b >= 1073741824 ? (b / 1073741824).toFixed(1) + ' GB' : Math.max(1, Math.round(b / 1048576)) + ' MB');
  const num = n => Math.round(n).toLocaleString('en-US');
  const say = t => { $('status').textContent = t; };
  const fail = t => { const e = $('error'); e.hidden = !t; e.textContent = t || ''; };
  const builder = (text, cls) => { const d = $('tBuilder'); d.textContent = text; d.className = cls || ''; };

  const DIM_LABEL = { overworld: 'Overworld', nether: 'Nether', end: 'The End' };
  const MODES = { 0: 'Survival', 1: 'Creative', 2: 'Adventure', 5: 'Default', 6: 'Spectator' };
  const DIFFS = ['Peaceful', 'Easy', 'Normal', 'Hard'];
  const dimLabel = n => DIM_LABEL[n] || n;
  const dimById = id => ({ 0: 'Overworld', 1: 'Nether', 2: 'The End' }[id] || 'dimension ' + id);
  const xz = p => num(p.x) + ', ' + num(p.z);
  const span = b => (b ? num(b.maxX - b.minX) + ' × ' + num(b.maxZ - b.minZ) : '');
  const ago = iso => {
    const s = (Date.now() - new Date(iso).getTime()) / 1000;
    if (!(s >= 0)) return new Date(iso).toLocaleString();
    if (s < 90) return 'just now';
    if (s < 5400) return Math.round(s / 60) + ' min ago';
    if (s < 129600) return Math.round(s / 3600) + ' h ago';
    return Math.round(s / 86400) + ' days ago';
  };

  // The control panel talks to the Rust side through Tauri commands (there is no local web server any more).
  const invoke = window.__TAURI__.core.invoke;
  // Built maps are served by the program itself; Windows reaches custom schemes over http://<scheme>.localhost.
  const MAPS = /windows/i.test(navigator.userAgent) ? 'http://maps.localhost/' : 'maps://localhost/';
  async function api(path, body) {
    try {
      if (path === '/api/worlds') return await invoke('list_worlds');
      if (path === '/api/add') return await invoke('add_world', { path: body.path });
      if (path === '/api/build') return await invoke('start_build', { id: body.id, maxY: body.maxY === '' ? null : body.maxY, caves: body.caves !== false });
      if (path.startsWith('/api/jobs/')) return await invoke('job_status', { id: path.slice(10) });
    } catch (e) {
      throw new Error(typeof e === 'string' ? e : (e && e.message) || 'Request failed');
    }
    throw new Error('Unknown request ' + path);
  }

  /* ---- radar scope: a small live render of the built map ---- */

  const loadImg = src => new Promise(res => { const i = new Image(); i.onload = () => res(i); i.onerror = () => res(null); i.src = src; });

  function rings(c) {
    const m = SCOPE / 2;
    c.save();
    c.strokeStyle = 'rgba(255,140,42,.32)'; c.lineWidth = 1;
    c.beginPath();
    for (const r of [0.28, 0.56, 0.84]) { c.moveTo(m + r * m, m); c.arc(m, m, r * m, 0, Math.PI * 2); }
    c.moveTo(m, 0); c.lineTo(m, SCOPE); c.moveTo(0, m); c.lineTo(SCOPE, m);
    c.stroke();
    c.strokeStyle = 'rgba(184,245,90,.5)'; c.beginPath();
    for (let a = 0; a < 360; a += 15) {
      const r = a % 90 === 0 ? 10 : 5, t = a * Math.PI / 180;
      c.moveTo(m + Math.cos(t) * (m - 1), m + Math.sin(t) * (m - 1)); c.lineTo(m + Math.cos(t) * (m - 1 - r), m + Math.sin(t) * (m - 1 - r));
    }
    c.stroke();
    c.restore();
  }

  // Draws one dimension of the world onto its scope. The map's meta.json is fetched once per card.
  async function drawScope(li, w, dimName) {
    const cv = li.querySelector('canvas');
    const dpr = Math.min(window.devicePixelRatio || 1, 2);
    cv.width = cv.height = Math.round(SCOPE * dpr);
    const c = cv.getContext('2d');
    c.setTransform(dpr, 0, 0, dpr, 0, 0);
    const base = () => { const g = c.createRadialGradient(SCOPE / 2, SCOPE / 2, 4, SCOPE / 2, SCOPE / 2, SCOPE / 2); g.addColorStop(0, '#241030'); g.addColorStop(1, '#09030c'); c.fillStyle = g; c.fillRect(0, 0, SCOPE, SCOPE); };
    base(); rings(c);
    if (!w.map) return;
    const token = (li._scopeToken = (li._scopeToken || 0) + 1);
    try {
      const root = MAPS + encodeURIComponent(w.slug) + '/';
      if (!li._meta) {
        const r = await fetch(root + 'meta.json');
        if (!r.ok) throw new Error();
        li._meta = await r.json();
      }
      const meta = li._meta;
      const name = dimName && meta.dims[dimName] ? dimName : Object.keys(meta.dims)[0], d = meta.dims[name];
      if (!d || !d.bounds || !d.have) throw new Error();
      const b = d.bounds, cx = (b.minX + b.maxX) / 2, cz = (b.minZ + b.maxZ) / 2;
      const s = SCOPE * 0.92 / Math.hypot(b.maxX - b.minX, b.maxZ - b.minZ);
      const k = Math.max(0, Math.min(d.maxZoom, Math.floor(Math.log2(1 / s)))), sp = TILE * 2 ** k;
      const ids = d.have[k] || [];
      const imgs = await Promise.all(ids.map(id => loadImg(root + 'tiles/' + name + '/' + k + '/' + id.replace(',', '_') + '.png')));
      if (token !== li._scopeToken) return; // a newer dimension was picked while tiles were loading
      const at = (x, z) => [SCOPE / 2 + (x - cx) * s, SCOPE / 2 + (z - cz) * s];
      base();
      ids.forEach((id, i) => {
        if (!imgs[i]) return;
        const [tx, tz] = id.split(',').map(Number), [sx, sy] = at(tx * sp, tz * sp);
        c.globalAlpha = 0.92; c.drawImage(imgs[i], sx, sy, sp * s + 0.5, sp * s + 0.5);
      });
      c.globalAlpha = 1;
      rings(c);
      const blip = (p, color) => { const [x, y] = at(p.x, p.z); c.fillStyle = color; c.shadowColor = color; c.shadowBlur = 8; c.beginPath(); c.arc(x, y, 3, 0, 7); c.fill(); c.shadowBlur = 0; };
      if (d.dim === 0 && meta.spawn) blip(meta.spawn, '#ffc247');
      if (meta.player && meta.player.dim === d.dim) blip(meta.player, '#e6d9ff');
      li.querySelector('.none').hidden = true;
      const cap = li.querySelector('.scopecap');
      if (cap) cap.textContent = dimLabel(name) + (d.bounds ? ' · ' + span(d.bounds) + ' blocks' : '');
    } catch { li.querySelector('.none').hidden = false; }
  }

  /* ---- world cards ---- */

  const fact = (label, value, cls) => '<div><dt>' + esc(label) + '</dt><dd' + (cls ? ' class="' + cls + '"' : '') + '>' + esc(value) + '</dd></div>';

  function detailFacts(m) {
    const out = [], det = m.details || {};
    if (det.gameMode !== undefined) out.push(fact('Game mode', MODES[det.gameMode] || 'Mode ' + det.gameMode));
    if (det.difficulty !== undefined) out.push(fact('Difficulty', DIFFS[det.difficulty] || 'Level ' + det.difficulty));
    if (det.version) out.push(fact('Saved by game version', det.version));
    if (det.cheats !== undefined) out.push(fact('Cheats', det.cheats ? 'On' : 'Off'));
    return out;
  }

  function mapFacts(m) {
    const out = [], chunks = (m.dims || []).reduce((a, d) => a + d.chunks, 0);
    if (chunks) out.push(fact('Chunks mapped', num(chunks), 'built'));
    if (m.spawn) out.push(fact('Spawn', xz(m.spawn)));
    if (m.player) out.push(fact('Player', xz(m.player) + (m.player.y !== undefined ? ' (Y ' + num(m.player.y) + ')' : '') + ' · ' + dimById(m.player.dim)));
    return out.concat(detailFacts(m));
  }

  function dimChips(m) {
    if (!m.dims || !m.dims.length) return '';
    return '<div class="dimrow" role="group" aria-label="Dimensions in this map">' + m.dims.map((d, i) =>
      '<button type="button" class="dimchip" data-dim="' + esc(d.name) + '" aria-pressed="' + (i === 0) + '">' +
        '<b>' + esc(dimLabel(d.name)) + '</b>' +
        '<span>' + num(d.chunks) + ' chunks · ' + num(d.tiles) + (d.tiles === 1 ? ' tile' : ' tiles') + '</span>' +
        (d.bounds ? '<span>' + esc(span(d.bounds)) + ' blocks</span>' : '') +
      '</button>').join('') + '</div>';
  }

  function buildReport(m) {
    const b = m.build;
    if (!b) return '<p class="hint oldmap">Built before build reports existed. Rebuild to add game mode, difficulty, game version and a build report.</p>' ;
    const bits = [b.seconds + ' s'];
    if (b.maxY !== null && b.maxY !== undefined) bits.push('height limit ' + b.maxY);
    if (b.unknownBlocks) bits.push(b.unknownBlocks + ' block types guessed');
    if (b.caves) bits.push('cave layer');
    return '<details class="report"><summary>Build report <span>' + esc(bits.join(' · ')) + '</span></summary><ul class="notes">' +
      (b.notes || []).map(n => '<li>' + esc(n) + '</li>').join('') + '</ul></details>';
  }

  function card(w) {
    const li = document.createElement('li');
    li.className = 'world';
    li.dataset.id = w.id;
    li.dataset.state = w.map ? 'built' : 'none';
    const m = w.map || {};
    const when = w.mtime ? new Date(w.mtime).toLocaleString() : 'unknown';
    const built = w.map && w.map.generated ? new Date(w.map.generated).toLocaleString() + ' (' + ago(w.map.generated) + ')' : 'No map yet';
    const extra = w.map ? mapFacts(m) : [];
    li.innerHTML =
      '<div class="scopebox"><div class="scope"><canvas aria-hidden="true"></canvas><i class="sweep"></i><span class="none"' + (w.map ? ' hidden' : '') + '>' + (w.map ? 'Preview unavailable' : 'No map yet') + '</span></div>' +
        (w.map ? '<p class="scopecap" aria-hidden="true"></p><ul class="legend" aria-label="Scope key"><li class="sp">Spawn</li><li class="pl">Player</li></ul>' : '') + '</div>' +
      '<div class="info">' +
        '<div class="head"><h3>' + esc(w.name) + '</h3>' + (w.kind === 'export' ? '<span class="tag">exported file</span>' : '') + '</div>' +
        '<dl class="facts">' +
          fact('Last played', when) + fact('Size', size(w.bytes)) + fact('Map built', built, 'built') +
        '</dl>' +
        (extra.length ? '<dl class="facts more">' + extra.join('') + '</dl>' : '') +
        (w.map ? dimChips(m) : '') +
        '<p class="path">' + esc(w.dir) + '</p>' +
        '<div class="actions">' +
          '<button type="button" class="primary build">' + (w.map ? 'Rebuild map' : 'Build map') + '</button>' +
          (w.map ? '<a class="btn open" href="#">Open map</a>' : '') +
          '<label class="opt chk" title="Also draw tunnels and caverns found below the surface (overworld). Makes the build a bit slower."><input type="checkbox" class="caves" checked> Cave layer</label>' +
          '<label class="opt">Ignore blocks above Y <input type="text" inputmode="numeric" class="maxy" placeholder="319" aria-label="Height limit for ' + esc(w.name) + '"></label>' +
        '</div>' +
        '<div class="prog" hidden>' +
          '<div class="readline"><small></small><span class="pct"></span></div>' +
          '<div class="meter wait" role="progressbar" aria-label="Build progress" aria-valuemin="0" aria-valuemax="100"><i></i></div>' +
          '<ul class="notes"></ul>' +
        '</div>' +
        (w.map ? buildReport(m) : '') +
      '</div>';
    li.querySelector('.build').addEventListener('click', () => build(w, li));
    const openBtn = li.querySelector('.open');
    if (openBtn) openBtn.addEventListener('click', ev => { ev.preventDefault(); invoke('open_map', { slug: w.slug }).catch(e => fail(String(e))); });
    li.querySelectorAll('.dimchip').forEach(b => b.addEventListener('click', () => {
      li.querySelectorAll('.dimchip').forEach(x => x.setAttribute('aria-pressed', String(x === b)));
      drawScope(li, w, b.dataset.dim);
    }));
    drawScope(li, w, m.dims && m.dims[0] ? m.dims[0].name : null);
    return li;
  }

  function totals(worlds) {
    const built = worlds.filter(w => w.map);
    $('tMaps').textContent = String(built.length);
    $('tChunks').textContent = num(built.reduce((a, w) => a + (w.map.dims || []).reduce((s, d) => s + d.chunks, 0), 0));
    const newest = built.map(w => w.map.generated).filter(Boolean).sort().pop();
    $('tLast').textContent = newest ? ago(newest) : 'None yet';
  }

  async function load() {
    fail('');
    say('Looking for worlds');
    $('tCount').textContent = 'Scanning';
    const { worlds, searched, busy } = await api('/api/worlds');
    list.innerHTML = '';
    for (const w of worlds) list.appendChild(card(w));
    const e = $('empty');
    e.hidden = worlds.length > 0;
    if (!worlds.length) e.innerHTML = 'No worlds were found automatically. Add one by hand, or export a world from Minecraft (World settings, Export World) and add the <code>.mcworld</code> file.<br><small class="hint">Looked in: ' + (searched.length ? searched.map(esc).join('; ') : 'the usual Minecraft folders (this does not look like a Windows PC)') + '</small>';
    $('searched').innerHTML = searched.length ? searched.map(p => '<li>' + esc(p) + '</li>').join('') : '<li class="none">No Minecraft folders on this PC (not Windows?)</li>';
    $('busyNote').hidden = false;
    $('tCount').textContent = String(worlds.length);
    totals(worlds);
    say(worlds.length + (worlds.length === 1 ? ' world found' : ' worlds found'));
    if (busy && !busyJob) { say('A map is being built in another window.'); builder('Busy in another window', 'busy'); }
    else if (!busyJob) builder('Idle');
  }

  function meterSet(meter, pct) {
    meter.classList.remove('wait');
    meter.style.setProperty('--p', pct + '%');
    meter.setAttribute('aria-valuenow', Math.round(pct));
  }

  async function build(w, li) {
    fail('');
    const btns = document.querySelectorAll('.build');
    btns.forEach(b => (b.disabled = true));
    const prog = li.querySelector('.prog'), meter = prog.querySelector('.meter'), txt = prog.querySelector('small'), pct = prog.querySelector('.pct'), notes = prog.querySelector('.notes');
    prog.hidden = false; meter.className = 'meter wait'; meter.removeAttribute('aria-valuenow'); txt.textContent = 'Starting...'; pct.textContent = ''; notes.innerHTML = '';
    li.classList.add('busy');
    builder('Building', 'busy');
    try {
      const maxY = li.querySelector('.maxy').value.trim();
      const job = await api('/api/build', { id: w.id, maxY, caves: li.querySelector('.caves').checked });
      busyJob = job.id;
      const started = Date.now();
      for (;;) {
        await new Promise(r => setTimeout(r, 500));
        const j = await api('/api/jobs/' + job.id);
        if (j.total) { const share = j.dimCount > 1 ? ((j.dimIndex - 1) + j.n / j.total) / j.dimCount : j.n / j.total; meterSet(meter, share * 100); pct.textContent = Math.round(share * 100) + '%'; }
        else { meter.className = 'meter wait'; pct.textContent = ''; }
        const secs = Math.round((Date.now() - started) / 1000);
        txt.textContent = j.status === 'running' ? j.stage + (j.dimCount > 1 && j.total ? ' (' + j.dimIndex + ' of ' + j.dimCount + '): ' + j.n + ' / ' + j.total + ' tiles' : j.total ? ': ' + j.n + ' / ' + j.total + ' tiles' : '') + ' · ' + secs + ' s' : '';
        notes.innerHTML = j.notes.map(n => '<li>' + esc(n) + '</li>').join('');
        if (j.status === 'done') {
          say('Map for ' + w.name + ' is ready');
          // Re-read the list so the card shows the new map's real details (chunks, extents, build report).
          let fresh;
          try {
            const { worlds } = await api('/api/worlds');
            const again = worlds.find(x => x.id === w.id);
            fresh = card(again || Object.assign({}, w, { map: { generated: new Date().toISOString() } }));
            totals(worlds);
          } catch { fresh = card(Object.assign({}, w, { map: { generated: new Date().toISOString() } })); }
          fresh.style.animation = 'none';
          li.replaceWith(fresh);
          const fp = fresh.querySelector('.prog'), fm = fp.querySelector('.meter');
          fp.hidden = false;
          fp.querySelector('small').innerHTML = '<span class="ok">Done.</span> Use "Open map" to view it.';
          fp.querySelector('.pct').textContent = '100%';
          fm.classList.add('done'); meterSet(fm, 100);
          fp.querySelector('.notes').innerHTML = j.notes.map(n => '<li>' + esc(n) + '</li>').join('');
          break;
        }
        if (j.status === 'error') { prog.hidden = true; li.classList.remove('busy'); fail(j.error); say('Build failed'); break; }
      }
    } catch (e) { prog.hidden = true; li.classList.remove('busy'); fail(e.message); }
    busyJob = null;
    builder('Idle');
    document.querySelectorAll('.build').forEach(b => (b.disabled = false));
  }

  const lb = $('leavesBtn');
  if (window.Leaves && window.Leaves.available) {
    const sync = () => { lb.setAttribute('aria-pressed', String(window.Leaves.on)); lb.textContent = 'Falling leaves: ' + (window.Leaves.on ? 'on' : 'off'); };
    lb.addEventListener('click', () => { window.Leaves.set(!window.Leaves.on); sync(); });
    sync();
  } else lb.hidden = true;

  $('refresh').addEventListener('click', () => load().catch(e => fail(e.message)));
  $('addForm').addEventListener('submit', async e => {
    e.preventDefault();
    fail('');
    try { await api('/api/add', { path: $('path').value }); $('path').value = ''; await load(); } catch (err) { fail(err.message); }
  });
  load().catch(e => fail(e.message));
})();
