// The options panel. It comes through a warp of the interface (see warp.js)
// and talks to main.js through a small context object.

import { segmented } from './menubar.js';

const TITLES = { playback: 'Playback', behavior: 'Behavior', storage: 'Storage & engine', about: 'About' };

export const DEFAULT_SETTINGS = {
  normalize: true,
  autoplay: false,
  launchVolume: 'last',
  closeAction: 'tray',
  startHidden: false,
  warp: true,
  autoUpdate: true,
  cacheLimitGB: 5,
};

function fmtBytes(n) {
  if (n >= 1 << 30) return `${(n / (1 << 30)).toFixed(1)} GB`;
  if (n >= 1 << 20) return `${Math.round(n / (1 << 20))} MB`;
  return `${Math.round(n / 1024)} KB`;
}

/** A button that needs a second click within 3 s before it does anything. */
function confirmButton(btn, label, run) {
  let timer = 0;
  btn.addEventListener('click', async () => {
    if (!btn.classList.contains('confirm')) {
      btn.classList.add('confirm');
      btn.textContent = 'Click again';
      timer = setTimeout(() => {
        btn.classList.remove('confirm');
        btn.textContent = label;
      }, 3000);
      return;
    }
    clearTimeout(timer);
    btn.classList.remove('confirm');
    btn.disabled = true;
    try {
      await run();
    } finally {
      btn.disabled = false;
      btn.textContent = label;
    }
  });
}

function toggle(btn, on, onChange) {
  btn.setAttribute('role', 'switch');
  const set = (v) => {
    btn.classList.toggle('on', v);
    btn.setAttribute('aria-checked', String(v));
  };
  set(on);
  btn.addEventListener('click', () => {
    const v = !btn.classList.contains('on');
    set(v);
    onChange(v);
  });
  return { set };
}

export class Options {
  constructor(root, ctx) {
    this.root = root;
    this.ctx = ctx;
    this.card = root.querySelector('.opt-card');
    this.section = 'playback';
    this.isOpen = false;
    this.busy = false;
    const $ = (id) => root.querySelector(`#${id}`);
    this.$ = $;

    root.querySelectorAll('.opt-tab').forEach((t) => t.addEventListener('click', () => this.show(t.dataset.sec)));
    $('optClose').addEventListener('click', () => this.close());
    root.addEventListener('pointerdown', (e) => e.target === root && this.close());
    window.addEventListener('keydown', (e) => {
      if (this.isOpen && e.key === 'Escape') {
        e.preventDefault();
        e.stopImmediatePropagation();
        this.close();
      }
    }, true);

    const S = ctx.S;
    // Playback
    const xf = $('oXf');
    const renderXf = () => {
      xf.value = S.crossfade;
      xf.style.setProperty('--fill', `${(S.crossfade / 12) * 100}%`);
      $('oXfVal').textContent = S.crossfade ? `${S.crossfade} s` : 'Off';
    };
    xf.addEventListener('input', () => {
      ctx.setCrossfade(+xf.value);
      renderXf();
    });
    this.renderXf = renderXf;
    // On/off settings, re-read from S.settings every time the panel opens (see refresh).
    const flags = { oNorm: 'normalize', oAutoplay: 'autoplay', oAutoUpdate: 'autoUpdate', oWarp: 'warp' };
    this.flags = Object.entries(flags).map(([id, key]) => [key, toggle($(id), S.settings[key], (v) => ctx.setSetting(key, v))]);
    this.launchVol = segmented(
      $('oLaunchVol'),
      [
        { label: 'Last used', value: 'last' },
        { label: '30 %', value: 0.3 },
        { label: '50 %', value: 0.5 },
        { label: '80 %', value: 0.8 },
      ],
      S.settings.launchVolume,
      (v) => ctx.setSetting('launchVolume', v)
    );

    // Behavior
    this.closeSeg = segmented(
      $('oClose'),
      [
        { label: 'Hide to tray', value: 'tray' },
        { label: 'Quit Plouk', value: 'quit' },
      ],
      S.settings.closeAction,
      (v) => ctx.setSetting('closeAction', v)
    );
    this.autostartToggle = toggle($('oAutostart'), false, async (enabled) => {
      $('oHidden').disabled = !enabled;
      try {
        await ctx.invoke('set_autostart', { enabled });
      } catch (e) {
        ctx.toast(`Couldn't change start at login: ${e}`, 'error');
        this.autostartToggle.set(!enabled);
      }
    });
    this.hiddenToggle = toggle($('oHidden'), S.settings.startHidden, (v) => ctx.setSetting('startHidden', v));

    // Storage & engine
    this.cacheLimit = segmented(
      $('oCacheLimit'),
      [
        { label: '2 GB', value: 2 },
        { label: '5 GB', value: 5 },
        { label: '10 GB', value: 10 },
        { label: 'No limit', value: 0 },
      ],
      S.settings.cacheLimitGB,
      (v) => {
        ctx.setSetting('cacheLimitGB', v);
        setTimeout(() => this.refreshCache(), 300);
      }
    );
    $('oCacheOpen').addEventListener('click', () => ctx.invoke('open_cache_dir').catch((e) => ctx.toast(String(e), 'error')));
    confirmButton($('oCacheClear'), 'Clear cache', async () => {
      const freed = await ctx.clearCache();
      ctx.toast(`Cleared ${fmtBytes(freed)} of cached songs`, 'ok');
      this.refreshCache();
    });
    confirmButton($('oLibClear'), 'Clear library', async () => {
      ctx.clearLibrary();
      this.refreshLibrary();
      ctx.toast('Library cleared', 'ok');
    });
    $('oEngineUpdate').addEventListener('click', async () => {
      const btn = $('oEngineUpdate');
      btn.disabled = true;
      btn.classList.add('working');
      btn.textContent = 'Updating…';
      try {
        const version = await ctx.invoke('update_engine');
        $('oEngineDesc').textContent = `yt-dlp ${version} · up to date`;
        ctx.toast(`YouTube engine is up to date (${version})`, 'ok');
      } catch (e) {
        ctx.toast(`Update failed: ${e}`, 'error');
      } finally {
        btn.disabled = false;
        btn.classList.remove('working');
        btn.textContent = 'Update now';
      }
    });
  }

  async refreshCache() {
    try {
      const info = await this.ctx.invoke('cache_info');
      this.$('oCacheDesc').textContent = info.songs
        ? `${fmtBytes(info.bytes)} · ${info.songs} song${info.songs === 1 ? '' : 's'} saved for instant replay`
        : 'Empty. Songs are saved here after they first play.';
    } catch {}
  }

  refreshLibrary() {
    const n = this.ctx.S.library.length;
    const songs = this.ctx.S.library.reduce((a, c) => a + c.tracks.length, 0);
    this.$('oLibDesc').textContent = n ? `${n} playlist${n === 1 ? '' : 's'} · ${songs} songs on your shelf` : 'Your shelf is empty.';
  }

  async refresh() {
    const S = this.ctx.S;
    this.renderXf();
    this.flags.forEach(([key, t]) => t.set(!!S.settings[key]));
    this.launchVol.select(S.settings.launchVolume, true);
    this.closeSeg.select(S.settings.closeAction, true);
    this.cacheLimit.select(S.settings.cacheLimitGB, true);
    this.refreshCache();
    this.refreshLibrary();
    try {
      const a = await this.ctx.invoke('autostart_state');
      this.autostartToggle.set(!!a?.enabled);
      this.hiddenToggle.set(!!S.settings.startHidden);
      this.$('oHidden').disabled = !a?.enabled;
    } catch {}
    try {
      const info = await this.ctx.invoke('app_info');
      this.$('oVersion').textContent = `Version ${info.version}`;
      const engine = info.engine;
      this.$('oEngineDesc').textContent =
        engine?.state === 'ready'
          ? engine.version === 'NewPipe' ? 'NewPipe extractor, built in and updated with Plouk' : `yt-dlp ${engine.version}`
          : engine?.state === 'failed' ? engine.message : 'Setting up…';
    } catch {}
  }

  show(section, instant = false) {
    this.section = section;
    const tabs = [...this.root.querySelectorAll('.opt-tab')];
    const tab = tabs.find((t) => t.dataset.sec === section);
    tabs.forEach((t) => t.classList.toggle('on', t === tab));
    const pill = this.root.querySelector('.opt-nav-pill');
    if (instant) pill.style.transition = 'none';
    pill.style.transform = `translateY(${tab.offsetTop}px)`;
    if (instant) requestAnimationFrame(() => (pill.style.transition = ''));
    this.$('optTitle').textContent = TITLES[section];
    this.root.querySelectorAll('.opt-section').forEach((s) => s.classList.toggle('on', s.dataset.sec === section));
  }

  async open(section = this.section) {
    if (this.isOpen || this.busy) return;
    this.busy = true;
    this.isOpen = true;
    this.ctx.beforeOpen?.();
    this.root.classList.add('open');
    this.root.setAttribute('aria-hidden', 'false');
    this.show(section, true);
    this.refresh();
    const reveal = [
      ...this.root.querySelectorAll('.opt-nav .opt-reveal, .opt-head'),
      ...this.root.querySelectorAll(`.opt-section[data-sec="${section}"] .opt-row, .opt-section[data-sec="${section}"] .about > *`),
    ];
    await this.ctx.warp.open(this.card, reveal);
    this.busy = false;
  }

  async close() {
    if (!this.isOpen || this.busy) return;
    this.busy = true;
    this.isOpen = false;
    this.root.classList.remove('open');
    this.root.setAttribute('aria-hidden', 'true');
    await this.ctx.warp.close(this.card);
    this.busy = false;
  }

  toggle(section) {
    this.isOpen ? this.close() : this.open(section);
  }
}
