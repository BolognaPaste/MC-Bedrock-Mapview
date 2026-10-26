/* Falling autumn leaves. Draws on one fixed canvas that ignores the mouse.
   window.Leaves: .available (false when the system asks for reduced motion), .on, .set(true|false).
   The choice is remembered in this browser. This file is the same in ui/ and viewer/. */
(() => {
  'use strict';
  const KEY = 'bedrock-map-leaves';
  const reduce = !!(window.matchMedia && window.matchMedia('(prefers-reduced-motion: reduce)').matches);
  const COLORS = ['#d9480f', '#f76707', '#f59f00', '#c92a2a', '#b5471b', '#e8a317', '#8f3a12', '#cf6a1c'];
  let on = true;
  try { if (localStorage.getItem(KEY) === 'off') on = false; } catch {}

  const api = window.Leaves = {
    available: !reduce,
    get on() { return on && !reduce; },
    set(v) {
      on = !!v;
      try { localStorage.setItem(KEY, on ? 'on' : 'off'); } catch {}
      if (reduce) return;
      if (on) start(); else { running = false; ctx.clearRect(0, 0, cv.width, cv.height); }
    }
  };
  if (reduce || !document.body) return;

  const cv = document.createElement('canvas');
  cv.id = 'leaves'; cv.setAttribute('aria-hidden', 'true');
  document.body.appendChild(cv);
  const ctx = cv.getContext('2d');
  let W = 0, H = 0, dpr = 1, running = false, last = 0, clock = 0;
  const leaves = [];

  /* ---- leaf sprites: three shapes drawn once per colour ---- */
  const shade = (hex, f) => { const n = parseInt(hex.slice(1), 16); return 'rgb(' + [n >> 16, (n >> 8) & 255, n & 255].map(v => Math.round(v * f)).join(',') + ')'; };

  const SHAPES = {
    maple(c) {
      const R = [[0.16, -0.66], [0.38, -0.80], [0.33, -0.34], [0.72, -0.48], [0.58, -0.12], [0.98, 0.0], [0.52, 0.30], [0.62, 0.52], [0.10, 0.42], [0.06, 0.62]];
      c.beginPath(); c.moveTo(0, -1);
      for (const [x, y] of R) c.lineTo(x, y);
      for (let i = R.length - 1; i >= 0; i--) c.lineTo(-R[i][0], R[i][1]);
      c.closePath();
      return { stem: [0, 0.6, 0, 1.08], veins: [[0, -0.85], [0.55, -0.5], [0.88, 0], [-0.55, -0.5], [-0.88, 0]], from: [0, 0.42] };
    },
    oak(c) {
      c.beginPath();
      for (let i = 0; i <= 72; i++) {
        const t = i / 72 * Math.PI * 2, lobe = 1 + 0.15 * Math.sin(t * 7);
        const x = 0.5 * lobe * Math.cos(t), y = 0.92 * lobe * Math.sin(t);
        if (i) c.lineTo(x, y); else c.moveTo(x, y);
      }
      c.closePath();
      return { stem: [0, 0.9, 0, 1.1], veins: [[0, -0.85], [0.38, -0.35], [0.4, 0.15], [-0.38, -0.35], [-0.4, 0.15]], from: [0, 0.8] };
    },
    aspen(c) {
      c.beginPath(); c.moveTo(0, -1);
      c.bezierCurveTo(0.95, -0.4, 0.8, 0.5, 0, 0.8);
      c.bezierCurveTo(-0.8, 0.5, -0.95, -0.4, 0, -1);
      c.closePath();
      return { stem: [0, 0.78, 0, 1.08], veins: [[0, -0.9], [0.5, -0.2], [-0.5, -0.2]], from: [0, 0.7] };
    }
  };
  const NAMES = ['maple', 'maple', 'maple', 'oak', 'oak', 'aspen'];
  const sprites = new Map();

  function sprite(shape, ci) {
    const key = shape + ci;
    let s = sprites.get(key);
    if (s) return s;
    s = document.createElement('canvas'); s.width = s.height = 96;
    const c = s.getContext('2d');
    c.translate(48, 48); c.scale(40, 40);
    const d = SHAPES[shape](c);
    const g = c.createLinearGradient(-0.6, -0.9, 0.6, 0.9);
    g.addColorStop(0, COLORS[ci]); g.addColorStop(1, shade(COLORS[ci], 0.72));
    c.fillStyle = g; c.fill();
    c.lineJoin = 'round'; c.lineWidth = 0.05; c.strokeStyle = shade(COLORS[ci], 0.55); c.stroke();
    c.strokeStyle = 'rgba(50,15,0,.45)'; c.lineWidth = 0.035; c.lineCap = 'round'; c.beginPath();
    for (const [x, y] of d.veins) { c.moveTo(d.from[0], d.from[1]); c.lineTo(x, y); }
    c.stroke();
    c.strokeStyle = shade(COLORS[ci], 0.5); c.lineWidth = 0.07; c.beginPath(); c.moveTo(d.stem[0], d.stem[1]); c.lineTo(d.stem[2], d.stem[3]); c.stroke();
    sprites.set(key, s);
    return s;
  }

  /* ---- motion ---- */
  function spawn(p, first) {
    const size = 22 + Math.random() * 26;
    p.size = size; p.depth = Math.min(1, size / 44);
    p.sprite = sprite(NAMES[Math.floor(Math.random() * NAMES.length)], Math.floor(Math.random() * COLORS.length));
    p.vy = 34 + p.depth * 52 + Math.random() * 22;
    p.amp = 14 + Math.random() * 30; p.freq = 0.6 + Math.random() * 0.9; p.phase = Math.random() * 6.28;
    p.rot = Math.random() * 6.28; p.rotV = (Math.random() - 0.5) * 1.6;
    p.flip = Math.random() * 6.28; p.flipV = 1.4 + Math.random() * 2.2;
    p.x = Math.random() * W;
    p.y = first ? -30 - Math.random() * H * 1.1 : -40 - Math.random() * 90;
    return p;
  }

  function resize() {
    dpr = Math.min(window.devicePixelRatio || 1, 2);
    W = window.innerWidth; H = window.innerHeight;
    cv.width = Math.round(W * dpr); cv.height = Math.round(H * dpr);
    const want = Math.max(16, Math.min(52, Math.round(W / 34)));
    while (leaves.length < want) leaves.push(spawn({}, true));
    leaves.length = want;
  }

  function frame(t) {
    if (!running) return;
    const dt = Math.min(0.05, (t - last) / 1000 || 0.016); last = t; clock += dt;
    const wind = 8 + 12 * Math.sin(clock * 0.23) + 6 * Math.sin(clock * 0.61);
    ctx.setTransform(1, 0, 0, 1, 0, 0);
    ctx.clearRect(0, 0, cv.width, cv.height);
    for (const p of leaves) {
      p.phase += p.freq * dt; p.flip += p.flipV * dt;
      p.x += (wind + p.amp * Math.cos(p.phase)) * dt * (0.5 + p.depth * 0.5);
      p.y += p.vy * dt;
      p.rot += (p.rotV + 0.5 * Math.cos(p.phase)) * dt;
      if (p.y > H + 40) spawn(p, false);
      if (p.x < -40) p.x = W + 40; else if (p.x > W + 40) p.x = -40;
      const f = Math.cos(p.flip), sx = (f < 0 ? -1 : 1) * Math.max(0.2, Math.abs(f));
      const cs = Math.cos(p.rot) * p.size * dpr, sn = Math.sin(p.rot) * p.size * dpr;
      ctx.setTransform(cs * sx, sn * sx, -sn, cs, p.x * dpr, p.y * dpr);
      ctx.globalAlpha = 0.62 + 0.38 * p.depth;
      ctx.drawImage(p.sprite, -0.5, -0.5, 1, 1);
    }
    requestAnimationFrame(frame);
  }

  function start() { if (running) return; running = true; last = performance.now(); requestAnimationFrame(frame); }

  resize();
  window.addEventListener('resize', resize);
  if (on) start();
})();
