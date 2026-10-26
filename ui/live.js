/* Live link tab: status of the server that companion plugins push to, connected plugins, players and activity. */
(() => {
  'use strict';
  const $ = id => document.getElementById(id);
  const invoke = window.__TAURI__.core.invoke;
  const esc = s => String(s).replace(/[&<>"']/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
  const num = n => Math.round(n).toLocaleString('en-US');
  const DIMS = { 0: 'Overworld', 1: 'Nether', 2: 'The End' };
  const since = ms => {
    const s = Math.max(0, (Date.now() - ms) / 1000);
    if (s < 5) return 'just now';
    if (s < 90) return Math.round(s) + ' s ago';
    if (s < 5400) return Math.round(s / 60) + ' min ago';
    return Math.round(s / 3600) + ' h ago';
  };
  const clock = ms => new Date(ms).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit', second: '2-digit' });

  let last = null, showKey = false, formDirty = false, timer = null, lastAuto = null;
  const open = () => !$('livePage').hidden;
  const fail = t => { const e = $('liveError'); e.hidden = !t; e.textContent = t || ''; };

  async function call(cmd, args) {
    try { return await invoke(cmd, args); }
    catch (e) { throw new Error(typeof e === 'string' ? e : (e && e.message) || 'Request failed'); }
  }

  function show(on) {
    $('livePage').hidden = !on;
    $('worldsPage').hidden = on;
    $('tabLive').setAttribute('aria-pressed', String(on));
    if (on) { refresh(); window.scrollTo(0, 0); }
    schedule();
  }

  // Poll quickly while the tab is open, slowly otherwise (the rail tab still shows the state).
  function schedule() {
    clearTimeout(timer);
    timer = setTimeout(() => refresh().finally(schedule), open() ? 1000 : 4000);
  }

  async function refresh() {
    try { render(await call('live_status')); }
    catch (e) { $('liveTabState').textContent = 'Unavailable'; }
  }

  function render(s) {
    last = s;
    const c = s.config, players = s.players.length, plugins = s.sessions.length;
    // rail tab
    const dot = $('liveDot');
    dot.className = 'dot' + (s.status === 'error' ? ' bad' : plugins ? ' live' : s.status === 'listening' ? ' on' : '');
    const building = s.auto && s.auto.status === 'running';
    $('liveTabState').textContent = building ? 'Building the map of the open world' : s.status === 'error' ? 'Problem: see the tab' : s.status === 'off' ? 'Off'
      : plugins ? plugins + (plugins === 1 ? ' plugin' : ' plugins') + ' · ' + players + (players === 1 ? ' player' : ' players') : 'Waiting for a world or plugin';
    // A map built automatically: refresh the Worlds page so its card shows it.
    const autoState = s.auto && s.auto.status;
    if (autoState === 'done' && lastAuto && lastAuto !== 'done') { const r = document.getElementById('refresh'); if (r) r.click(); }
    lastAuto = autoState;
    if (!open()) return;

    const st = $('liveState');
    st.textContent = s.status === 'listening' ? 'Listening on ' + s.detail : s.status === 'off' ? 'Off' : 'Error';
    st.className = 'state' + (s.status === 'listening' ? ' on' : s.status === 'error' ? ' bad' : '');
    if (s.status === 'error') fail(s.detail + '. Pick another port, or close the program that uses it. (The in-game pack does not need the server.)');
    else if ($('liveError').textContent.includes('Pick another port')) fail('');
    if (!formDirty && document.activeElement !== $('livePort')) {
      $('liveOn').checked = c.enabled; $('livePort').value = c.port; $('liveLan').checked = c.allowLan;
    }
    $('liveLanHint').hidden = !$('liveLan').checked;
    $('liveUrl').textContent = s.urls[0];
    $('liveLanRow').hidden = s.urls.length < 2;
    $('liveLanUrl').textContent = s.urls[1] || '';
    $('liveKey').textContent = showKey ? c.key : '•'.repeat(12);
    $('liveKey').classList.toggle('secret', !showKey);
    $('liveTotals').textContent = num(s.totals.requests) + ' requests · ' + num(s.totals.chunksDrawn) + ' chunks drawn' +
      (s.queued ? ' · ' + num(s.queued) + ' waiting' : '') + (s.totals.rejected ? ' · ' + num(s.totals.rejected) + ' refused (wrong key)' : '');
    $('liveProto').textContent = 'v' + s.protocol;

    // in-game pack
    const g = s.game || {}, packSess = s.sessions.filter(x => x.id.startsWith('g-'));
    const ps = $('packState');
    if (packSess.length) {
      const live = packSess.some(x => x.state === 'live');
      ps.textContent = live ? 'Connected' : 'Quiet';
      ps.className = 'state' + (live ? ' on' : '');
    } else { ps.textContent = g.lines ? 'Not playing now' : 'Not seen yet'; ps.className = 'state'; }
    const lc = $('logCheck');
    lc.textContent = g.contentLog === true ? 'On' : g.contentLog === false ? 'Off' : 'Could not check';
    lc.className = 'check' + (g.contentLog === true ? ' ok' : g.contentLog === false ? ' bad' : '');
    $('logEnable').hidden = g.contentLog !== false;
    $('logEnable').disabled = !!g.minecraftRunning;
    $('logEnable').title = g.minecraftRunning ? 'Close Minecraft first (it saves its settings when it closes)' : 'Changes this setting in Minecraft’s settings file';
    const bits = [];
    if (g.lines) bits.push('Read ' + num(g.lines) + ' messages from the game, the last ' + since(g.lastLine) + '.');
    else if (g.contentLog === true) bits.push('Waiting for a world with the pack. ' + (g.watching ? 'Reading ' + g.watching + '.' : 'No content log yet: it appears once Minecraft is running.'));
    if (g.badLines) bits.push(num(g.badLines) + ' lines could not be read.');
    $('packDetail').textContent = bits.join(' ');
    if (document.activeElement !== $('autoOpen')) $('autoOpen').checked = c.autoOpen !== false;

    // automatic map build for the world that was opened
    const a = s.auto, box = $('autoBuild'), meter = $('autoMeter');
    box.hidden = !a;
    if (a) {
      const share = a.total ? (a.dimCount > 1 ? ((a.dimIndex - 1) + a.n / a.total) / a.dimCount : a.n / a.total) : 0;
      $('autoText').textContent = a.status === 'done' ? 'The map of "' + a.world + '" is built and open.'
        : a.status === 'error' ? 'Could not build the map of "' + a.world + '": ' + (a.error || 'unknown error')
        : a.status === 'waiting' ? a.stage + ' (map of "' + a.world + '").'
        : 'Building the map of "' + a.world + '": ' + (a.stage || 'starting') + (a.total ? ' · ' + a.n + ' / ' + a.total + ' tiles' : '');
      meter.className = 'meter' + (a.status === 'done' ? ' done' : a.status === 'running' && !a.total ? ' wait' : '');
      meter.style.setProperty('--p', (a.status === 'done' ? 100 : share * 100) + '%');
      meter.hidden = a.status === 'error' || a.status === 'waiting';
    }

    // plugins (keep a select that is open or focused, so polling does not close it)
    const list = $('liveSessions');
    $('livePluginCount').textContent = plugins;
    $('liveNoSessions').hidden = plugins > 0;
    if (!list.contains(document.activeElement)) {
      list.innerHTML = s.sessions.map(x => {
        const opts = '<option value="">No map (chunk updates are dropped)</option>' + s.maps.map(m =>
          '<option value="' + esc(m.slug) + '"' + (m.slug === x.link ? ' selected' : '') + '>' + esc(m.name || m.slug) + '</option>').join('');
        const w = x.worldState || {}, bits = [];
        if (w.time !== undefined) bits.push('time ' + w.time);
        if (w.weather) bits.push(w.weather);
        return '<li class="sess">' +
          '<h4>' + esc(x.plugin || 'Unnamed plugin') + (x.version ? ' <span class="pill">' + esc(x.version) + '</span>' : '') +
            ' <span class="pill ' + x.state + '">' + (x.state === 'live' ? 'live' : 'quiet') + '</span></h4>' +
          '<button type="button" class="mini" data-kick="' + esc(x.id) + '">Disconnect</button>' +
          '<div class="meta">' + [x.world && 'World "' + esc(x.world) + '"', x.server && esc(x.server), esc(x.addr), 'connected ' + since(x.started), 'last message ' + since(x.last),
            num(x.players) + ' online', num(x.chunks) + ' chunks', x.droppedChunks ? '<span class="warnlink">' + num(x.droppedChunks) + ' dropped (no map linked)</span>' : '', bits.join(', ')].filter(Boolean).join(' · ') + '</div>' +
          '<label class="linkrow">Draw into map <select data-link="' + esc(x.id) + '">' + opts + '</select>' +
            (!x.link ? '<span class="warnlink">Pick the map of this world</span>' : '') + '</label>' +
        '</li>';
      }).join('');
    }

    // players
    $('livePlayerCount').textContent = players;
    $('liveNoPlayers').hidden = players > 0;
    $('livePlayers').innerHTML = s.players.map(p =>
      '<li><b>' + esc(p.name) + '</b><span>' + (DIMS[p.dim] || 'dim ' + p.dim) + ' · ' + num(p.x) + ', ' + num(p.y) + ', ' + num(p.z) +
      (p.health !== null && p.health !== undefined ? ' · ' + Math.round(p.health) + ' hp' : '') + '</span></li>').join('');

    // activity (newest first)
    $('liveLog').innerHTML = s.log.map(l => '<li class="' + esc(l.kind) + '"><time>' + clock(l.t) + '</time><span>' + esc(l.text) + '</span></li>').join('');
  }

  $('tabLive').addEventListener('click', () => show(!open()));
  $('liveBack').addEventListener('click', () => show(false));
  for (const id of ['liveOn', 'livePort', 'liveLan']) $(id).addEventListener('input', () => { formDirty = true; $('liveLanHint').hidden = !$('liveLan').checked; });
  $('liveForm').addEventListener('submit', async e => {
    e.preventDefault();
    fail('');
    const port = parseInt($('livePort').value, 10);
    try {
      render(await call('live_configure', { enabled: $('liveOn').checked, port: Number.isFinite(port) ? port : 0, allowLan: $('liveLan').checked }));
      formDirty = false;
    } catch (err) { fail(err.message); }
  });
  $('liveShowKey').addEventListener('click', e => { showKey = !showKey; e.currentTarget.textContent = showKey ? 'Hide' : 'Show'; e.currentTarget.setAttribute('aria-pressed', String(showKey)); if (last) render(last); });
  async function copy(text, btn) {
    try { await navigator.clipboard.writeText(text); } catch { /* clipboard blocked: leave the text selectable */ }
    const was = btn.textContent; btn.textContent = 'Copied'; setTimeout(() => { btn.textContent = was; }, 1300);
  }
  $('liveCopyKey').addEventListener('click', e => { if (last) copy(last.config.key, e.currentTarget); });
  document.querySelectorAll('[data-copy]').forEach(b => b.addEventListener('click', () => copy($(b.dataset.copy).textContent, b)));
  $('liveNewKey').addEventListener('click', async () => {
    if (!confirm('Make a new key? Plugins using the current key stop working until you give them the new one.')) return;
    try { render(await call('live_new_key')); } catch (err) { fail(err.message); }
  });
  $('packInstall').addEventListener('click', async () => { fail(''); try { render(await call('live_pack', { reveal: false })); } catch (err) { fail(err.message); } });
  $('packSave').addEventListener('click', async () => { fail(''); try { render(await call('live_pack', { reveal: true })); } catch (err) { fail(err.message); } });
  $('autoOpen').addEventListener('change', async e => { try { render(await call('live_set_auto_open', { on: e.target.checked })); } catch (err) { fail(err.message); } });
  $('logEnable').addEventListener('click', async () => { fail(''); try { render(await call('live_enable_content_log')); } catch (err) { fail(err.message); } });
  $('liveSessions').addEventListener('change', async e => {
    const sel = e.target.closest('select[data-link]');
    if (!sel) return;
    try { const s = await call('live_link', { session: sel.dataset.link, slug: sel.value || null }); sel.blur(); render(s); }
    catch (err) { fail(err.message); }
  });
  $('liveSessions').addEventListener('click', async e => {
    const b = e.target.closest('[data-kick]');
    if (!b) return;
    try { render(await call('live_disconnect', { session: b.dataset.kick })); } catch (err) { fail(err.message); }
  });

  refresh().finally(schedule);
})();
