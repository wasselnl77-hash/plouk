// The tracklist: renders a collection, cascades rows in with spring physics
// when a playlist lands, and keeps the playing row in view.

import { spring, fmtTime, clamp } from './motion.js';

const PLAY_ICON = '<svg class="play-ico" viewBox="0 0 24 24" width="14" height="14"><path d="M8 5.5v13a1 1 0 0 0 1.5.86l10.4-6.5a1 1 0 0 0 0-1.72L9.5 4.64A1 1 0 0 0 8 5.5z" fill="currentColor"/></svg>';

function totalTime(tracks) {
  const mins = Math.round(tracks.reduce((a, t) => a + t.durationMs, 0) / 60000);
  return mins >= 60 ? `${Math.floor(mins / 60)} h ${mins % 60} min` : `${mins} min`;
}

export class Queue {
  constructor(listEl, headEl, { onPick, onMove, onMenu }) {
    this.list = listEl;
    this.head = headEl;
    this.onPick = onPick;
    this.onMove = onMove; // (from, to): a row was dragged to a new spot
    this.onMenu = onMenu; // (index, x, y): right-click, or a press that stayed put on a touch screen
    this.current = -1;
    this.eq = [];
    this.press = null; // a pointer is down on a row
    this.drag = null; // a row is being carried
    this.noClickUntil = 0;
    this.lastType = 'mouse';
    this.lastDownAt = 0;
    listEl.addEventListener('click', (e) => {
      if (performance.now() < this.noClickUntil) return;
      const row = e.target.closest('.row');
      if (row) this.onPick(+row.dataset.i);
    });
    // Rows are moved with pointer events (HTML5 drag and drop does not work by touch in the
    // Android WebView). A mouse drags after a few pixels. A finger first has to rest for a
    // moment, so ordinary scrolling still works: lift without moving opens the menu, move
    // after resting to carry the row.
    listEl.addEventListener('pointerdown', (e) => this.down(e));
    listEl.addEventListener('pointermove', (e) => this.move(e));
    listEl.addEventListener('pointerup', (e) => this.up(e));
    listEl.addEventListener('pointercancel', () => this.cancel());
    // Claim the finger as soon as the hold registers: once a scroll starts it can't be taken back.
    listEl.addEventListener('touchmove', (e) => (this.drag || this.press?.armed) && e.cancelable && e.preventDefault(), { passive: false });
    listEl.addEventListener('contextmenu', (e) => {
      e.preventDefault();
      const row = e.target.closest('.row');
      const touch = e.pointerType === 'touch' || (this.lastType !== 'mouse' && performance.now() - this.lastDownAt < 2000);
      if (row && !touch && !this.drag) this.onMenu(+row.dataset.i, e.clientX, e.clientY);
    });
    window.addEventListener('keydown', (e) => e.key === 'Escape' && this.drag && this.cancel());
  }

  down(e) {
    if (this.press && !this.press.row.isConnected) this.cancel(); // its row was redrawn mid-press
    if (this.press || (e.pointerType === 'mouse' && e.button !== 0)) return;
    const row = e.target.closest('.row');
    if (!row) return;
    this.lastType = e.pointerType;
    this.lastDownAt = performance.now();
    const p = (this.press = { id: e.pointerId, row, x: e.clientX, y: e.clientY, type: e.pointerType, armed: e.pointerType === 'mouse', timer: 0 });
    if (!p.armed) {
      p.timer = setTimeout(() => {
        p.armed = true;
        row.classList.add('held');
        navigator.vibrate?.(10);
      }, 420);
    }
  }

  move(e) {
    const p = this.press;
    if (!p || e.pointerId !== p.id) return;
    if (!this.drag) {
      const dist = Math.hypot(e.clientX - p.x, e.clientY - p.y);
      if (!p.armed) {
        if (dist > 8) this.release(); // the finger is scrolling the list
        return;
      }
      if (dist < 5) return;
      this.startDrag();
    }
    this.drag.y = e.clientY;
    if (!this.drag.raf) this.drag.raf = requestAnimationFrame(() => this.tick());
  }

  up(e) {
    const p = this.press;
    if (!p || e.pointerId !== p.id) return;
    if (this.drag) {
      const { from, to } = this.drag;
      this.endDrag();
      if (to !== from) this.onMove(from, to);
      return;
    }
    const heldStill = p.type !== 'mouse' && p.armed;
    const index = +p.row.dataset.i;
    this.release();
    if (heldStill) {
      this.noClickUntil = performance.now() + 500; // the tap that follows a long press is not a pick
      this.onMenu(index, e.clientX, e.clientY);
    }
  }

  cancel() {
    if (this.drag) this.endDrag();
    else this.release();
  }

  release() {
    const p = this.press;
    if (!p) return;
    clearTimeout(p.timer);
    p.row.classList.remove('held');
    this.press = null;
  }

  startDrag() {
    const p = this.press;
    clearTimeout(p.timer);
    const rows = this.list.querySelectorAll('.row');
    const stride = rows.length > 1 ? rows[1].offsetTop - rows[0].offsetTop : 54;
    const line = document.createElement('div');
    line.className = 'drop-line';
    this.list.append(line);
    this.drag = {
      from: +p.row.dataset.i,
      to: +p.row.dataset.i,
      row: p.row,
      line,
      n: rows.length,
      top0: rows[0].offsetTop,
      stride,
      rowTop: p.row.offsetTop,
      startY: p.y,
      startScroll: this.list.scrollTop,
      y: p.y,
      raf: 0,
    };
    try {
      this.list.setPointerCapture(p.id);
    } catch {}
    p.row.classList.remove('held');
    p.row.classList.add('dragging');
    this.list.classList.add('reordering');
  }

  tick() {
    const d = this.drag;
    if (!d) return;
    d.raf = 0;
    const r = this.list.getBoundingClientRect();
    const edge = 48;
    let v = 0;
    if (d.y < r.top + edge) v = -Math.min(1, (r.top + edge - d.y) / edge) * 14;
    else if (d.y > r.bottom - edge) v = Math.min(1, (d.y - (r.bottom - edge)) / edge) * 14;
    if (v) this.list.scrollTop += v;
    const dy = d.y - d.startY + (this.list.scrollTop - d.startScroll);
    d.row.style.transform = `translateY(${dy}px)`;
    const centre = d.rowTop + dy + d.row.offsetHeight / 2;
    const to = clamp(Math.floor((centre - d.top0) / d.stride), 0, d.n - 1);
    if (to !== d.to || !d.line.style.transform) {
      d.to = to;
      d.line.style.visibility = to === d.from ? 'hidden' : 'visible';
      d.line.style.transform = `translateY(${d.top0 + (to > d.from ? to + 1 : to) * d.stride - 1}px)`;
    }
    if (v) d.raf = requestAnimationFrame(() => this.tick());
  }

  endDrag() {
    const d = this.drag;
    cancelAnimationFrame(d.raf);
    d.line.remove();
    d.row.classList.remove('dragging');
    d.row.style.transform = '';
    this.list.classList.remove('reordering');
    try {
      this.list.releasePointerCapture(this.press.id);
    } catch {}
    this.noClickUntil = performance.now() + 400;
    this.drag = null;
    this.release();
  }

  renderHead(col) {
    if (!col) {
      this.head.innerHTML = '';
      return;
    }
    this.head.innerHTML = `
      <div class="qh-cover" style="background-image:url('${col.cover || ''}')"></div>
      <div class="qh-text">
        <div class="qh-kind">${{ playlist: 'Playlist', album: 'Album', tracks: 'Songs' }[col.kind] || 'Playlist'}</div>
        <div class="qh-name"></div>
        <div class="qh-meta">${col.tracks.length} songs · ${totalTime(col.tracks)}</div>
      </div>`;
    this.head.querySelector('.qh-name').textContent = col.name;
  }

  render(col, current, { cascade = false } = {}) {
    this.renderHead(col);
    this.list.innerHTML = '';
    this.current = -1;
    if (!col) {
      this.list.innerHTML = `
        <div class="queue-empty">
          <div class="qe-stack"><i></i><i></i><i></i></div>
          <div class="qe-title">Nothing queued yet</div>
          <div class="qe-sub">Drop a playlist, album or a few songs from Spotify anywhere in this window.</div>
        </div>`;
      return;
    }
    this.fill(col);
    this.list.scrollTop = 0;
    this.setCurrent(current, false);
    if (cascade) this.cascade();
    else this.list.animate([{ opacity: 0 }, { opacity: 1 }], { duration: 500, easing: 'ease-out' });
  }

  fill(col) {
    const frag = document.createDocumentFragment();
    if (!col.tracks.length) frag.append(Object.assign(document.createElement('div'), { className: 'queue-none', textContent: 'No songs left in this playlist' }));
    col.tracks.forEach((t, i) => {
      const row = document.createElement('div');
      row.className = 'row';
      row.dataset.i = i;
      row.innerHTML = `
        <div class="idx"><span class="num">${i + 1}</span><span class="eq"><i></i><i></i><i></i></span>${PLAY_ICON}</div>
        <div class="txt"><div class="t"></div><div class="a"></div></div>
        <div class="dur">${fmtTime(t.durationMs / 1000)}</div>`;
      row.querySelector('.t').textContent = t.title;
      const a = row.querySelector('.a');
      if (t.explicit) a.insertAdjacentHTML('afterbegin', '<span class="e">E</span>');
      a.append(t.artist);
      frag.append(row);
    });
    this.list.append(frag);
  }

  /** The list changed (moved, removed, restored): redraw the rows in place, keeping the scroll
   *  position and no entrance animation. */
  sync(col, current) {
    const top = this.list.scrollTop;
    const loading = !!this.list.querySelector('.row.loading');
    this.renderHead(col);
    this.list.innerHTML = '';
    this.current = -1;
    this.fill(col);
    this.list.scrollTop = top;
    this.setCurrent(current, false);
    if (loading) this.setLoading(current, true);
  }

  cascade() {
    const rows = [...this.list.children];
    const { easing, duration } = spring({ stiffness: 150, damping: 15 });
    const count = Math.min(rows.length, 18);
    rows.forEach((row, i) => {
      if (i < count) {
        row.animate(
          [
            { opacity: 0, transform: 'translateY(-38px) rotateX(-62deg) scale(.95)', filter: 'blur(7px)' },
            { opacity: 1, transform: 'none', filter: 'blur(0px)' },
          ],
          { duration, delay: 120 + i * 38, easing, fill: 'backwards' }
        );
      } else {
        row.animate([{ opacity: 0 }, { opacity: 1 }], { duration: 400, delay: 120 + count * 38, fill: 'backwards' });
      }
    });
    this.head.animate(
      [
        { opacity: 0, transform: 'translateY(-10px)', filter: 'blur(6px)' },
        { opacity: 1, transform: 'none', filter: 'blur(0px)' },
      ],
      { duration: 700, easing: 'cubic-bezier(.22,1,.36,1)' }
    );
  }

  setCurrent(i, scroll = true) {
    const rows = this.list.children;
    rows[this.current]?.classList.remove('current');
    this.current = i;
    const row = rows[i];
    if (!row || !row.classList.contains('row')) {
      this.eq = [];
      return;
    }
    row.classList.add('current');
    this.eq = [...row.querySelectorAll('.eq i')];
    if (scroll) {
      const top = row.offsetTop - this.list.clientHeight * 0.3;
      const visible = row.offsetTop > this.list.scrollTop + 20 && row.offsetTop + row.offsetHeight < this.list.scrollTop + this.list.clientHeight - 30;
      if (!visible) this.list.scrollTo({ top, behavior: 'smooth' });
    }
  }

  setLoading(i, on) {
    this.list.children[i]?.classList.toggle('loading', on);
  }

  /** Little equalizer bars on the playing row. */
  pulse(level, playing) {
    if (!this.eq.length) return;
    const t = performance.now() / 1000;
    this.eq.forEach((bar, k) => {
      const wobble = 0.5 + 0.5 * Math.sin(t * (7 + k * 2.3) + k * 1.7);
      const h = playing ? 18 + Math.min(1, level) * 62 * (0.55 + wobble * 0.45) + wobble * 20 : 18;
      const tf = `scaleY(${(h / 100).toFixed(2)})`;
      if (bar.style.transform !== tf) bar.style.transform = tf;
    });
  }
}
