// The tracklist: renders a collection, cascades rows in with spring physics
// when a playlist lands, and keeps the playing row in view.

import { spring, fmtTime } from './motion.js';

const PLAY_ICON = '<svg class="play-ico" viewBox="0 0 24 24" width="14" height="14"><path d="M8 5.5v13a1 1 0 0 0 1.5.86l10.4-6.5a1 1 0 0 0 0-1.72L9.5 4.64A1 1 0 0 0 8 5.5z" fill="currentColor"/></svg>';

function totalTime(tracks) {
  const mins = Math.round(tracks.reduce((a, t) => a + t.durationMs, 0) / 60000);
  return mins >= 60 ? `${Math.floor(mins / 60)} h ${mins % 60} min` : `${mins} min`;
}

export class Queue {
  constructor(listEl, headEl, { onPick }) {
    this.list = listEl;
    this.head = headEl;
    this.onPick = onPick;
    this.current = -1;
    this.eq = [];
    listEl.addEventListener('click', (e) => {
      const row = e.target.closest('.row');
      if (row) this.onPick(+row.dataset.i);
    });
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
    const frag = document.createDocumentFragment();
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
    this.list.scrollTop = 0;
    this.setCurrent(current, false);
    if (cascade) this.cascade();
    else this.list.animate([{ opacity: 0 }, { opacity: 1 }], { duration: 500, easing: 'ease-out' });
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
      bar.style.height = `${h.toFixed(0)}%`;
    });
  }
}
