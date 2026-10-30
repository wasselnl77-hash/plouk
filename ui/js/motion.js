// Small motion toolkit: easings, cancellable tweens, and spring curves.

export const ease = {
  linear: (t) => t,
  outCubic: (t) => 1 - Math.pow(1 - t, 3),
  outQuint: (t) => 1 - Math.pow(1 - t, 5),
  inOutCubic: (t) => (t < 0.5 ? 4 * t * t * t : 1 - Math.pow(-2 * t + 2, 3) / 2),
  inOutSine: (t) => -(Math.cos(Math.PI * t) - 1) / 2,
};

export const clamp = (v, a, b) => Math.min(b, Math.max(a, v));

/** Animates a number; returns a promise (resolves true when finished, false if cancelled) with .cancel(). */
export function tween(from, to, ms, easing, onUpdate) {
  let raf = 0;
  let done;
  const promise = new Promise((resolve) => (done = resolve));
  // Hidden windows get no animation frames; jump to the end so playback never waits on visuals.
  if (document.hidden) {
    onUpdate(to);
    done(true);
    promise.cancel = () => {};
    return promise;
  }
  const t0 = performance.now();
  const step = (now) => {
    const t = ms <= 0 ? 1 : Math.min(1, (now - t0) / ms);
    onUpdate(from + (to - from) * easing(t));
    if (t < 1) raf = requestAnimationFrame(step);
    else done(true);
  };
  raf = requestAnimationFrame(step);
  promise.cancel = () => {
    cancelAnimationFrame(raf);
    done(false);
  };
  return promise;
}

/** Samples a damped spring from 0 to 1 (may overshoot). */
export function springSamples({ stiffness = 170, damping = 17, mass = 1 } = {}) {
  const out = [0];
  let x = 0;
  let v = 0;
  const dt = 1 / 120;
  for (let i = 0; i < 1200; i++) {
    const f = -stiffness * (x - 1) - damping * v;
    v += (f / mass) * dt;
    x += v * dt;
    if (i % 2 === 1) out.push(x);
    if (i > 30 && Math.abs(x - 1) < 0.0008 && Math.abs(v) < 0.002) break;
  }
  out.push(1);
  return out;
}

const linearSupported =
  typeof CSS !== 'undefined' && CSS.supports && CSS.supports('transition-timing-function', 'linear(0, 0.5, 1)');

/** A CSS easing that feels like a spring. Returns { easing, duration } for WAAPI. */
export function spring(opts) {
  const s = springSamples(opts);
  const duration = Math.round((s.length / 60) * 1000);
  if (!linearSupported) return { easing: 'cubic-bezier(.34,1.45,.64,1)', duration };
  const step = Math.max(1, Math.floor(s.length / 48));
  const pts = s.filter((_, i) => i % step === 0 || i === s.length - 1).map((v) => +v.toFixed(4));
  return { easing: `linear(${pts.join(', ')})`, duration };
}

export const fmtTime = (secs) => {
  if (!isFinite(secs) || secs < 0) secs = 0;
  const m = Math.floor(secs / 60);
  const s = Math.floor(secs % 60);
  return `${m}:${s.toString().padStart(2, '0')}`;
};
