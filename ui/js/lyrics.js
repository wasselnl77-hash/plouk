// Synced lyrics: parses LRC, keeps the active line glowing at a fixed height,
// and lets you click a line to jump there.

export function parseLrc(text) {
  const out = [];
  for (const raw of text.split('\n')) {
    const stamps = [...raw.matchAll(/\[(\d+):(\d+(?:[.:]\d+)?)\]/g)];
    if (!stamps.length) continue;
    const words = raw.replace(/\[[^\]]*\]/g, '').trim();
    for (const m of stamps) out.push({ t: +m[1] * 60 + parseFloat(m[2].replace(':', '.')), text: words });
  }
  out.sort((a, b) => a.t - b.t);
  // Collapse runs of empty lines into one breathing "♪".
  return out.filter((l, i) => l.text || (out[i - 1] && out[i - 1].text));
}

export class Lyrics {
  constructor(viewEl, scrollEl, { onSeek }) {
    this.view = viewEl;
    this.scroll = scrollEl;
    this.onSeek = onSeek;
    this.lines = [];
    this.els = [];
    this.active = -1;
    this.manual = 0;
    this.manualTimer = 0;
    scrollEl.addEventListener('click', (e) => {
      const el = e.target.closest('.lyric');
      if (el && this.lines.length) this.onSeek(this.lines[+el.dataset.i].t);
    });
    viewEl.addEventListener(
      'wheel',
      (e) => {
        if (!this.lines.length) return;
        e.preventDefault();
        this.manual -= e.deltaY;
        clearTimeout(this.manualTimer);
        this.manualTimer = setTimeout(() => {
          this.manual = 0;
          this.position();
        }, 2600);
        this.position(true);
      },
      { passive: false }
    );
  }

  message(title, sub = '', cls = '') {
    this.lines = [];
    this.els = [];
    this.active = -1;
    this.scroll.style.transform = 'none';
    this.scroll.className = 'lyrics-scroll note-mode';
    this.scroll.innerHTML = `<div class="lyrics-note ${cls}"><div class="ln-title"></div><div class="ln-sub"></div></div>`;
    this.scroll.querySelector('.ln-title').textContent = title;
    this.scroll.querySelector('.ln-sub').textContent = sub;
  }

  loading() {
    this.message('Looking for lyrics…', '', 'shimmer');
  }

  empty() {
    this.message('');
  }

  set(result) {
    if (result?.synced) {
      this.lines = parseLrc(result.synced);
      this.scroll.className = 'lyrics-scroll';
      this.scroll.innerHTML = '';
      const frag = document.createDocumentFragment();
      this.els = this.lines.map((l, i) => {
        const el = document.createElement('div');
        el.className = 'lyric' + (l.text ? '' : ' gap');
        el.dataset.i = i;
        el.textContent = l.text || '♪';
        frag.append(el);
        return el;
      });
      this.scroll.append(frag);
      this.active = -1;
      this.manual = 0;
      this.scroll.style.transition = 'none';
      this.position();
      void this.scroll.offsetHeight;
      this.scroll.style.transition = '';
      return;
    }
    if (result?.plain) {
      this.lines = [];
      this.els = [];
      this.scroll.style.transform = 'none';
      this.scroll.className = 'lyrics-scroll plain';
      this.scroll.innerHTML = '<div class="plain-note">Not synced · scroll to read</div><div class="plain-text"></div>';
      this.scroll.querySelector('.plain-text').textContent = result.plain;
      return;
    }
    if (result?.instrumental) this.message('Instrumental', 'No words — just listen.');
    else this.message('No lyrics found', 'LRCLIB has nothing for this song yet.');
  }

  update(pos, jump = false) {
    if (!this.lines.length) return;
    const t = pos + 0.25;
    let lo = 0, hi = this.lines.length - 1, idx = -1;
    while (lo <= hi) {
      const mid = (lo + hi) >> 1;
      if (this.lines[mid].t <= t) {
        idx = mid;
        lo = mid + 1;
      } else hi = mid - 1;
    }
    if (idx === this.active && !jump) return;
    this.active = idx;
    this.els.forEach((el, i) => {
      el.classList.toggle('active', i === idx);
      el.classList.toggle('passed', i < idx);
    });
    if (jump) {
      this.manual = 0;
    }
    this.position();
  }

  position(instant = false) {
    const el = this.els[Math.max(0, this.active)];
    if (!el) return;
    const y = this.view.clientHeight * 0.36 - (el.offsetTop + el.offsetHeight / 2) + this.manual;
    this.scroll.style.transition = instant ? 'transform .15s linear' : '';
    this.scroll.style.transform = `translateY(${y.toFixed(1)}px)`;
  }
}
