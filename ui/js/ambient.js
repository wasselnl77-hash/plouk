// Two WebGL layers:
//  - background: slow, domain-warped glow pulled from the album palette, which
//    breathes with the bass and warps outward when ripples pass through it;
//  - ripples: thin liquid light rings drawn above the UI while you drag a
//    playlist over the window, and as a splash when you drop it.

import { DEFAULT_PALETTE } from './palette.js';

const VERT = `
attribute vec2 a;
varying vec2 vUv;
void main() { vUv = a * 0.5 + 0.5; gl_Position = vec4(a, 0.0, 1.0); }`;

const RIPPLE_LOOP = `
  for (int i = 0; i < 10; i++) {
    vec4 r = uRip[i];
    if (r.w <= 0.0) continue;
    float age = uTime - r.z;
    if (age < 0.0 || age > 4.0) continue;
    vec2 d = p - vec2(r.x * asp, r.y);
    float dist = length(d);
    float x = dist - age * 0.46;
    float fade = exp(-age * 1.15) * r.w * smoothstep(0.0, 0.06, age);
    RIPPLE_BODY
  }`;

const BG_FRAG = `
precision highp float;
uniform vec2 uRes;
uniform float uTime;
uniform vec3 uC0, uC1, uC2, uC3;
uniform float uBreath, uDrag;
uniform vec4 uRip[10];
varying vec2 vUv;

float hash(vec2 p) { return fract(sin(dot(p, vec2(127.1, 311.7))) * 43758.5453); }
float noise(vec2 p) {
  vec2 i = floor(p), f = fract(p);
  vec2 u = f * f * (3.0 - 2.0 * f);
  return mix(mix(hash(i), hash(i + vec2(1.0, 0.0)), u.x), mix(hash(i + vec2(0.0, 1.0)), hash(i + vec2(1.0, 1.0)), u.x), u.y);
}
float fbm(vec2 p) {
  float v = 0.0, a = 0.5;
  for (int i = 0; i < 4; i++) { v += a * noise(p); p *= 2.03; a *= 0.5; }
  return v;
}
float blob(vec2 q, vec2 c, float k) { vec2 d = q - c; return exp(-dot(d, d) * k); }

void main() {
  float asp = uRes.x / uRes.y;
  vec2 p = vec2(vUv.x * asp, vUv.y);
  vec2 disp = vec2(0.0);
  float crest = 0.0;
  ${RIPPLE_LOOP.replace('RIPPLE_BODY', `
    float env = exp(-x * x * 140.0) * fade;
    disp += (d / (dist + 1e-4)) * sin(x * 70.0) * env * 0.03;
    crest += env;`)}
  p += disp;

  float t = uTime * 0.055;
  vec2 q = p + 0.24 * vec2(fbm(p * 1.4 + t), fbm(p * 1.4 - t + 4.2)) - 0.12;
  float k = 1.0 + uBreath * 0.45;
  vec3 col = vec3(0.012, 0.012, 0.016);
  vec3 glow = vec3(0.0);
  glow += uC0 * blob(q, vec2(asp * (0.34 + 0.10 * sin(t * 2.1)), 0.64 + 0.09 * cos(t * 1.7)), 7.0) * 0.30;
  glow += uC1 * blob(q, vec2(asp * (0.80 + 0.08 * cos(t * 1.3)), 0.80 + 0.10 * sin(t * 2.4)), 9.0) * 0.20;
  glow += uC2 * blob(q, vec2(asp * (0.62 + 0.12 * sin(t * 1.1 + 1.0)), 0.16 + 0.08 * cos(t * 1.9)), 8.0) * 0.18;
  glow += uC3 * blob(q, vec2(asp * (0.10 + 0.08 * cos(t * 1.6 + 2.0)), 0.18 + 0.10 * sin(t * 1.2)), 10.0) * 0.14;
  // soft tone curve keeps the darks deep and the color pockets gentle
  glow = glow / (1.0 + glow * 1.6) * k;
  col += glow;
  col += mix(vec3(1.0), uC0 * 1.6, 0.5) * crest * 0.05;
  col *= 1.0 - uDrag * 0.15;

  float v = smoothstep(1.25, 0.15, length((vUv - 0.5) * vec2(asp * 0.85, 1.0)));
  col *= mix(0.35, 1.0, v);
  col += (hash(gl_FragCoord.xy + fract(uTime * 7.0)) - 0.5) * (2.5 / 255.0);
  gl_FragColor = vec4(col, 1.0);
}`;

const FX_FRAG = `
precision highp float;
uniform vec2 uRes;
uniform float uTime;
uniform vec3 uTint;
uniform vec4 uRip[10];
varying vec2 vUv;
void main() {
  float asp = uRes.x / uRes.y;
  vec2 p = vec2(vUv.x * asp, vUv.y);
  float a = 0.0, hot = 0.0;
  ${RIPPLE_LOOP.replace('RIPPLE_BODY', `
    float crest = exp(-x * x * 2600.0);
    float trough = exp(-(x + 0.02) * (x + 0.02) * 1700.0);
    float inner = exp(-(x + 0.055) * (x + 0.055) * 800.0);
    a += (crest * 0.6 + trough * 0.18 + inner * 0.12) * fade;
    hot += crest * fade;`)}
  a = clamp(a, 0.0, 0.7);
  vec3 col = mix(uTint, vec3(1.0), clamp(hot, 0.0, 1.0) * 0.65);
  gl_FragColor = vec4(col * a, a);
}`;

function program(gl, frag) {
  const sh = (type, src) => {
    const s = gl.createShader(type);
    gl.shaderSource(s, src);
    gl.compileShader(s);
    if (!gl.getShaderParameter(s, gl.COMPILE_STATUS)) throw new Error(gl.getShaderInfoLog(s));
    return s;
  };
  const p = gl.createProgram();
  gl.attachShader(p, sh(gl.VERTEX_SHADER, VERT));
  gl.attachShader(p, sh(gl.FRAGMENT_SHADER, frag));
  gl.linkProgram(p);
  if (!gl.getProgramParameter(p, gl.LINK_STATUS)) throw new Error(gl.getProgramInfoLog(p));
  const buf = gl.createBuffer();
  gl.bindBuffer(gl.ARRAY_BUFFER, buf);
  gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([-1, -1, 3, -1, -1, 3]), gl.STATIC_DRAW);
  const loc = gl.getAttribLocation(p, 'a');
  gl.enableVertexAttribArray(loc);
  gl.vertexAttribPointer(loc, 2, gl.FLOAT, false, 0, 0);
  gl.useProgram(p);
  const u = {};
  const n = gl.getProgramParameter(p, gl.ACTIVE_UNIFORMS);
  for (let i = 0; i < n; i++) {
    const name = gl.getActiveUniform(p, i).name.replace('[0]', '');
    u[name] = gl.getUniformLocation(p, name);
  }
  return u;
}

const MAX_RIPPLES = 10;

export class Ambient {
  constructor(bgCanvas, fxCanvas) {
    this.bg = bgCanvas;
    this.fx = fxCanvas;
    this.t0 = performance.now();
    this.ripples = [];
    this.breath = 0;
    this.paused = false;
    this.calm = true; // music paused: the glow can drift at a lower frame rate
    this.drag = 0;
    this.dragTarget = 0;
    this.colors = DEFAULT_PALETTE.glow.map((c) => [...c]);
    this.from = this.colors.map((c) => [...c]);
    this.to = this.colors.map((c) => [...c]);
    this.mixStart = 0;
    this.tint = [0.7, 0.64, 1];
    this.frame = 0;

    try {
      this.gl = bgCanvas.getContext('webgl', { antialias: false, alpha: false, powerPreference: 'low-power' });
      this.u = program(this.gl, BG_FRAG);
      this.gx = fxCanvas.getContext('webgl', { antialias: false, alpha: true, premultipliedAlpha: true });
      this.ux = program(this.gx, FX_FRAG);
    } catch (e) {
      console.warn('[plouk] WebGL unavailable, using a static backdrop', e);
      this.gl = null;
      bgCanvas.classList.add('fallback');
    }
    this.resize();
    window.addEventListener('resize', () => this.resize());
    this.loop = this.loop.bind(this);
    requestAnimationFrame(this.loop);
  }

  time() {
    return (performance.now() - this.t0) / 1000;
  }

  resize() {
    const w = this.bg.clientWidth || window.innerWidth;
    const h = this.bg.clientHeight || window.innerHeight;
    const dpr = window.devicePixelRatio || 1;
    // Lite mode draws the soft glow much smaller; it's blurry anyway.
    // The glow is soft, so a small buffer scaled up looks the same.
    const bgScale = this.lite ? 0.25 : 0.4;
    this.bg.width = Math.max(2, Math.round(w * bgScale));
    this.bg.height = Math.max(2, Math.round(h * bgScale));
    this.fxScale = this.lite ? 1 : Math.min(dpr, 1.5);
    this.w = w;
    this.h = h;
    this.sizeFx(this.ripples.length > 0);
  }

  /** The ripple layer is full resolution only while ripples run; otherwise it's 1×1 and costs nothing. */
  sizeFx(on) {
    const w = on ? Math.max(2, Math.round(this.w * this.fxScale)) : 1;
    const h = on ? Math.max(2, Math.round(this.h * this.fxScale)) : 1;
    if (this.fx.width !== w || this.fx.height !== h) {
      this.fx.width = w;
      this.fx.height = h;
    }
  }

  setPalette(glow, accent) {
    this.from = this.colors.map((c) => [...c]);
    this.to = glow.map((c) => [...c]);
    this.mixStart = performance.now();
    this.tint = accent.map((v) => v / 255);
  }

  ripple(clientX, clientY, strength = 1) {
    const r = this.bg.getBoundingClientRect();
    this.ripples.push({ x: (clientX - r.left) / r.width, y: 1 - (clientY - r.top) / r.height, t: this.time(), s: strength });
    this.sizeFx(true);
    if (this.ripples.length > MAX_RIPPLES) this.ripples.shift();
  }

  setDragging(on) {
    this.dragTarget = on ? 1 : 0;
  }

  ripplesUniform(now) {
    const arr = new Float32Array(MAX_RIPPLES * 4);
    this.ripples = this.ripples.filter((r) => now - r.t < 4);
    this.ripples.forEach((r, i) => arr.set([r.x, r.y, r.t, r.s], i * 4));
    return arr;
  }

  loop() {
    requestAnimationFrame(this.loop);
    if (!this.gl || document.hidden || this.paused) return;
    this.frame++;
    const now = this.time();
    const active = this.ripples.length > 0 || this.drag > 0.01 || this.dragTarget > 0;
    // The glow moves slowly: 30 fps is plenty unless ripples run, 15 while paused, less in lite mode.
    if (!active && this.frame % (this.lite ? (this.calm ? 6 : 4) : this.calm ? 4 : 2)) return;

    const k = Math.min(1, (performance.now() - this.mixStart) / 1800);
    const e = k * k * (3 - 2 * k);
    for (let i = 0; i < 4; i++) for (let j = 0; j < 3; j++) this.colors[i][j] = this.from[i][j] + (this.to[i][j] - this.from[i][j]) * e;
    this.drag += (this.dragTarget - this.drag) * 0.08;
    const rip = this.ripplesUniform(now);

    const gl = this.gl, u = this.u;
    gl.viewport(0, 0, this.bg.width, this.bg.height);
    gl.uniform2f(u.uRes, this.bg.width, this.bg.height);
    gl.uniform1f(u.uTime, now);
    gl.uniform3fv(u.uC0, this.colors[0]);
    gl.uniform3fv(u.uC1, this.colors[1]);
    gl.uniform3fv(u.uC2, this.colors[2]);
    gl.uniform3fv(u.uC3, this.colors[3]);
    gl.uniform1f(u.uBreath, this.breath);
    gl.uniform1f(u.uDrag, this.drag);
    gl.uniform4fv(u.uRip, rip);
    gl.drawArrays(gl.TRIANGLES, 0, 3);

    const gx = this.gx, ux = this.ux;
    if (this.ripples.length || this.fxDirty) {
      gx.viewport(0, 0, this.fx.width, this.fx.height);
      gx.clearColor(0, 0, 0, 0);
      gx.clear(gx.COLOR_BUFFER_BIT);
      if (this.ripples.length) {
        gx.uniform2f(ux.uRes, this.fx.width, this.fx.height);
        gx.uniform1f(ux.uTime, now);
        gx.uniform3fv(ux.uTint, this.tint);
        gx.uniform4fv(ux.uRip, rip);
        gx.drawArrays(gx.TRIANGLES, 0, 3);
      }
      this.fxDirty = this.ripples.length > 0;
      if (!this.fxDirty) this.sizeFx(false);
    }
  }
}
