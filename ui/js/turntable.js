// The turntable: record spin physics, a tonearm whose angle *is* the song
// progress (drag it to seek), and the record-swap choreography.
// Geometry lives in the tonearm SVG's 1000×1000 viewBox.

import { tween, ease, clamp } from './motion.js';

const PIVOT = { x: 960, y: 110 };
const CENTER = { x: 500, y: 500 };
// Stylus tip in arm-local coordinates: end of the tube (0, 640) plus the
// headshell (rotated 22°) reaching 128 units further.
const STYLUS = { x: -128 * Math.sin((22 * Math.PI) / 180), y: 640 + 128 * Math.cos((22 * Math.PI) / 180) };
const R_OUTER = 456;
const R_INNER = 194;
const RPM_DEG = 200; // 33⅓ rpm in degrees per second
const D = Math.PI / 180;

function stylusAt(theta) {
  const c = Math.cos(theta * D), s = Math.sin(theta * D);
  return { x: PIVOT.x + STYLUS.x * c - STYLUS.y * s, y: PIVOT.y + STYLUS.x * s + STYLUS.y * c };
}
function solveAngle(r) {
  let lo = 0, hi = 45;
  for (let i = 0; i < 40; i++) {
    const mid = (lo + hi) / 2;
    const p = stylusAt(mid);
    if (Math.hypot(p.x - CENTER.x, p.y - CENTER.y) > r) lo = mid;
    else hi = mid;
  }
  return (lo + hi) / 2;
}
const A_START = solveAngle(R_OUTER);
const A_END = solveAngle(R_INNER);
const A_REST = -3;
const STYLUS_BASE = Math.atan2(STYLUS.y, STYLUS.x) / D;

export class Turntable {
  constructor(el) {
    this.el = el;
    this.slot = el.querySelector('.record-slot');
    this.record = el.querySelector('.record');
    this.label = el.querySelector('.label');
    this.sleeve = el.querySelector('.sleeve');
    this.sleeveArt = el.querySelector('.sleeve-art');
    this.svg = el.querySelector('svg.tonearm');
    this.armShadow = el.querySelector('#armShadow');
    this.arm = el.querySelector('#arm');
    this.armHit = el.querySelector('.arm-hit');
    this.bubble = el.querySelector('.arm-bubble');

    this.angle = Math.random() * 360;
    this.omega = 0;
    this.playing = false;
    this.theta = A_REST;
    this.lift = 0;
    this.mode = 'rest'; // rest | track | drag | anim
    this.progress = 0;
    this.hasRecord = false;
    this.scratch = 0;
    this.duration = 200;
    this.seq = 0;
    this.swapSeq = 0;
    this.onScrub = null;
    this.formatTime = (p) => '';

    const rest = stylusAt(A_REST);
    const post = el.querySelector('#armRest');
    post.setAttribute('cx', rest.x.toFixed(1));
    post.setAttribute('cy', rest.y.toFixed(1));

    this.renderArm(true);
    this.bindScrub();
    this.loop = this.loop.bind(this);
    requestAnimationFrame(this.loop);
  }

  angleFor(p) {
    return A_START + (A_END - A_START) * clamp(p, 0, 1);
  }

  loop(now) {
    requestAnimationFrame(this.loop);
    const dt = Math.min(0.05, (now - (this.last ?? now)) / 1000);
    this.last = now;

    const target = this.playing ? RPM_DEG : 0;
    const k = target > this.omega ? 2.6 : 1.7;
    this.omega += (target - this.omega) * (1 - Math.exp(-dt * k));
    if (!this.playing && this.omega < 0.3) this.omega = 0;
    if (this.omega || this.scratch) {
      this.angle = (this.angle + this.omega * dt + this.scratch) % 360;
      this.scratch = 0;
      this.record.style.transform = `rotate(${this.angle.toFixed(2)}deg)`;
    }

    if (this.mode === 'track') {
      const t = this.angleFor(this.progress);
      this.theta += (t - this.theta) * (1 - Math.exp(-dt * 9));
    }
    this.renderArm();
  }

  renderArm(force = false) {
    if (!force && this.drawnTheta === this.theta && this.drawnLift === this.lift) return;
    this.drawnTheta = this.theta;
    this.drawnLift = this.lift;
    const s = 1 + this.lift * 0.022;
    const tf = `translate(${PIVOT.x} ${PIVOT.y}) rotate(${this.theta.toFixed(3)}) scale(${s.toFixed(4)})`;
    this.arm.setAttribute('transform', tf);
    // The shadow falls further away as the arm lifts off the record.
    const ox = 10 + this.lift * 26, oy = 16 + this.lift * 34;
    this.armShadow.setAttribute('transform', `translate(${ox.toFixed(1)} ${oy.toFixed(1)}) ${tf}`);
    this.armShadow.style.opacity = (0.55 - this.lift * 0.25).toFixed(3);
  }

  cancelArm() {
    this.armTween?.cancel();
    this.liftTween?.cancel();
  }

  armTo(theta, ms, easing = ease.inOutCubic) {
    this.armTween?.cancel();
    this.mode = 'anim';
    this.armTween = tween(this.theta, theta, ms, easing, (v) => (this.theta = v));
    return this.armTween;
  }

  liftTo(v, ms) {
    this.liftTween?.cancel();
    this.liftTween = tween(this.lift, v, ms, ease.outCubic, (x) => (this.lift = x));
    return this.liftTween;
  }

  setArt(url) {
    this.label.style.backgroundImage = url ? `url("${url}")` : '';
  }

  setPlaying(on) {
    this.playing = on;
    this.el.classList.toggle('spinning', on);
  }

  setProgress(p) {
    if (this.mode !== 'drag') this.progress = clamp(p, 0, 1);
  }

  setLoading(on) {
    this.el.classList.toggle('loading', on);
  }

  /** Lift, swing over the groove for progress p, and drop the needle. Resolves when it lands. */
  async needleDown(p) {
    const token = ++this.seq;
    const target = this.angleFor(p);
    if (Math.abs(this.theta - target) > 0.4 || this.mode === 'rest') {
      if (this.lift < 0.95) await this.liftTo(1, 170);
      if (token !== this.seq) return false;
      const dist = Math.abs(this.theta - target);
      await this.armTo(target, 380 + dist * 14);
      if (token !== this.seq) return false;
    }
    this.progress = p;
    this.mode = 'track';
    await this.liftTo(0, 260);
    if (token !== this.seq) return false;
    this.el.classList.add('needle-on');
    return true;
  }

  /** Paused: the arm hovers just above the groove. */
  hover() {
    ++this.seq;
    this.el.classList.remove('needle-on');
    if (this.mode !== 'rest') this.mode = 'track';
    return this.liftTo(0.5, 240);
  }

  lower() {
    ++this.seq;
    this.mode = 'track';
    this.el.classList.add('needle-on');
    return this.liftTo(0, 220);
  }

  async toRest() {
    const token = ++this.seq;
    this.el.classList.remove('needle-on');
    if (this.mode === 'rest' && Math.abs(this.theta - A_REST) < 0.2) return true;
    await this.liftTo(1, 170);
    if (token !== this.seq) return false;
    await this.armTo(A_REST, 560);
    if (token !== this.seq) return false;
    this.mode = 'rest';
    await this.liftTo(0, 240);
    return token === this.seq;
  }

  /** Restores a session: record on the platter, arm hovering over where you left off. */
  placeRecord(art, p) {
    this.setArt(art);
    this.hasRecord = true;
    this.el.classList.add('has-record');
    this.progress = clamp(p, 0, 1);
    this.theta = this.angleFor(this.progress);
    this.lift = 0.5;
    this.mode = 'track';
    this.slot.animate([{ opacity: 0, transform: 'scale(.94)' }, { opacity: 1, transform: 'none' }], {
      duration: 900,
      easing: 'cubic-bezier(.22,1,.36,1)',
    });
  }

  clear() {
    this.hasRecord = false;
    this.el.classList.remove('has-record', 'needle-on');
    this.setPlaying(false);
    this.toRest();
  }

  /** Old record lifts away, the new sleeve rises, the record slides out onto the platter. */
  async swap(art) {
    const token = ++this.swapSeq;
    const hadRecord = this.hasRecord;
    const armAway = this.mode === 'rest' ? Promise.resolve() : this.toRest();

    if (document.hidden) {
      this.sleeveArt.style.backgroundImage = art ? `url("${art}")` : '';
      this.setArt(art);
      this.slot.style.opacity = '';
      this.hasRecord = true;
      this.el.classList.add('has-record');
      await armAway;
      return token === this.swapSeq;
    }

    if (hadRecord) {
      const ghost = this.slot.cloneNode(true);
      ghost.classList.add('ghost');
      ghost.querySelectorAll('[id]').forEach((n) => n.removeAttribute('id'));
      this.slot.after(ghost);
      ghost
        .animate(
          [
            { transform: 'translate(0,0) scale(1)', opacity: 1, filter: 'brightness(1)' },
            { transform: 'translate(0,-2.5%) scale(1.04)', opacity: 1, filter: 'brightness(1.1)', offset: 0.3 },
            { transform: 'translate(10%, 26%) scale(.9)', opacity: 0, filter: 'brightness(.5)' },
          ],
          { duration: 760, delay: 140, easing: 'cubic-bezier(.55,0,.25,1)', fill: 'forwards' }
        )
        .finished.then(() => ghost.remove());
      this.slot.style.opacity = '0';
    }

    this.sleeveArt.style.backgroundImage = art ? `url("${art}")` : '';
    this.setArt(art);
    const held = { transform: 'translate(-44%, 3%) rotate(-5deg) scale(.9)', opacity: 1 };
    const sleeveIn = this.sleeve.animate(
      [{ transform: 'translate(-70%, 46%) rotate(-16deg) scale(.82)', opacity: 0 }, held],
      { duration: 640, delay: hadRecord ? 420 : 60, easing: 'cubic-bezier(.22,1,.36,1)', fill: 'forwards' }
    );
    await sleeveIn.finished.catch(() => {});
    if (token !== this.swapSeq) return false;

    this.slot.style.opacity = '';
    this.hasRecord = true;
    this.el.classList.add('has-record');
    const recordIn = this.slot.animate(
      [
        { transform: 'translate(-44%, 3%) scale(.88)', opacity: 0 },
        { transform: 'translate(-34%, 2.5%) scale(.9)', opacity: 1, offset: 0.16 },
        { transform: 'translate(1%, -1.8%) scale(1.04)', opacity: 1, offset: 0.74 },
        { transform: 'translate(0,0) scale(1)', opacity: 1 },
      ],
      { duration: 900, easing: 'cubic-bezier(.32,.72,.2,1)' }
    );
    this.sleeve.animate([held, { transform: 'translate(-58%, 44%) rotate(-12deg) scale(.84)', opacity: 0 }], {
      duration: 640,
      delay: 420,
      easing: 'cubic-bezier(.55,0,.8,.2)',
      fill: 'forwards',
    });
    await recordIn.finished.catch(() => {});
    await armAway;
    return token === this.swapSeq;
  }

  bindScrub() {
    const toSvg = (e) => {
      const r = this.svg.getBoundingClientRect();
      return { x: ((e.clientX - r.left) / r.width) * 1000, y: ((e.clientY - r.top) / r.height) * 1000 };
    };
    this.armHit.addEventListener('pointerdown', (e) => {
      if (!this.hasRecord || !this.onScrub || this.mode === 'rest' || this.mode === 'anim') return;
      e.preventDefault();
      this.armHit.setPointerCapture(e.pointerId);
      ++this.seq;
      this.cancelArm();
      this.mode = 'drag';
      this.liftTo(0.6, 140);
      this.el.classList.add('scrubbing');
      const move = (ev) => {
        const p = toSvg(ev);
        const th = clamp(Math.atan2(p.y - PIVOT.y, p.x - PIVOT.x) / D - STYLUS_BASE, A_START, A_END);
        const prog = (th - A_START) / (A_END - A_START);
        this.scratch += clamp((prog - this.progress) * this.duration * RPM_DEG * 0.35, -60, 60);
        this.theta = th;
        this.progress = prog;
        this.showBubble(prog);
        this.onScrub(prog, false);
      };
      const up = () => {
        this.armHit.removeEventListener('pointermove', move);
        this.armHit.removeEventListener('pointerup', up);
        this.armHit.removeEventListener('pointercancel', up);
        this.el.classList.remove('scrubbing');
        this.mode = 'track';
        this.liftTo(this.playing ? 0 : 0.5, 220);
        this.onScrub(this.progress, true);
      };
      this.armHit.addEventListener('pointermove', move);
      this.armHit.addEventListener('pointerup', up);
      this.armHit.addEventListener('pointercancel', up);
      this.showBubble(this.progress);
    });
  }

  showBubble(prog) {
    const tip = stylusAt(this.theta);
    this.bubble.style.left = `${tip.x / 10}%`;
    this.bubble.style.top = `${tip.y / 10}%`;
    this.bubble.textContent = this.formatTime(prog);
  }
}
