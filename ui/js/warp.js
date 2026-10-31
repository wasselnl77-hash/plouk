// The options transition: the interface warps out of the way. The middle bulges
// and bends outward while a radial zoom blur streaks everything from the centre,
// and the options panel comes through it. Plouk snapshots its own window (Rust)
// and warps that image on the GPU; without a snapshot it falls back to CSS.

import { invoke } from './api.js';
import { tween, ease } from './motion.js';

const VERT = `
attribute vec2 a;
varying vec2 vUv;
void main() { vUv = a * 0.5 + 0.5; gl_Position = vec4(a, 0.0, 1.0); }`;

const FRAG = `
precision highp float;
uniform sampler2D uTex;
uniform vec2 uRes;
uniform float uAmt, uTime, uOpaque;
varying vec2 vUv;

float hash(vec2 p) { return fract(sin(dot(p, vec2(127.1, 311.7))) * 43758.5453); }

vec4 snap(vec2 uv) {
  vec4 c = texture2D(uTex, vec2(uv.x, 1.0 - uv.y)).bgra;
  c.a = mix(c.a, 1.0, uOpaque);
  float inside = step(0.0, uv.x) * step(uv.x, 1.0) * step(0.0, uv.y) * step(uv.y, 1.0);
  return c * inside;
}

void main() {
  float asp = uRes.x / uRes.y;
  vec2 d = vec2((vUv.x - 0.5) * asp, vUv.y - 0.5);
  float r = length(d);
  float a = uAmt;

  // Bending: the centre bulges and shoves everything outward, with a soft twist.
  float bulge = a * 0.46 * exp(-r * r * 2.4);
  float twist = a * (0.28 * exp(-r * r * 3.0) + 0.015 * sin(uTime * 0.9));
  float cs = cos(twist), sn = sin(twist);
  vec2 bent = vec2(cs * d.x - sn * d.y, sn * d.x + cs * d.y) * (1.0 - bulge);

  // Radial zoom blur toward the centre, stronger farther out. Each pixel starts
  // at a random offset so the samples melt into a smooth streak, not copies.
  float spread = a * (0.03 + 0.26 * r);
  float jitter = hash(gl_FragCoord.xy);
  vec4 acc = vec4(0.0);
  float wsum = 0.0;
  for (int i = 0; i < 28; i++) {
    float t = (float(i) + jitter) / 28.0;
    float w = 1.0 - 0.65 * t;
    vec2 p = bent * (1.0 - spread * t);
    acc += snap(vec2(p.x / asp + 0.5, p.y + 0.5)) * w;
    wsum += w;
  }
  vec4 col = acc / wsum;

  // A little colour fringing on the streaks.
  float ca = a * 0.009 * r;
  vec2 pr = bent * (1.0 + ca), pb = bent * (1.0 - ca);
  col.r = mix(col.r, snap(vec2(pr.x / asp + 0.5, pr.y + 0.5)).r, 0.2 * a);
  col.b = mix(col.b, snap(vec2(pb.x / asp + 0.5, pb.y + 0.5)).b, 0.2 * a);

  // Dim as it warps so the panel stands out.
  col.rgb *= 1.0 - a * (0.38 + 0.18 * smoothstep(0.2, 0.9, r));
  gl_FragColor = col;
}`;

const PIECES = ['.titlebar', '.sidebar', '.stage', '.panel'];
const reduced = matchMedia('(prefers-reduced-motion: reduce)');
const wait = (ms) => new Promise((r) => setTimeout(r, ms));

export class Warp {
  constructor(canvas, ambient) {
    this.canvas = canvas;
    this.ambient = ambient;
    this.amt = 0;
    this.live = false;
    this.cssAnims = [];
    this.t0 = performance.now();
    try {
      const gl = (this.gl = canvas.getContext('webgl', { premultipliedAlpha: true, alpha: true, antialias: false }));
      const sh = (type, src) => {
        const s = gl.createShader(type);
        gl.shaderSource(s, src);
        gl.compileShader(s);
        if (!gl.getShaderParameter(s, gl.COMPILE_STATUS)) throw new Error(gl.getShaderInfoLog(s));
        return s;
      };
      const p = gl.createProgram();
      gl.attachShader(p, sh(gl.VERTEX_SHADER, VERT));
      gl.attachShader(p, sh(gl.FRAGMENT_SHADER, FRAG));
      gl.linkProgram(p);
      gl.useProgram(p);
      gl.bindBuffer(gl.ARRAY_BUFFER, gl.createBuffer());
      gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([-1, -1, 3, -1, -1, 3]), gl.STATIC_DRAW);
      const loc = gl.getAttribLocation(p, 'a');
      gl.enableVertexAttribArray(loc);
      gl.vertexAttribPointer(loc, 2, gl.FLOAT, false, 0, 0);
      this.u = Object.fromEntries(['uTex', 'uRes', 'uAmt', 'uTime', 'uOpaque'].map((n) => [n, gl.getUniformLocation(p, n)]));
      this.tex = gl.createTexture();
      gl.bindTexture(gl.TEXTURE_2D, this.tex);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.LINEAR);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.LINEAR);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
      gl.uniform1i(this.u.uTex, 0);
    } catch (e) {
      console.warn('[plouk] warp renderer unavailable', e);
      this.gl = null;
    }
  }

  /** Snapshot of the window as a texture; false if this platform can't do it. */
  async snapshot() {
    if (!this.gl) return false;
    // Let a closing menu finish fading so it isn't frozen into the picture.
    for (let i = 0; i < 10 && document.querySelector('.dropdown.open'); i++) await wait(30);
    try {
      const buf = await invoke('capture_window');
      if (!(buf instanceof ArrayBuffer) || buf.byteLength < 12) return false;
      const head = new DataView(buf, 0, 12);
      const w = head.getUint32(0, true);
      const h = head.getUint32(4, true);
      const depth = head.getUint32(8, true);
      const gl = this.gl;
      gl.bindTexture(gl.TEXTURE_2D, this.tex);
      gl.pixelStorei(gl.UNPACK_PREMULTIPLY_ALPHA_WEBGL, false);
      gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, w, h, 0, gl.RGBA, gl.UNSIGNED_BYTE, new Uint8Array(buf, 12, w * h * 4));
      gl.uniform1f(this.u.uOpaque, depth === 32 ? 0 : 1);
      return true;
    } catch (e) {
      console.info('[plouk] no window snapshot, using the CSS warp:', e);
      return false;
    }
  }

  draw() {
    const gl = this.gl;
    const c = this.canvas;
    const dpr = devicePixelRatio || 1;
    const w = Math.round(c.clientWidth * dpr);
    const h = Math.round(c.clientHeight * dpr);
    if (c.width !== w || c.height !== h) {
      c.width = w;
      c.height = h;
    }
    gl.viewport(0, 0, w, h);
    gl.uniform2f(this.u.uRes, w, h);
    gl.uniform1f(this.u.uAmt, this.amt);
    gl.uniform1f(this.u.uTime, (performance.now() - this.t0) / 1000);
    gl.drawArrays(gl.TRIANGLES, 0, 3);
  }

  run() {
    let frame = 0;
    const loop = () => {
      if (!this.live) return;
      // Full rate while moving, half rate for the gentle drift while held open.
      if (this.moving || frame++ % 2 === 0) this.draw();
      requestAnimationFrame(loop);
    };
    requestAnimationFrame(loop);
  }

  async animateTo(target, ms, easing) {
    this.amtTween?.cancel();
    this.moving = true;
    this.amtTween = tween(this.amt, target, ms, easing, (v) => (this.amt = v));
    const done = await this.amtTween;
    if (done) this.moving = false;
    return done;
  }

  async open(card, reveal = []) {
    if (this.simple || reduced.matches) {
      card.animate([{ opacity: 0 }, { opacity: 1 }], { duration: 200, easing: 'ease-out', fill: 'forwards' });
      return;
    }
    this.mode = (await this.snapshot()) ? 'gpu' : 'css';
    if (this.mode === 'gpu') {
      this.amt = 0;
      this.live = true;
      this.draw();
      this.canvas.classList.add('on');
      this.ambient.paused = true;
      this.run();
      this.animateTo(1, 760, ease.inOutCubic);
    } else {
      this.cssWarp(true);
    }

    const emerge = card.animate(
      [
        { opacity: 0, transform: 'scale(.7)' },
        { opacity: 1, transform: 'scale(1.025)', offset: 0.65 },
        { opacity: 1, transform: 'scale(1)' },
      ],
      { duration: 720, delay: 240, easing: 'cubic-bezier(.2,.8,.2,1)', fill: 'forwards' }
    );
    reveal.forEach((el, i) =>
      el.animate([{ opacity: 0, transform: 'translateY(10px)', filter: 'blur(4px)' }, { opacity: 1, transform: 'none', filter: 'blur(0px)' }], {
        duration: 500,
        delay: 560 + i * 40,
        easing: 'cubic-bezier(.22,1,.36,1)',
        fill: 'backwards',
      })
    );
    this.emerge = emerge;
    await emerge.finished.catch(() => {});
  }

  async close(card) {
    if (this.simple) {
      return card.animate([{ opacity: 1 }, { opacity: 0 }], { duration: 180, easing: 'ease-in', fill: 'forwards' }).finished.catch(() => {});
    }
    this.emerge?.cancel();
    const away = card.animate(
      [
        { opacity: 1, transform: 'scale(1)' },
        { opacity: 0, transform: 'scale(.8)' },
      ],
      { duration: 260, easing: 'cubic-bezier(.5,0,.9,.5)', fill: 'forwards' }
    );
    if (reduced.matches) return away.finished.catch(() => {});
    if (this.mode === 'gpu') {
      const done = await this.animateTo(0, 620, ease.inOutCubic);
      if (!done) return;
      // Hand back to the live interface (it kept running underneath).
      this.ambient.paused = false;
      await this.canvas.animate([{ opacity: 1 }, { opacity: 0 }], { duration: 180, easing: 'ease-out' }).finished.catch(() => {});
      this.canvas.classList.remove('on');
      this.live = false;
    } else {
      await this.cssWarp(false);
    }
  }

  /** Fallback without a snapshot: each piece zooms out of the centre and blurs. */
  cssWarp(out) {
    const app = document.getElementById('app').getBoundingClientRect();
    const cx = app.left + app.width / 2;
    const cy = app.top + app.height / 2;
    const old = this.cssAnims;
    this.cssAnims = PIECES.map((sel) => document.querySelector(sel))
      .filter(Boolean)
      .map((el) => {
        const r = el.getBoundingClientRect();
        el.style.transformOrigin = `${cx - r.left}px ${cy - r.top}px`;
        const rest = { transform: 'scale(1) rotate(0deg)', filter: 'blur(0px) brightness(1)' };
        const warped = { transform: 'scale(1.14) rotate(1.5deg)', filter: 'blur(14px) brightness(.55)' };
        return el.animate(out ? [rest, warped] : [warped, rest], {
          duration: out ? 700 : 560,
          easing: 'cubic-bezier(.65,0,.35,1)',
          fill: 'forwards',
        });
      });
    old.forEach((a) => a.cancel());
    const all = Promise.all(this.cssAnims.map((a) => a.finished.catch(() => {})));
    if (!out) {
      all.then(() => {
        this.cssAnims.forEach((a) => a.cancel());
        this.cssAnims = [];
        PIECES.forEach((sel) => document.querySelector(sel)?.style.removeProperty('transform-origin'));
      });
    }
    return all;
  }
}
