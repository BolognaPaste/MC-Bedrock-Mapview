/* 3D view: the colour tiles and height tiles of the map turned into blocky terrain, drawn with WebGL2.
   Every block column becomes a box from its surface down to its neighbours, so hills, cliffs and trees stand up.
   Near the camera each column is its own box; farther away 2x2, 4x4 and 8x8 columns share one (level of detail). */
window.Map3D = (() => {
  'use strict';
  const TILE = 512, G = 64, NONE = -32768;
  const FOV = 50 * Math.PI / 180, TAN = Math.tan(FOV / 2);
  const DMAX = 2000;     // farthest the camera may sit from the point it looks at
  const LOD = 3;         // a patch is split into four while the eye is closer than LOD x its size
  const MAX_TILES = 100; // 512x512 tiles kept on the GPU
  const SKY = {
    overworld: { zen: [0.22, 0.40, 0.68], hor: [0.68, 0.79, 0.90] },
    nether: { zen: [0.13, 0.03, 0.03], hor: [0.40, 0.12, 0.07] },
    end: { zen: [0.04, 0.02, 0.09], hor: [0.19, 0.13, 0.29] }
  };

  const TERRAIN_VS = `#version 300 es
precision highp float; precision highp int;
layout(location=0) in uvec4 a;        // cell x, cell z, face (0 top, 1 east, 2 south, 3 west skirt, 4 north skirt), corner
uniform highp isampler2D uH;          // 513x513 surface heights; the last row and column come from the next tiles
uniform sampler2D uC, uK;             // colour and cave tiles (mipmapped)
uniform int uCaves, uCell, uLv;
uniform ivec2 uO;                     // patch origin inside the tile, in blocks
uniform vec3 uRel;                    // tile origin minus the eye (y: minus the eye height)
uniform mat4 uVP;
uniform float uSkirt;
out vec3 vCol; out vec3 vPos; out vec2 vTex; out float vTop;

int hAt(ivec2 c) { return texelFetch(uH, min(uO + c * uCell + uCell / 2, ivec2(512)), 0).r; }
vec3 colAt(ivec2 c) {
  ivec2 t = min((uO + c * uCell) >> uLv, ivec2((512 >> uLv) - 1));
  vec3 rgb = texelFetch(uC, t, uLv).rgb;
  if (uCaves == 1) { vec4 k = texelFetch(uK, t, uLv); rgb = mix(rgb * 0.3, k.rgb, k.a); }
  return rgb;
}
void cull() { gl_Position = vec4(2.0, 2.0, 2.0, 1.0); vCol = vec3(0); vPos = vec3(0); vTex = vec2(0); vTop = 0.0; }

void main() {
  ivec2 cell = ivec2(a.xy); int face = int(a.z), k = int(a.w);
  int hs = hAt(cell);
  bool sOk = hs != ${NONE};
  float ys = float(hs) + 1.0, along = (k == 1 || k == 2) ? 1.0 : 0.0, y, shade;
  vec2 xz; vec3 col;
  vTop = 0.0;
  if (face == 0) {
    if (!sOk) { cull(); return; }
    xz = vec2(cell) + vec2(along, k >= 2 ? 1.0 : 0.0);
    y = ys; shade = 1.0; col = colAt(cell); vTop = 1.0;
  } else if (face <= 2) {
    ivec2 nc = cell + (face == 1 ? ivec2(1, 0) : ivec2(0, 1));
    int hn = hAt(nc);
    bool nOk = hn != ${NONE};
    if (!sOk && !nOk) { cull(); return; }
    float yn = float(hn) + 1.0;
    if (!sOk) ys = yn - 6.0;
    if (!nOk) yn = ys - 6.0;
    bool high = ys >= yn, atSelf = k < 2;
    y = atSelf ? ys : yn;
    xz = face == 1 ? vec2(float(cell.x + 1), float(cell.y) + along) : vec2(float(cell.x) + along, float(cell.y + 1));
    col = high ? colAt(cell) : colAt(nc);
    shade = face == 1 ? 0.62 : 0.8;          // Minecraft-style face light: east/west darker than north/south
    if (atSelf != high) shade *= 0.72;       // darker at the foot of a wall, a cheap ambient occlusion
  } else {
    if (!sOk) { cull(); return; }            // skirts hide cracks where patches of different detail meet
    y = k < 2 ? ys : ys - uSkirt;
    xz = face == 3 ? vec2(float(cell.x), float(cell.y) + along) : vec2(float(cell.x) + along, float(cell.y));
    shade = face == 3 ? 0.62 : 0.8; col = colAt(cell);
  }
  vec2 tex = vec2(uO) + xz * float(uCell);
  vec3 p = vec3(uRel.x + tex.x, y + uRel.y, uRel.z + tex.y);
  vTex = tex; vPos = p; vCol = col * shade;
  gl_Position = uVP * vec4(p, 1.0);
}`;

  const TERRAIN_FS = `#version 300 es
precision highp float;
in vec3 vCol; in vec3 vPos; in vec2 vTex; in float vTop;
uniform vec3 uFog, uGridCol;
uniform vec2 uFogR;
uniform int uGrid;
out vec4 o;
void main() {
  vec3 c = vCol;
  if (uGrid == 1 && vTop > 0.5) {
    vec2 g = abs(fract(vTex / 16.0 + 0.5) - 0.5) * 16.0, w = max(fwidth(vTex), vec2(1e-4));
    c = mix(c, uGridCol, (1.0 - clamp(min(g.x / w.x, g.y / w.y), 0.0, 1.0)) * 0.55);
  }
  o = vec4(mix(c, uFog, smoothstep(uFogR.x, uFogR.y, length(vPos))), 1.0);
}`;

  const SKY_VS = `#version 300 es
out vec2 vN;
void main() { vec2 p = vec2(float((gl_VertexID << 1) & 2), float(gl_VertexID & 2)) * 2.0 - 1.0; vN = p; gl_Position = vec4(p, 0.0, 1.0); }`;

  const SKY_FS = `#version 300 es
precision highp float;
in vec2 vN;
uniform vec3 uF, uR, uU, uZen, uHor;
uniform vec2 uTan;
out vec4 o;
void main() {
  vec3 d = normalize(uF + uR * vN.x * uTan.x + uU * vN.y * uTan.y);
  vec3 c = d.y > 0.0 ? mix(uHor, uZen, pow(d.y, 0.55)) : uHor * (1.0 + d.y * 0.35);
  o = vec4(c, 1.0);
}`;

  function compile(gl, vs, fs) {
    const p = gl.createProgram();
    for (const [type, src] of [[gl.VERTEX_SHADER, vs], [gl.FRAGMENT_SHADER, fs]]) {
      const s = gl.createShader(type);
      gl.shaderSource(s, src); gl.compileShader(s);
      if (!gl.getShaderParameter(s, gl.COMPILE_STATUS)) throw new Error(gl.getShaderInfoLog(s));
      gl.attachShader(p, s);
    }
    gl.linkProgram(p);
    if (!gl.getProgramParameter(p, gl.LINK_STATUS)) throw new Error(gl.getProgramInfoLog(p));
    const u = {}, n = gl.getProgramParameter(p, gl.ACTIVE_UNIFORMS);
    for (let i = 0; i < n; i++) { const name = gl.getActiveUniform(p, i).name; u[name] = gl.getUniformLocation(p, name); }
    return { p, u: new Proxy(u, { get: (t, k) => t[k] || null }) }; // uniforms the compiler dropped read as null (a no-op)
  }

  // One shared patch mesh of GxG cells: a top face and east and south walls per cell, plus skirts along the west and north edges.
  function buildGrid(gl) {
    const v = [], idx = [];
    const quad = (i, j, face) => { const b = v.length / 4; for (let k = 0; k < 4; k++) v.push(i, j, face, k); idx.push(b, b + 1, b + 2, b, b + 2, b + 3); };
    for (let j = 0; j < G; j++) for (let i = 0; i < G; i++) {
      quad(i, j, 0); quad(i, j, 1); quad(i, j, 2);
      if (i === 0) quad(i, j, 3);
      if (j === 0) quad(i, j, 4);
    }
    const vao = gl.createVertexArray();
    gl.bindVertexArray(vao);
    gl.bindBuffer(gl.ARRAY_BUFFER, gl.createBuffer());
    gl.bufferData(gl.ARRAY_BUFFER, new Uint8Array(v), gl.STATIC_DRAW);
    gl.enableVertexAttribArray(0);
    gl.vertexAttribIPointer(0, 4, gl.UNSIGNED_BYTE, 4, 0);
    gl.bindBuffer(gl.ELEMENT_ARRAY_BUFFER, gl.createBuffer());
    gl.bufferData(gl.ELEMENT_ARRAY_BUFFER, new Uint16Array(idx), gl.STATIC_DRAW);
    gl.bindVertexArray(null);
    return { vao, count: idx.length };
  }

  const sub = (a, b) => [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
  const dot = (a, b) => a[0] * b[0] + a[1] * b[1] + a[2] * b[2];

  function perspective(near, far, aspect) {
    const f = 1 / TAN, nf = 1 / (near - far);
    return [f / aspect, 0, 0, 0, 0, f, 0, 0, 0, 0, (far + near) * nf, -1, 0, 0, 2 * far * near * nf, 0];
  }
  function mul(a, b) { // column-major 4x4: a * b
    const o = new Float32Array(16);
    for (let c = 0; c < 4; c++) for (let r = 0; r < 4; r++) {
      let s = 0;
      for (let k = 0; k < 4; k++) s += a[k * 4 + r] * b[c * 4 + k];
      o[c * 4 + r] = s;
    }
    return o;
  }
  function frustum(m) { // planes (a, b, c, d) facing inwards, from a view-projection matrix
    const row = r => [m[r], m[4 + r], m[8 + r], m[12 + r]], w = row(3), out = [];
    for (const r of [0, 1, 2]) {
      const p = row(r);
      out.push(w.map((x, i) => x + p[i]), w.map((x, i) => x - p[i]));
    }
    return out;
  }
  function boxVisible(planes, x0, y0, z0, x1, y1, z1) {
    for (const p of planes) if (p[0] * (p[0] > 0 ? x1 : x0) + p[1] * (p[1] > 0 ? y1 : y0) + p[2] * (p[2] > 0 ? z1 : z0) + p[3] < 0) return false;
    return true;
  }
  const boxDist = (e, x0, y0, z0, x1, y1, z1) => Math.hypot(Math.max(x0 - e[0], 0, e[0] - x1), Math.max(y0 - e[1], 0, e[1] - y1), Math.max(z0 - e[2], 0, e[2] - z1));

  /* host: { dim(), heightAt(x, z), heightTile(tx, tz), image(tx, tz, group) }
     heightTile returns null when the map has no such tile, otherwise { h: Int16Array | null while loading, min, max, bad }. */
  function create(canvas, host) {
    const gl = canvas.getContext('webgl2', { antialias: true, alpha: false, powerPreference: 'high-performance' });
    if (!gl) return null;
    let terrain, sky, grid;
    try {
      terrain = compile(gl, TERRAIN_VS, TERRAIN_FS);
      sky = compile(gl, SKY_VS, SKY_FS);
      grid = buildGrid(gl);
    } catch (e) { console.error('3D view unavailable:', e); return null; }
    const skyVao = gl.createVertexArray();
    gl.pixelStorei(gl.UNPACK_ALIGNMENT, 1);
    gl.pixelStorei(gl.UNPACK_COLORSPACE_CONVERSION_WEBGL, gl.NONE);
    gl.pixelStorei(gl.UNPACK_PREMULTIPLY_ALPHA_WEBGL, false);

    const blank = gl.createTexture();
    gl.bindTexture(gl.TEXTURE_2D, blank);
    gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA8, 1, 1, 0, gl.RGBA, gl.UNSIGNED_BYTE, new Uint8Array(4));
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.NEAREST);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.NEAREST);

    const recs = new Map();
    let frame = 0, ty = null, cam = null, lost = false;
    canvas.addEventListener('webglcontextlost', e => { e.preventDefault(); lost = true; });

    function colourTexture(img) {
      const t = gl.createTexture();
      gl.bindTexture(gl.TEXTURE_2D, t);
      gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA8, gl.RGBA, gl.UNSIGNED_BYTE, img);
      gl.generateMipmap(gl.TEXTURE_2D);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.NEAREST_MIPMAP_NEAREST);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.NEAREST);
      return t;
    }

    function makeRec(tx, tz, ht, img) {
      const h = new Int16Array(513 * 513).fill(NONE);
      for (let z = 0; z < TILE; z++) h.set(ht.h.subarray(z * TILE, z * TILE + TILE), z * 513);
      const tex = gl.createTexture();
      gl.bindTexture(gl.TEXTURE_2D, tex);
      gl.texImage2D(gl.TEXTURE_2D, 0, gl.R16I, 513, 513, 0, gl.RED_INTEGER, gl.SHORT, h);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.NEAREST);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.NEAREST);
      return { tx, tz, h, tex, col: colourTexture(img), cave: null, min: ht.min, max: ht.max, need: 3, used: frame };
    }

    // The last column and row of a tile's height texture hold the first column and row of its east and south neighbours.
    function fillBorders(r) {
      const side = (bit, dx, dz) => {
        if (!(r.need & bit)) return;
        const n = host.heightTile(r.tx + dx, r.tz + dz);
        if (n && !n.bad && !n.h) return; // still loading
        r.need &= ~bit;
        if (!n || n.bad) return;
        gl.bindTexture(gl.TEXTURE_2D, r.tex);
        if (dx) {
          const col = new Int16Array(TILE);
          for (let z = 0; z < TILE; z++) col[z] = n.h[z * TILE];
          gl.texSubImage2D(gl.TEXTURE_2D, 0, 512, 0, 1, TILE, gl.RED_INTEGER, gl.SHORT, col);
        } else {
          gl.texSubImage2D(gl.TEXTURE_2D, 0, 0, 512, TILE, 1, gl.RED_INTEGER, gl.SHORT, n.h.subarray(0, TILE));
        }
      };
      side(1, 1, 0); side(2, 0, 1);
      if (r.need & 4) {
        const n = host.heightTile(r.tx + 1, r.tz + 1);
        if (!(n && !n.bad && !n.h)) {
          r.need &= ~4;
          if (n && !n.bad) { gl.bindTexture(gl.TEXTURE_2D, r.tex); gl.texSubImage2D(gl.TEXTURE_2D, 0, 512, 512, 1, 1, gl.RED_INTEGER, gl.SHORT, n.h.subarray(0, 1)); }
        }
      }
    }

    function drop(key) {
      const r = recs.get(key);
      if (!r) return;
      gl.deleteTexture(r.tex); gl.deleteTexture(r.col);
      if (r.cave) gl.deleteTexture(r.cave);
      recs.delete(key);
    }
    function evictOne() {
      let worst = null;
      for (const [k, r] of recs) if (r.used < frame && (!worst || r.used < recs.get(worst).used)) worst = k;
      if (worst) drop(worst);
      return !!worst;
    }

    // Camera basis from yaw (0 = looking north) and pitch (angle below the horizon).
    function basis(yaw, pitch) {
      const cp = Math.cos(pitch);
      const F = [-Math.sin(yaw) * cp, -Math.sin(pitch), -Math.cos(yaw) * cp];
      const R = [Math.cos(yaw), 0, -Math.sin(yaw)];
      const U = [R[1] * F[2] - R[2] * F[1], R[2] * F[0] - R[0] * F[2], R[0] * F[1] - R[1] * F[0]];
      return { F, R, U };
    }

    function render(o) {
      if (lost) return false;
      const w = Math.round(o.W * o.dpr), hpx = Math.round(o.H * o.dpr);
      if (canvas.width !== w || canvas.height !== hpx) { canvas.width = w; canvas.height = hpx; }
      const dim = host.dim(), name = dim && dim.name in SKY ? dim.name : 'overworld';
      const dist = Math.min(DMAX, o.H / o.s / (2 * TAN));
      let again = false;

      // Look at the ground under the view centre; ease towards it so the camera does not jump at cliffs.
      const hv = host.heightAt(Math.floor(o.x), Math.floor(o.z));
      if (typeof hv === 'number') {
        const want = hv + 1;
        if (ty === null || Math.abs(want - ty) < 0.05) ty = want;
        else { ty += (want - ty) * 0.3; again = true; }
      }
      if (ty === null) ty = dim && dim.dim === 1 ? 32 : 64;
      const { F, R, U } = basis(o.yaw, o.pitch);
      const eye = [o.x - F[0] * dist, ty - F[1] * dist, o.z - F[2] * dist];
      const he = host.heightAt(Math.floor(eye[0]), Math.floor(eye[2]));
      if (typeof he === 'number' && eye[1] < he + 3) eye[1] = he + 3;

      const reach = Math.min(4200, dist * 1.5 + 1200);
      const near = Math.max(0.1, Math.min(8, dist * 0.01)), far = reach + 200, aspect = o.W / o.H;
      const view = [R[0], U[0], -F[0], 0, R[1], U[1], -F[1], 0, R[2], U[2], -F[2], 0, 0, 0, 0, 1];
      const vp = mul(perspective(near, far, aspect), view), planes = frustum(vp);
      cam = { eye, F, R, U, aspect, near, reach, ty, W: o.W, H: o.H };
      frame++;

      // Which tiles: inside the view and closer than the fog.
      const sets = dim && dim.sets ? dim.sets[0] : null;
      const cand = [];
      const tx0 = Math.floor((eye[0] - reach) / TILE), tx1 = Math.floor((eye[0] + reach) / TILE);
      const tz0 = Math.floor((eye[2] - reach) / TILE), tz1 = Math.floor((eye[2] + reach) / TILE);
      for (let tz = tz0; tz <= tz1; tz++) for (let tx = tx0; tx <= tx1; tx++) {
        const key = tx + ',' + tz;
        if (sets && !sets.has(key)) continue;
        const r = recs.get(key), lo = r ? r.min : -64, hi = r ? r.max + 1 : 320;
        const x0 = tx * TILE - eye[0], z0 = tz * TILE - eye[2], y0 = lo - eye[1], y1 = hi - eye[1];
        const d = boxDist([0, 0, 0], x0, y0, z0, x0 + TILE, y1, z0 + TILE);
        if (d > reach || !boxVisible(planes, x0, y0, z0, x0 + TILE, y1, z0 + TILE)) continue;
        cand.push({ key, tx, tz, d });
      }
      cand.sort((a, b) => a.d - b.d);
      const shown = [];
      let made = 0;
      for (const c of cand) {
        let r = recs.get(c.key);
        if (!r) {
          const ht = host.heightTile(c.tx, c.tz);
          if (!ht || ht.bad) continue;
          const img = host.image(c.tx, c.tz, 'tiles');
          if (!ht.h || !img.ok) continue; // their onload asks for another frame
          if (made >= 3) { again = true; continue; }
          if (recs.size >= MAX_TILES && !evictOne()) continue;
          r = makeRec(c.tx, c.tz, ht, img.img);
          recs.set(c.key, r);
          made++;
        }
        r.used = frame;
        if (r.need) fillBorders(r);
        if (o.caves && r.cave === null && dim.caves) {
          const k = host.image(c.tx, c.tz, 'caves');
          if (k.ok) r.cave = colourTexture(k.img); else if (k.bad) r.cave = false;
        }
        shown.push(r);
      }

      // Split each tile into patches: fine ones near the eye, coarse ones far away.
      const patches = [];
      for (const r of shown) {
        const bx = r.tx * TILE - eye[0], bz = r.tz * TILE - eye[2], y0 = r.min - eye[1], y1 = r.max + 1 - eye[1];
        const walk = (ox, oz, size) => {
          const x0 = bx + ox, z0 = bz + oz;
          if (!boxVisible(planes, x0, y0 - 16, z0, x0 + size, y1, z0 + size)) return;
          const d = boxDist([0, 0, 0], x0, y0, z0, x0 + size, y1, z0 + size);
          if (d > reach) return;
          if (size > G && d < size * LOD) {
            const h = size / 2;
            walk(ox, oz, h); walk(ox + h, oz, h); walk(ox, oz + h, h); walk(ox + h, oz + h, h);
          } else patches.push({ r, ox, oz, size, d });
        };
        walk(0, 0, TILE);
      }
      patches.sort((a, b) => a.d - b.d);

      gl.viewport(0, 0, w, hpx);
      const s = SKY[name];
      gl.disable(gl.DEPTH_TEST); gl.depthMask(false);
      gl.useProgram(sky.p);
      gl.uniform3fv(sky.u.uF, F); gl.uniform3fv(sky.u.uR, R); gl.uniform3fv(sky.u.uU, U);
      gl.uniform3fv(sky.u.uZen, s.zen); gl.uniform3fv(sky.u.uHor, s.hor);
      gl.uniform2f(sky.u.uTan, TAN * aspect, TAN);
      gl.bindVertexArray(skyVao);
      gl.drawArrays(gl.TRIANGLES, 0, 3);

      gl.enable(gl.DEPTH_TEST); gl.depthMask(true); gl.disable(gl.CULL_FACE);
      gl.clear(gl.DEPTH_BUFFER_BIT);
      const u = terrain.u;
      gl.useProgram(terrain.p);
      gl.uniformMatrix4fv(u.uVP, false, vp);
      gl.uniform1i(u.uH, 0); gl.uniform1i(u.uC, 1); gl.uniform1i(u.uK, 2);
      gl.uniform3fv(u.uFog, s.hor);
      gl.uniform2f(u.uFogR, Math.max(dist * 1.05, reach * 0.5), reach);
      gl.uniform1i(u.uGrid, o.grid ? 1 : 0);
      gl.uniform3fv(u.uGridCol, o.gridCol || [1, 0.6, 0.24]);
      gl.bindVertexArray(grid.vao);
      let bound = null;
      for (const p of patches) {
        const r = p.r;
        if (bound !== r) {
          bound = r;
          gl.activeTexture(gl.TEXTURE0); gl.bindTexture(gl.TEXTURE_2D, r.tex);
          gl.activeTexture(gl.TEXTURE1); gl.bindTexture(gl.TEXTURE_2D, r.col);
          gl.activeTexture(gl.TEXTURE2); gl.bindTexture(gl.TEXTURE_2D, r.cave || blank);
          gl.uniform1i(u.uCaves, o.caves && r.cave ? 1 : 0);
          gl.uniform3f(u.uRel, r.tx * TILE - eye[0], -eye[1], r.tz * TILE - eye[2]);
        }
        const cell = p.size / G;
        gl.uniform2i(u.uO, p.ox, p.oz);
        gl.uniform1i(u.uCell, cell);
        gl.uniform1i(u.uLv, Math.log2(cell));
        gl.uniform1f(u.uSkirt, 8 + cell * 4);
        gl.drawElements(gl.TRIANGLES, grid.count, gl.UNSIGNED_SHORT, 0);
      }
      gl.bindVertexArray(null);
      return again;
    }

    // Screen point -> ray direction (world space, normalised).
    function ray(px, py) {
      const { F, R, U, aspect } = cam;
      const nx = (px / cam.W) * 2 - 1, ny = 1 - (py / cam.H) * 2;
      const d = [0, 1, 2].map(i => F[i] + R[i] * nx * TAN * aspect + U[i] * ny * TAN);
      const l = Math.hypot(d[0], d[1], d[2]);
      return d.map(v => v / l);
    }

    // The block under a screen point: { x, y, z } (y is null when the ray only met the plane at the view height).
    function pick(px, py) {
      if (!cam) return null;
      const d = ray(px, py), e = cam.eye;
      const top = 330, bot = -70;
      let t0 = 0, t1 = cam.reach;
      if (d[1] < 0) { t0 = Math.max(0, (top - e[1]) / d[1]); t1 = Math.min(t1, (bot - e[1]) / d[1]); }
      else if (e[1] > top) t1 = -1;
      const hit = t => {
        const x = e[0] + d[0] * t, y = e[1] + d[1] * t, z = e[2] + d[2] * t, h = host.heightAt(Math.floor(x), Math.floor(z));
        return typeof h === 'number' && y <= h + 1 ? h : null;
      };
      for (let t = t0, prev = t0; t <= t1; prev = t, t += 0.5) {
        if (hit(t) === null) continue;
        let a = prev, b = t;
        for (let i = 0; i < 8; i++) { const m = (a + b) / 2; if (hit(m) === null) a = m; else b = m; }
        const x = e[0] + d[0] * b, z = e[2] + d[2] * b;
        return { x, z, y: hit(b) };
      }
      if (d[1] < -1e-3) { const t = (cam.ty - e[1]) / d[1]; if (t > 0 && t < cam.reach) return { x: e[0] + d[0] * t, z: e[2] + d[2] * t, y: null }; }
      return null;
    }

    // Screen position of a world point; behind tells it is behind the camera (x, y then point the way to turn).
    function project(x, y, z) {
      if (!cam) return null;
      const r = sub([x, y, z], cam.eye), cx = dot(r, cam.R), cy = dot(r, cam.U), cz = dot(r, cam.F);
      if (cz < cam.near) {
        const l = Math.hypot(cx, cy) || 1;
        return { x: cam.W / 2 + (cx / l) * 1e5, y: cam.H / 2 - (cy / l) * 1e5, behind: true };
      }
      return { x: cam.W / 2 + (cx / (cz * TAN * cam.aspect)) * cam.W / 2, y: cam.H / 2 - (cy / (cz * TAN)) * cam.H / 2, behind: false };
    }

    // The patch of ground the screen shows, as four [x, z] corners (for the overview map).
    function footprint() {
      if (!cam) return null;
      const e = cam.eye;
      return [[0, 0], [cam.W, 0], [cam.W, cam.H], [0, cam.H]].map(([px, py]) => {
        const d = ray(px, py);
        let t = d[1] < -1e-3 ? (cam.ty - e[1]) / d[1] : Infinity;
        if (!(t > 0) || t > cam.reach) {
          const hl = Math.hypot(d[0], d[2]) || 1;
          return [e[0] + (d[0] / hl) * cam.reach, e[2] + (d[2] / hl) * cam.reach];
        }
        return [e[0] + d[0] * t, e[2] + d[2] * t];
      });
    }

    function reset() { for (const k of [...recs.keys()]) drop(k); ty = null; cam = null; }
    // A tile was redrawn (live link): upload it again next frame. Its neighbours' borders hold its edge too.
    function invalidate(tx, tz) {
      drop(tx + ',' + tz);
      for (const [dx, dz, bit] of [[-1, 0, 1], [0, -1, 2], [-1, -1, 4]]) { const r = recs.get((tx + dx) + ',' + (tz + dz)); if (r) r.need |= bit; }
    }

    return {
      render, pick, project, footprint, reset, invalidate,
      // zoom limits in pixels per block (at the point looked at)
      minScale: H => H / (2 * TAN * DMAX),
      maxScale: 96
    };
  }

  return { create };
})();
