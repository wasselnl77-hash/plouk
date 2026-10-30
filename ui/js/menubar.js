// Desktop-style menu bar: click a title to open, glide across titles to switch,
// arrow keys to navigate. One dropdown morphs between menus; a pill slides
// under the active title.

import { spring } from './motion.js';

const CHECK = '<svg viewBox="0 0 16 16" width="13" height="13"><path d="M3.5 8.5l3 3 6-7" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/></svg>';

const value = (v) => (typeof v === 'function' ? v() : v);

export function segmented(el, options, current, onPick) {
  el.classList.add('seg');
  el.innerHTML = '<span class="seg-pill"></span>';
  const pill = el.firstChild;
  const buttons = options.map((o) => {
    const b = document.createElement('button');
    b.type = 'button';
    b.textContent = o.label;
    b.addEventListener('click', (e) => {
      e.stopPropagation();
      select(o.value);
      onPick(o.value);
    });
    el.append(b);
    return b;
  });
  const select = (v, instant = false) => {
    const i = Math.max(0, options.findIndex((o) => o.value === v));
    buttons.forEach((b, k) => b.classList.toggle('on', k === i));
    const b = buttons[i];
    if (instant) pill.style.transition = 'none';
    pill.style.width = `${b.offsetWidth}px`;
    pill.style.transform = `translateX(${b.offsetLeft - 3}px)`;
    if (instant) requestAnimationFrame(() => (pill.style.transition = ''));
  };
  requestAnimationFrame(() => select(current, true));
  return { select };
}

export class MenuBar {
  constructor(bar, dropdown, menus) {
    this.bar = bar;
    this.drop = dropdown;
    this.menus = menus; // () => [{ id, items }]
    this.pill = bar.querySelector('.menu-pill');
    this.openId = null;
    this.hl = -1;
    this.titles = [...bar.querySelectorAll('.menu-title')];

    this.titles.forEach((t) => {
      t.addEventListener('click', (e) => {
        e.stopPropagation();
        this.openId === t.dataset.menu ? this.close() : this.open(t.dataset.menu);
      });
      t.addEventListener('pointerenter', () => {
        this.movePill(t);
        if (this.openId && this.openId !== t.dataset.menu) this.open(t.dataset.menu);
      });
    });
    bar.addEventListener('pointerleave', () => !this.openId && this.bar.classList.remove('hovering'));
    bar.addEventListener('pointerenter', () => this.bar.classList.add('hovering'));
    document.addEventListener('pointerdown', (e) => {
      if (this.openId && !e.target.closest('.dropdown') && !e.target.closest('.menu-title')) this.close();
    });
    window.addEventListener('keydown', (e) => this.onKey(e), true);
    window.addEventListener('blur', () => this.close());
  }

  get isOpen() {
    return !!this.openId;
  }

  movePill(title) {
    this.pill.style.width = `${title.offsetWidth}px`;
    this.pill.style.transform = `translateX(${title.offsetLeft}px)`;
  }

  open(id) {
    const title = this.titles.find((t) => t.dataset.menu === id);
    const menu = this.menus().find((m) => m.id === id);
    if (!title || !menu) return;
    const wasOpen = !!this.openId;
    const from = wasOpen ? this.drop.getBoundingClientRect() : null;

    this.titles.forEach((t) => t.classList.toggle('open', t === title));
    this.bar.classList.add('hovering', 'menu-open');
    this.movePill(title);
    this.openId = id;
    this.hl = -1;
    this.render(menu);

    const appLeft = this.bar.getBoundingClientRect().left;
    this.drop.style.left = `${title.getBoundingClientRect().left - appLeft}px`;
    this.drop.classList.add('open');
    const to = this.drop.getBoundingClientRect();

    if (wasOpen && from) {
      // Morph from the previous menu's box into this one.
      this.drop.animate(
        [
          { transform: `translate(${from.left - to.left}px, 0) scale(${from.width / to.width}, ${from.height / to.height})` },
          { transform: 'none' },
        ],
        { duration: 380, easing: 'cubic-bezier(.22,1,.36,1)' }
      );
      [...this.drop.children].forEach((c) => c.animate([{ opacity: 0 }, { opacity: 1 }], { duration: 220, delay: 60, fill: 'backwards' }));
    } else {
      const { easing, duration } = spring({ stiffness: 260, damping: 22 });
      this.drop.animate(
        [
          { opacity: 0, transform: 'translateY(-8px) scale(.94)', filter: 'blur(6px)' },
          { opacity: 1, transform: 'none', filter: 'blur(0px)' },
        ],
        { duration, easing }
      );
      [...this.drop.children].forEach((c, i) =>
        c.animate([{ opacity: 0, transform: 'translateY(-4px)' }, { opacity: 1, transform: 'none' }], {
          duration: 320,
          delay: 30 + i * 18,
          easing: 'cubic-bezier(.22,1,.36,1)',
          fill: 'backwards',
        })
      );
    }
  }

  close() {
    if (!this.openId) return;
    this.openId = null;
    this.titles.forEach((t) => t.classList.remove('open'));
    this.bar.classList.remove('menu-open');
    if (!this.bar.matches(':hover')) this.bar.classList.remove('hovering');
    const a = this.drop.animate(
      [
        { opacity: 1, transform: 'none' },
        { opacity: 0, transform: 'translateY(-4px) scale(.97)' },
      ],
      { duration: 160, easing: 'ease-in', fill: 'forwards' }
    );
    a.finished
      .then(() => {
        if (!this.openId) this.drop.classList.remove('open');
        a.cancel();
      })
      .catch(() => {});
  }

  render(menu) {
    this.drop.innerHTML = '';
    this.items = [];
    for (const it of menu.items) {
      if (it === '-') {
        this.drop.insertAdjacentHTML('beforeend', '<div class="msep"></div>');
        continue;
      }
      if (it.type === 'segment') {
        const wrap = document.createElement('div');
        wrap.className = 'mseg';
        wrap.innerHTML = `<div class="mseg-label"></div><div class="mseg-ctl"></div>`;
        wrap.querySelector('.mseg-label').textContent = it.label;
        this.drop.append(wrap);
        segmented(wrap.querySelector('.mseg-ctl'), it.options, value(it.value), it.onPick);
        continue;
      }
      const b = document.createElement('button');
      b.type = 'button';
      b.className = 'mi' + (value(it.disabled) ? ' disabled' : '') + (it.danger ? ' danger' : '');
      b.innerHTML = `<span class="check">${value(it.checked) ? CHECK : ''}</span><span class="lbl"></span>${it.kbd ? `<kbd>${it.kbd}</kbd>` : ''}`;
      b.querySelector('.lbl').textContent = value(it.label);
      b.addEventListener('click', () => this.activate(it));
      b.addEventListener('pointermove', () => this.highlight(this.items.indexOf(entry)));
      const entry = { it, el: b };
      this.items.push(entry);
      this.drop.append(b);
    }
  }

  activate(it) {
    if (value(it.disabled)) return;
    this.close();
    // Let the menu start closing before heavy actions (like the options warp) kick in.
    requestAnimationFrame(() => it.action());
  }

  highlight(i) {
    this.hl = i;
    this.items.forEach((e, k) => e.el.classList.toggle('hl', k === i));
  }

  onKey(e) {
    if (!this.openId) {
      // F10 opens the menu bar, as in most Linux apps.
      if (e.key === 'F10' && !e.target.closest?.('input') && !document.querySelector('.options.open')) {
        e.preventDefault();
        this.open(this.titles[0].dataset.menu);
        this.highlight(0);
      }
      return;
    }
    const ids = this.titles.map((t) => t.dataset.menu);
    const enabled = this.items.map((e, i) => (value(e.it.disabled) ? -1 : i)).filter((i) => i >= 0);
    const step = (dir) => {
      if (!enabled.length) return;
      const at = enabled.indexOf(this.hl);
      this.highlight(enabled[(at + dir + enabled.length) % enabled.length] ?? enabled[0]);
    };
    const handled = {
      Escape: () => this.close(),
      ArrowDown: () => step(1),
      ArrowUp: () => step(this.hl < 0 ? 0 : -1),
      ArrowRight: () => this.open(ids[(ids.indexOf(this.openId) + 1) % ids.length]),
      ArrowLeft: () => this.open(ids[(ids.indexOf(this.openId) - 1 + ids.length) % ids.length]),
      Enter: () => this.items[this.hl] && this.activate(this.items[this.hl].it),
      ' ': () => this.items[this.hl] && this.activate(this.items[this.hl].it),
    }[e.key];
    if (handled) {
      e.preventDefault();
      e.stopImmediatePropagation();
      handled();
    }
  }
}
