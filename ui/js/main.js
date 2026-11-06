import { invoke, listen, appWindow, isTauri } from './api.js';
import { Ambient } from './ambient.js';
import { extractPalette, DEFAULT_PALETTE } from './palette.js';
import { Turntable } from './turntable.js';
import { Queue } from './queue.js';
import { Lyrics } from './lyrics.js';
import { spring, fmtTime, clamp } from './motion.js';
import { MenuBar } from './menubar.js';
import { Options, DEFAULT_SETTINGS } from './options.js';
import { Warp } from './warp.js';

const $ = (id) => document.getElementById(id);
const app = $('app');

// Platform: set once at boot (desktop Linux in the browser design preview).
const PLAT = { os: 'linux', mobile: false };
const isMac = () => PLAT.os === 'macos';
const MOD = () => (isMac() ? '⌘' : 'Ctrl ');

// ------------------------------------------------------------------ state

const S = {
  library: [],
  currentId: null,
  index: -1,
  position: 0,
  volume: 0.8,
  muted: false,
  crossfade: 6,
  loop: 'off', // 'off' | 'all' (the playlist) | 'one' (the song)
  shuffle: false, // play order only; the list on screen keeps its order
  rate: 1,
  pitch: 0, // semitones, on top of the speed
  keepPitch: false, // cancel out the pitch change that speed brings
  fxScope: 'playlist', // speed and pitch apply to the 'playlist' or just this 'song'
  tab: 'queue',
  art: {},
  settings: { ...DEFAULT_SETTINGS },
};

let deck = 0; // id of the audio deck that is "current" in the engine
let playing = false;
let duration = 0;
let pos = 0;
let playToken = 0;
let xfStarted = false;
let failStreak = 0;
let autoplayIntent = false; // the current song is playing, or is about to
let lastMediaPush = 0;
const prepared = new Map(); // track id -> local path
const preparing = new Map(); // track id -> Promise<path>
const lyricCache = new Map();

// Visual choreography never gets to hold the music hostage.
const within = (promise, ms) => Promise.race([promise, new Promise((r) => setTimeout(() => r(true), ms))]);

const current = () => S.library.find((c) => c.id === S.currentId) || null;
const currentTrack = () => current()?.tracks[S.index] || null;

// ------------------------------------------------------------------ parts

const ambient = new Ambient($('ambient'), $('ripples'));
const tt = new Turntable($('deck'));
tt.formatTime = (p) => fmtTime(p * (duration || (currentTrack()?.durationMs ?? 0) / 1000));
tt.onScrub = (p, final) => {
  const d = duration || (currentTrack()?.durationMs ?? 0) / 1000;
  showProgress(p * d, d);
  if (final) seekTo(p * d);
};

const queue = new Queue($('queueList'), $('queueHead'), {
  onPick: (i) => playIndex(i),
  onMove: (from, to) => moveTrack(from, to),
  onMenu: (i, x, y) => openRowMenu(i, x, y),
});
const lyrics = new Lyrics($('lyricsView'), $('lyrics'), { onSeek: (t) => seekTo(t) });
const warp = new Warp($('warp'), ambient);
warp.turntable = tt; // lets the warp keep the record turning (see warp.js)

// ------------------------------------------------------------------ settings

function applySettings() {
  const st = S.settings;
  invoke('audio_normalize', { on: !!st.normalize }).catch(() => {});
  invoke('set_close_to_tray', { on: st.closeAction !== 'quit' }).catch(() => {});
  invoke('set_cache_limit', { bytes: (st.cacheLimitGB || 0) * 2 ** 30 }).catch(() => {});
  invoke('set_discord', { on: st.discord === true }).catch(() => {});
  // Lite mode: lighter glow, no glass blur, no warp, a still halo. Phones flicker under the warp regardless.
  const lite = !!st.lite;
  app.classList.toggle('lite', lite);
  app.classList.toggle('no-texture', st.texture === false);
  warp.simple = lite || !!PLAT.mobile || st.warp === false;
  if (ambient.lite !== lite) {
    ambient.lite = lite;
    ambient.resize();
  }
}

function setSetting(key, value) {
  S.settings[key] = value;
  applySettings();
  save();
}

function setCrossfade(v) {
  S.crossfade = v;
  renderXf();
  save();
}

// ------------------------------------------------------------------ loop & speed

const LOOPS = ['off', 'all', 'one'];
const LOOP_NAMES = { off: 'Off', all: 'Playlist', one: 'Song' };
function setLoop(mode) {
  S.loop = LOOPS.includes(mode) ? mode : 'off';
  const btn = $('loopBtn');
  btn.dataset.loop = S.loop;
  btn.classList.toggle('off', S.loop === 'off');
  btn.setAttribute('aria-label', `Loop: ${LOOP_NAMES[S.loop].toLowerCase()}`);
  btn.title = `Loop: ${LOOP_NAMES[S.loop]}`;
  save();
}
const LOOP_SAY = { off: 'Loop off', all: 'Looping the whole playlist', one: 'Looping this song' };
$('loopBtn').addEventListener('click', () => {
  setLoop(LOOPS[(LOOPS.indexOf(S.loop) + 1) % LOOPS.length]);
  toast(LOOP_SAY[S.loop], 'info', { id: 'loop' });
});

// Shuffle only changes the order songs are played in, never the list. Everything is tracked by
// each song's own key (`k`, so a song that appears twice in a playlist is two songs), never by
// position, so moving or removing songs in the list cannot point it at the wrong one.
//   hist/hp  what was actually played, and where we are in it (Previous walks back through it)
//   heard    songs already played in this round; a song repeats only once all have played
//   up       songs the user sent to "Play next", used before any random pick
//   pick     the random song chosen to follow this one, decided once so that preparing it,
//            the crossfade and the Next button all agree on the same song
let sh = { colId: null, hist: [], hp: -1, heard: new Set(), up: [], pick: null };
const newKey = () => Math.random().toString(36).slice(2, 9);
const ensureKeys = (col) => col.tracks.forEach((t) => (t.k ||= newKey()));

function shReset() {
  const k = currentTrack()?.k;
  sh = { colId: S.currentId, hist: k ? [k] : [], hp: k ? 0 : -1, heard: new Set(k ? [k] : []), up: [], pick: null };
}

/** A song started playing (by any route). */
function shNote(key) {
  if (sh.colId !== S.currentId) shReset();
  if (sh.pick?.fresh && sh.pick.k === key) sh.heard.clear(); // a new round begins
  sh.heard.add(key);
  sh.up = sh.up.filter((k) => k !== key);
  sh.pick = null;
  if (sh.hist[sh.hp] === key) return;
  if (sh.hist[sh.hp + 1] === key) sh.hp++;
  else if (sh.hp > 0 && sh.hist[sh.hp - 1] === key) sh.hp--;
  else {
    sh.hist.length = sh.hp + 1;
    sh.hist.push(key);
    sh.hp++;
    if (sh.hist.length > 300) {
      sh.hist.shift();
      sh.hp--;
    }
  }
}

/** A song left the list. */
function shForget(key) {
  for (let i = sh.hist.length - 1; i >= 0; i--) {
    if (sh.hist[i] !== key) continue;
    sh.hist.splice(i, 1);
    if (i <= sh.hp) sh.hp--;
  }
  sh.up = sh.up.filter((k) => k !== key);
  if (sh.pick?.k === key) sh.pick = null;
}

/** The index of the song that follows in shuffle order, or -1. */
function shuffleNext(col) {
  if (sh.colId !== S.currentId) shReset();
  const cur = currentTrack()?.k;
  const at = (k) => col.tracks.findIndex((t) => t.k === k);
  const again = sh.hist[sh.hp + 1]; // after going back, Next replays what came next
  if (again) {
    const i = at(again);
    if (i >= 0) return i;
  }
  while (sh.up.length) {
    const i = at(sh.up[0]);
    if (i >= 0 && sh.up[0] !== cur) return i;
    sh.up.shift();
  }
  if (sh.pick) {
    const i = at(sh.pick.k);
    if (i >= 0) return i;
    sh.pick = null;
  }
  let pool = col.tracks.filter((t) => !sh.heard.has(t.k) && t.k !== cur);
  let fresh = false;
  if (!pool.length) {
    if (S.loop !== 'all') return -1; // everything has played once
    fresh = true;
    pool = col.tracks.filter((t) => t.k !== cur);
    if (!pool.length) pool = col.tracks.slice(); // a playlist of one song
  }
  if (!pool.length) return -1;
  const t = pool[Math.floor(Math.random() * pool.length)];
  sh.pick = { k: t.k, fresh };
  return col.tracks.indexOf(t);
}

function setShuffle(on, { quiet = false } = {}) {
  S.shuffle = !!on;
  const btn = $('shuffleBtn');
  btn.classList.toggle('off', !S.shuffle);
  btn.setAttribute('aria-pressed', String(S.shuffle));
  btn.setAttribute('aria-label', `Shuffle: ${S.shuffle ? 'on' : 'off'}`);
  btn.title = `Shuffle: ${S.shuffle ? 'on' : 'off'}`;
  shReset(); // starts a fresh round from the song that is playing now
  if (quiet) return;
  toast(S.shuffle ? 'Shuffle on' : 'Shuffle off', 'info', { id: 'shuffle' });
  prefetchNext();
  save();
}
$('shuffleBtn').addEventListener('click', () => setShuffle(!S.shuffle));

// Speed works like a turntable (slower is deeper); pitch shifts on top of it, and "keep the
// original pitch" cancels the turntable part out. With the "This song" scope both go back to
// normal when a different song starts.
const RATES = [0.75, 0.8, 0.9, 1, 1.1, 1.25];
const fmtRate = (r) => String(+r.toFixed(2));
const fmtPitch = (p) => (p > 0 ? `+${p}` : p < 0 ? `−${-p}` : '');
let fxSong = null; // the song the speed and pitch were set for

function setRate(r) {
  S.rate = Math.round(clamp(r, 0.5, 1.5) * 20) / 20;
  invoke('audio_rate', { rate: S.rate }).catch(() => {});
  tt.rate = S.rate;
  applyFx();
}
function setPitch(p) {
  S.pitch = clamp(Math.round(p), -12, 12);
  applyFx();
}
function applyFx() {
  const shift = S.pitch + (S.keepPitch ? -12 * Math.log2(S.rate) : 0);
  invoke('audio_pitch', { semitones: shift }).catch(() => {});
  const changed = S.rate !== 1 || S.pitch !== 0;
  $('rateLabel').textContent = `${fmtRate(S.rate)}×${S.pitch ? ` ${fmtPitch(S.pitch)}` : ''}`;
  $('rateBtn').classList.toggle('off', !changed);
  $('rateValue').textContent = S.rate === 1 ? 'Normal' : `${fmtRate(S.rate)}×`;
  $('rateRange').value = S.rate;
  $('rateRange').style.setProperty('--fill', `${(S.rate - 0.5) * 100}%`);
  $('ratePresets').querySelectorAll('button').forEach((b) => b.classList.toggle('on', +b.dataset.rate === S.rate));
  $('pitchValue').textContent = S.pitch ? `${fmtPitch(S.pitch)} semitone${Math.abs(S.pitch) === 1 ? '' : 's'}` : 'Original';
  $('pitchRange').value = S.pitch;
  $('pitchRange').style.setProperty('--fill', `${((S.pitch + 12) / 24) * 100}%`);
  $('keepPitch').classList.toggle('on', S.keepPitch);
  const press = PLAT.mobile ? 'Long-press' : 'Right-click';
  $('fxHint').textContent =
    S.fxScope === 'song'
      ? `Just this song: it goes back to normal when the next one starts. ${press} the speed button to change that.`
      : `Stays on for the whole playlist. ${press} the speed button to set it for one song only.`;
  fxSong = currentTrack()?.id ?? null;
  save();
}
/** A song started: with the "This song" scope, a different song plays at normal speed and pitch. */
function fxNewSong(track) {
  if (S.fxScope === 'song' && track.id !== fxSong && (S.rate !== 1 || S.pitch !== 0)) {
    S.pitch = 0;
    setRate(1);
  }
  fxSong = track.id;
}
$('rateRange').addEventListener('input', (e) => setRate(+e.target.value));
$('ratePresets').addEventListener('click', (e) => e.target.dataset.rate && setRate(+e.target.dataset.rate));
$('pitchRange').addEventListener('input', (e) => setPitch(+e.target.value));
$('keepPitch').addEventListener('click', () => {
  S.keepPitch = !S.keepPitch;
  applyFx();
});
// Right-click (long-press on phones) the speed button: whole playlist or this song only.
const fxMenu = $('fxMenu');
function showFxMenu(on, x = 0, y = 0) {
  fxMenu.classList.toggle('open', on);
  fxMenu.setAttribute('aria-hidden', String(!on));
  if (!on) return;
  fxMenu.querySelectorAll('.mi').forEach((b) => b.classList.toggle('on', b.dataset.scope === S.fxScope));
  const a = app.getBoundingClientRect();
  const w = fxMenu.offsetWidth, h = fxMenu.offsetHeight;
  fxMenu.style.left = `${clamp(x - a.left - w / 2, 8, a.width - w - 8)}px`;
  fxMenu.style.top = `${clamp(y - a.top - h - 10, 8, a.height - h - 8)}px`;
}
$('rateBtn').addEventListener('contextmenu', (e) => {
  e.preventDefault();
  closePopovers();
  showFxMenu(true, e.clientX, e.clientY);
});
fxMenu.addEventListener('click', (e) => {
  const scope = e.target.closest('.mi')?.dataset.scope;
  if (!scope) return;
  S.fxScope = scope;
  applyFx();
  showFxMenu(false);
  toast(scope === 'song' ? 'Speed and pitch: this song only' : 'Speed and pitch: the whole playlist', 'info', { id: 'fx' });
});
document.addEventListener('pointerdown', (e) => {
  if (fxMenu.classList.contains('open') && !e.target.closest('#fxMenu')) showFxMenu(false);
});
window.addEventListener('keydown', (e) => e.key === 'Escape' && (showFxMenu(false), showRowMenu(false)));

async function clearCache() {
  const freed = await invoke('clear_cache');
  prepared.clear();
  return freed || 0;
}

function clearLibrary() {
  [...S.library].forEach((c) => removeCollection(c.id));
}

function hideToTray() {
  save(0);
  if (!PLAT.mobile) appWindow?.hide();
}

function quit() {
  save(0);
  setTimeout(() => invoke('quit_app'), 80);
}

async function updateEngine() {
  toast('Updating the YouTube engine…', 'info', { id: 'upd', sticky: true });
  try {
    const version = await invoke('update_engine');
    toast(`YouTube engine is up to date (${version})`, 'ok', { id: 'upd' });
  } catch (e) {
    toast(`Update failed: ${e}`, 'error', { id: 'upd' });
  }
}

const options = new Options($('options'), {
  S,
  invoke,
  warp,
  toast: (...a) => toast(...a),
  setSetting,
  setCrossfade,
  clearCache,
  clearLibrary,
  beforeOpen: () => {
    menubar.close();
    document.querySelectorAll('.modal.open').forEach((m) => openModal(m.id, false));
    closePopovers();
  },
});

// ------------------------------------------------------------------ menus

const nearest = (v, list) => list.reduce((a, b) => (Math.abs(b - v) < Math.abs(a - v) ? b : a));

const menubar = new MenuBar($('menubar'), $('dropdown'), () => {
  const col = current();
  const has = !!currentTrack();
  return [
    {
      id: 'plouk',
      items: [
        { label: 'Options…', kbd: `${MOD()},`, action: () => options.open() },
        { label: 'Play a Spotify link…', kbd: `${MOD()}L`, action: () => openModal('pasteModal') },
        '-',
        { label: 'Hide to tray', kbd: `${MOD()}W`, action: hideToTray },
        { label: 'Quit Plouk', kbd: `${MOD()}Q`, action: quit },
      ],
    },
    {
      id: 'playback',
      items: [
        { label: playing ? 'Pause' : 'Play', kbd: 'Space', disabled: !has, action: togglePlay },
        { label: 'Next song', kbd: 'Shift →', disabled: nextIndex() < 0, action: next },
        { label: 'Previous song', kbd: 'Shift ←', disabled: !has, action: prev },
        '-',
        { label: 'Forward 5 seconds', kbd: '→', disabled: !deck, action: () => seekTo(pos + 5) },
        { label: 'Back 5 seconds', kbd: '←', disabled: !deck, action: () => seekTo(pos - 5) },
        '-',
        {
          type: 'segment',
          label: 'Crossfade',
          value: nearest(S.crossfade, [0, 3, 6, 9, 12]),
          options: [0, 3, 6, 9, 12].map((v) => ({ label: v ? `${v}s` : 'Off', value: v })),
          onPick: setCrossfade,
        },
        {
          type: 'segment',
          label: 'Loop',
          value: S.loop,
          options: LOOPS.map((v) => ({ label: LOOP_NAMES[v], value: v })),
          onPick: setLoop,
        },
        {
          type: 'segment',
          label: 'Speed',
          value: nearest(S.rate, RATES),
          options: RATES.map((v) => ({ label: `${fmtRate(v)}×`, value: v })),
          onPick: setRate,
        },
        '-',
        { label: 'Shuffle', kbd: 'S', checked: S.shuffle, action: () => $('shuffleBtn').click() },
        { label: 'Mute', kbd: 'M', checked: S.muted, action: () => $('muteBtn').click() },
        { label: 'Even out volume', checked: S.settings.normalize, action: () => setSetting('normalize', !S.settings.normalize) },
      ],
    },
    {
      id: 'view',
      items: [
        { label: 'Queue', kbd: 'Q', checked: S.tab === 'queue', action: () => setTab('queue') },
        { label: 'Lyrics', kbd: 'L', checked: S.tab === 'lyrics', action: () => setTab('lyrics') },
        ...(PLAT.mobile
          ? []
          : [
              '-',
              { label: app.classList.contains('maximized') ? 'Restore window' : 'Maximize window', action: () => appWindow?.toggleMaximize() },
              { label: 'Minimize', action: () => appWindow?.minimize() },
            ]),
      ],
    },
    {
      id: 'help',
      items: [
        { label: 'Keyboard shortcuts', kbd: '?', action: () => openModal('helpModal') },
        { label: 'Open song cache folder', action: () => invoke('open_cache_dir').catch((e) => toast(String(e), 'error')) },
        { label: 'Update YouTube engine', action: updateEngine },
        '-',
        { label: 'About Plouk', action: () => options.open('about') },
      ],
    },
  ];
});
$('optBtn').addEventListener('click', () => options.toggle());

// ------------------------------------------------------------------ persistence

// ------------------------------------------------------------------ Last.fm

// A song counts once it has really been listened to for half its length or 4 minutes, and
// only if it's longer than 30 seconds (Last.fm's rule). Without a connected account the
// backend ignores these calls.
let fm = null;
function fmStart(track, col, length) {
  const song = { artist: track.artist, track: track.title, album: col.name, duration: length || 0, timestamp: Math.floor(Date.now() / 1000) };
  fm = { song, listened: 0, sent: false, last: 0 };
  invoke('lastfm_now_playing', { song }).catch(() => {});
}
function fmTick() {
  if (!fm || fm.sent) return;
  const now = performance.now();
  if (playing && fm.last) fm.listened += (now - fm.last) / 1000;
  fm.last = playing ? now : 0;
  const d = fm.song.duration;
  if (d > 30 && fm.listened >= Math.min(d / 2, 240)) {
    fm.sent = true;
    invoke('lastfm_scrobble', { song: fm.song }).catch(() => {});
  }
}

// ------------------------------------------------------------------ self-update

// Checks quietly in the background (see updater.rs); once something is found, it waits for the user.
let updateFound = false;
async function checkUpdates() {
  if (updateFound || S.settings.autoUpdate === false) return;
  const u = await invoke('update_check').catch(() => null);
  if (!u) return;
  updateFound = true;
  $('updVersion').textContent = u.version;
  $('updText').textContent = u.installed
    ? 'It\'s already in place. Restart Plouk to start using it, and you\'ll pick up right where you left off.'
    : PLAT.mobile
      ? 'Tap Install and Android takes it from there. Your music and library stay put.'
      : 'Install it now and Plouk restarts on the new version, right where you left off.';
  $('updGo').textContent = u.installed ? 'Restart' : 'Install';
  showUpdateCard(true);
}
function showUpdateCard(on) {
  $('updateCard').classList.toggle('open', on);
  $('updateCard').setAttribute('aria-hidden', String(!on));
}
$('updLater').addEventListener('click', () => showUpdateCard(false));
$('updGo').addEventListener('click', async () => {
  $('updGo').disabled = true;
  await invoke('save_state', { state: { v: 1, ...S, position: pos || S.position } }).catch(() => {});
  invoke('update_apply')
    .catch((e) => toast(`Update failed: ${e}`, 'error'))
    .finally(() => ($('updGo').disabled = false));
});

let saveTimer = 0;
function save(delay = 700) {
  clearTimeout(saveTimer);
  saveTimer = setTimeout(() => {
    const state = { v: 1, ...S, position: pos || S.position };
    invoke('save_state', { state }).catch(() => {});
  }, delay);
}

// ------------------------------------------------------------------ toasts & status

const toasts = new Map();
function toast(text, kind = 'info', { id, sticky = false, progress, action, ttl } = {}) {
  let el = id && toasts.get(id);
  if (!el) {
    el = document.createElement('div');
    el.innerHTML = '<span class="dot"></span><span class="msg"></span>';
    $('toasts').append(el);
    if (id) toasts.set(id, el);
  }
  el.className = `toast ${kind}`;
  el.querySelector('.msg').textContent = text;
  let meter = el.querySelector('.meter');
  if (progress != null) {
    if (!meter) {
      meter = document.createElement('span');
      meter.className = 'meter';
      meter.innerHTML = '<b></b>';
      el.append(meter);
    }
    meter.firstChild.style.width = `${Math.round(progress * 100)}%`;
  } else meter?.remove();
  el.querySelector('.act')?.remove();
  if (action) {
    const btn = document.createElement('button');
    btn.className = 'act';
    btn.textContent = action.label;
    btn.addEventListener('click', action.run);
    el.append(btn);
  }
  clearTimeout(el._t);
  if (!sticky) el._t = setTimeout(() => dismissToast(el, id), ttl ?? (kind === 'error' ? 5200 : 3200));
  return el;
}
function dismissToast(el, id) {
  if (!el || !el.isConnected) return;
  el.classList.add('out');
  setTimeout(() => el.remove(), 300);
  if (id) toasts.delete(id);
}

let statusTimer = 0;
function setStatus(text, { shimmer = false, error = false, delay = 0 } = {}) {
  clearTimeout(statusTimer);
  const el = $('status');
  const apply = () => {
    el.textContent = text;
    el.className = 'status' + (text ? ' show' : '') + (shimmer ? ' shimmer' : '') + (error ? ' error' : '');
  };
  if (delay) statusTimer = setTimeout(apply, delay);
  else apply();
}

// ------------------------------------------------------------------ meta & color

function setTitle(text, dir = 1) {
  const title = $('title');
  const old = [...title.querySelectorAll('.line:not(.out)')];
  const line = document.createElement('span');
  line.className = 'line';
  text.split(/\s+/).forEach((w) => {
    const s = document.createElement('span');
    s.className = 'w';
    s.textContent = w;
    line.append(s, ' ');
  });
  title.classList.toggle('long', text.length > 26 && text.length <= 42);
  title.classList.toggle('xlong', text.length > 42);
  old.forEach((o) => {
    o.classList.add('out');
    o.animate(
      [
        { opacity: 1, transform: 'none', filter: 'blur(0px)' },
        { opacity: 0, transform: `translateY(${-16 * dir}px)`, filter: 'blur(8px)' },
      ],
      { duration: 380, easing: 'cubic-bezier(.4,0,1,1)', fill: 'forwards' }
    ).finished.then(() => o.remove());
  });
  title.append(line);
  [...line.querySelectorAll('.w')].forEach((w, i) =>
    w.animate(
      [
        { opacity: 0, transform: `translateY(${20 * dir}px)`, filter: 'blur(10px)' },
        { opacity: 1, transform: 'none', filter: 'blur(0px)' },
      ],
      { duration: 760, delay: (old.length ? 160 : 0) + i * 48, easing: 'cubic-bezier(.22,1,.36,1)', fill: 'backwards' }
    )
  );
}

function setSub(parts) {
  const sub = $('sub');
  sub.innerHTML = '';
  parts.filter(Boolean).forEach((p, i) => {
    if (i) sub.insertAdjacentHTML('beforeend', '<span class="dot">·</span>');
    sub.append(p);
  });
  sub.animate([{ opacity: 0, transform: 'translateY(6px)' }, { opacity: 1, transform: 'none' }], {
    duration: 600,
    delay: 220,
    easing: 'cubic-bezier(.22,1,.36,1)',
    fill: 'backwards',
  });
}

async function artFor(track) {
  if (!track) return null;
  if (track.art) return track.art;
  if (S.art[track.id]) return S.art[track.id];
  try {
    const url = await Promise.race([invoke('track_art', { id: track.id }), new Promise((r) => setTimeout(() => r(null), 2500))]);
    if (url) {
      S.art[track.id] = url;
      return url;
    }
  } catch {}
  return current()?.cover || null;
}

async function applyPalette(url) {
  const pal = await extractPalette(url);
  app.style.setProperty('--accent', `rgb(${pal.accent.join(',')})`);
  ambient.setPalette(pal.glow, pal.accent);
}

// ------------------------------------------------------------------ progress UI

let scrubbingBar = false;
// Progress arrives 30 times a second; only touch the page when something visibly changes
// (each write meant a layout and repaint, even while paused).
const shown = { frac: -1, cur: '', dur: '' };
function showProgress(p, d) {
  const frac = d > 0 ? clamp(p / d, 0, 1) : 0;
  if (Math.abs(frac - shown.frac) > 0.0015 || (frac === 0) !== (shown.frac === 0)) {
    shown.frac = frac;
    $('barFill').style.width = `${frac * 100}%`;
    $('barKnob').style.left = `${frac * 100}%`;
  }
  const cur = fmtTime(p), dur = fmtTime(d);
  if (cur !== shown.cur) $('tCur').textContent = shown.cur = cur;
  if (dur !== shown.dur) $('tDur').textContent = shown.dur = dur;
}

function setPlayingUI(on) {
  playing = on;
  ambient.calm = !on;
  app.classList.toggle('playing', on);
  $('playBtn').setAttribute('aria-label', on ? 'Pause' : 'Play');
  pushMedia(true);
}

function pushMedia(force = false) {
  const now = performance.now();
  if (!force && now - lastMediaPush < 4000) return;
  lastMediaPush = now;
  if (currentTrack()) invoke('media_playback', { playing, pos }).catch(() => {});
}

// ------------------------------------------------------------------ preparing audio

function prepare(track) {
  if (prepared.has(track.id)) return Promise.resolve(prepared.get(track.id));
  if (!preparing.has(track.id)) {
    const req = { id: track.id, title: track.title, artist: track.artist, durationMs: track.durationMs };
    preparing.set(
      track.id,
      invoke('prepare_track', { track: req })
        .then((r) => {
          prepared.set(track.id, r.path);
          return r.path;
        })
        .finally(() => preparing.delete(track.id))
    );
  }
  return preparing.get(track.id);
}

/** Gets the song(s) that will follow ready. Works from the play order, so it has to run again
 *  whenever that changes (shuffle, a move, a removal, "Play next"). */
function prefetchNext() {
  const col = current();
  if (!col || S.index < 0 || !currentTrack()) return;
  const ahead = [nextIndex(true)];
  if (!S.shuffle) ahead.push(S.index + 2);
  for (const k of ahead) {
    const t = col.tracks[k];
    if (t) {
      prepare(t).catch(() => {});
      artFor(t).then((u) => u && extractPalette(u));
    }
  }
}

async function loadLyrics(track) {
  const dur = track.durationMs / 1000;
  if (lyricCache.has(track.id)) return lyrics.set(lyricCache.get(track.id));
  lyrics.loading();
  try {
    const res = await invoke('lyrics', { title: track.title, artist: track.artist, duration: dur });
    lyricCache.set(track.id, res);
    if (currentTrack()?.id === track.id) lyrics.set(res);
  } catch {
    if (currentTrack()?.id === track.id) lyrics.set(null);
  }
}

// ------------------------------------------------------------------ playback

function showTrack(track, col, dir = 1) {
  setTitle(track.title, dir);
  setSub([track.artist, col.name]);
  app.classList.remove('is-empty');
  duration = track.durationMs / 1000;
  tt.duration = duration;
}

/**
 * Plays song i of the current collection.
 * transition: 'swap' (record swap) or 'none' (record is already on the platter).
 */
async function playIndex(i, { autoplay = true, start = 0, transition = 'swap' } = {}) {
  const col = current();
  const track = col?.tracks[i];
  if (!track) return;
  const token = ++playToken;
  const dir = i >= S.index ? 1 : -1;
  xfStarted = false;
  S.index = i;
  S.position = start;
  col.lastIndex = i;
  autoplayIntent = autoplay;
  shNote(track.k);
  pos = start;
  queue.setCurrent(i);
  save();

  if (deck && playing) invoke('audio_pause');
  setPlayingUI(false);
  tt.setPlaying(false);
  deck = 0;

  showTrack(track, col, dir);
  showProgress(start, duration);
  loadLyrics(track);

  const art = await artFor(track);
  if (token !== playToken) return;
  applyPalette(art);
  const swapped = transition === 'swap' ? tt.swap(art) : Promise.resolve(true);

  const cached = prepared.has(track.id);
  if (!cached) {
    setStatus('Finding it on YouTube…', { shimmer: true, delay: 300 });
    tt.setLoading(true);
    queue.setLoading(i, true);
  }
  let path;
  try {
    path = await prepare(track);
  } catch (e) {
    if (token !== playToken) return;
    tt.setLoading(false);
    queue.setLoading(i, false);
    failStreak++;
    setStatus(`Couldn't play this one`, { error: true });
    toast(`Couldn't find “${track.title}” — ${String(e).slice(0, 120)}`, 'error');
    const onward = nextIndex();
    if (autoplay && failStreak < 3 && onward >= 0) setTimeout(() => token === playToken && playIndex(onward), 1400);
    return;
  }
  if (token !== playToken) return;
  tt.setLoading(false);
  queue.setLoading(i, false);
  setStatus('');
  failStreak = 0;
  await within(swapped, 3500);
  if (token !== playToken) return;

  invoke('media_meta', { title: track.title, artist: track.artist, album: col.name, cover: art, duration }).catch(() => {});
  fmStart(track, col, duration);
  fxNewSong(track);

  if (!autoplay) return;
  await within(tt.needleDown(duration ? start / duration : 0), 2500);
  if (token !== playToken) return;
  try {
    const res = await invoke('audio_load', { path, start, autoplay: true });
    if (token !== playToken) return;
    deck = res.deck;
    if (res.duration > 1) duration = res.duration;
    tt.duration = duration;
    tt.setPlaying(true);
    setPlayingUI(true);
    prefetchNext();
  } catch (e) {
    prepared.delete(track.id);
    toast(String(e), 'error');
    tt.hover();
  }
}

async function crossfadeTo(track, path, secs) {
  const col = current();
  const token = ++playToken;
  try {
    const res = await invoke('audio_crossfade', { path, secs });
    if (token !== playToken) return;
    deck = res.deck;
    // The list may have been edited while this was starting: find the song again.
    const i = col.tracks.indexOf(track);
    if (i < 0) {
      xfStarted = false;
      invoke('audio_stop');
      deck = 0;
      const onward = nextIndex();
      if (onward >= 0) playIndex(onward);
      else idle();
      return;
    }
    S.index = i;
    col.lastIndex = i;
    shNote(track.k);
    pos = 0;
    xfStarted = false;
    queue.setCurrent(i);
    showTrack(track, col, 1);
    if (res.duration > 1) duration = res.duration;
    tt.duration = duration;
    loadLyrics(track);
    save();
    const art = await artFor(track);
    if (token !== playToken) return;
    applyPalette(art);
    invoke('media_meta', { title: track.title, artist: track.artist, album: col.name, cover: art, duration }).catch(() => {});
    fmStart(track, col, duration);
    fxNewSong(track);
    prefetchNext();
    // The new song is already playing; the turntable catches up with a record swap.
    if (await tt.swap(art)) {
      if (token === playToken && playing) tt.needleDown(duration ? pos / duration : 0);
    }
  } catch (e) {
    xfStarted = false;
    toast(String(e), 'error');
  }
}

function onEnded(id) {
  if (id !== deck) return;
  deck = 0;
  const i = nextIndex(true);
  if (i >= 0) {
    playIndex(i, { transition: i === S.index ? 'none' : 'swap' });
  } else {
    autoplayIntent = false;
    setPlayingUI(false);
    tt.setPlaying(false);
    tt.toRest();
    S.position = 0;
    pos = 0;
    showProgress(0, duration);
    save();
  }
}

function togglePlay() {
  const track = currentTrack();
  if (!track) {
    if (current()?.tracks.length) playIndex(0); // nothing on the platter (the playing song was removed)
    return;
  }
  if (!deck) {
    playIndex(S.index, { start: S.position || 0, transition: tt.hasRecord ? 'none' : 'swap' });
    return;
  }
  if (playing) {
    autoplayIntent = false;
    invoke('audio_pause');
    tt.setPlaying(false);
    tt.hover();
    setPlayingUI(false);
    save(0);
  } else {
    autoplayIntent = true;
    invoke('audio_play');
    tt.setPlaying(true);
    tt.lower();
    setPlayingUI(true);
  }
}

/** The song after this one (in shuffle order when shuffle is on), or -1. Only a song ending by itself (`ended`) repeats under "loop song";
 *  pressing Next still moves on. "Loop playlist" wraps around to the first song. */
function nextIndex(ended = false) {
  const col = current();
  if (!col || S.index < 0) return -1;
  if (ended && S.loop === 'one') return S.index;
  if (S.shuffle) return shuffleNext(col);
  if (S.index + 1 < col.tracks.length) return S.index + 1;
  return S.loop === 'all' ? 0 : -1;
}

function next() {
  const i = nextIndex();
  if (i >= 0) playIndex(i);
}

function prev() {
  if (pos > 4 && deck) return seekTo(0);
  const col = current();
  if (S.shuffle) {
    // back through what was really played
    const i = col ? col.tracks.findIndex((t) => t.k === sh.hist[sh.hp - 1]) : -1;
    if (i >= 0) playIndex(i);
    else if (deck) seekTo(0);
    return;
  }
  if (S.index > 0) playIndex(S.index - 1);
  else if (col && S.loop === 'all') playIndex(col.tracks.length - 1);
}

function seekTo(secs) {
  const d = duration || 0;
  secs = clamp(secs, 0, Math.max(0, d - 0.5));
  pos = secs;
  S.position = secs;
  xfStarted = false;
  showProgress(secs, d);
  tt.setProgress(d ? secs / d : 0);
  lyrics.update(secs, true);
  if (deck) invoke('audio_seek', { secs });
  pushMedia(true);
  save();
}

function setVolume(v) {
  S.volume = clamp(v, 0, 1);
  S.muted = false;
  applyVolume();
  save();
}

function applyVolume() {
  const v = S.muted ? 0 : S.volume;
  invoke('audio_volume', { volume: v });
  invoke('media_volume', { volume: v }).catch(() => {});
  $('volFill').style.width = `${v * 100}%`;
  $('volKnob').style.left = `${v * 100}%`;
  const vol = document.querySelector('.vol');
  vol.classList.toggle('muted', v === 0);
  vol.classList.toggle('low', v > 0 && v < 0.34);
  vol.classList.toggle('mid', v >= 0.34 && v < 0.67);
}

// ------------------------------------------------------------------ engine events

let breathTarget = 0;
let breath = 0;
let bassPeak = 0.08;
let lastLevel = 0;
let lastSave = 0;

listen('audio', ({ payload: e }) => {
  if (!deck && e.playing && currentTrack()) {
    deck = e.deck;
    tt.setPlaying(true);
    tt.lower();
    setPlayingUI(true);
  }
  if (e.deck !== deck) return;
  pos = e.pos;
  if (e.dur > 1) duration = e.dur;
  fmTick();
  if (!scrubbingBar && tt.mode !== 'drag') {
    showProgress(pos, duration);
    tt.setProgress(duration ? pos / duration : 0);
  }
  lyrics.update(pos);
  bassPeak = Math.max(bassPeak * 0.996, e.bass, 0.02);
  breathTarget = playing ? Math.pow(Math.min(1, e.bass / bassPeak), 1.7) : 0;
  lastLevel = playing ? Math.min(1, e.level * 3.2) : 0;
  pushMedia();
  if (playing && performance.now() - lastSave > 10000) {
    lastSave = performance.now();
    S.position = pos;
    save(0);
  }

  // Start the crossfade once the next song is ready and the end is near.
  // "Loop song" restarts instead (see onEnded); blending a song into itself sounds muddy.
  const ni = S.loop === 'one' ? -1 : nextIndex(true);
  const nextTrack = ni >= 0 ? current()?.tracks[ni] : null;
  if (playing && nextTrack && S.crossfade > 0 && !xfStarted && duration > S.crossfade + 10) {
    const remain = (duration - pos) / S.rate;
    if (remain <= S.crossfade) {
      const path = prepared.get(nextTrack.id);
      if (path) {
        xfStarted = true;
        crossfadeTo(nextTrack, path, Math.max(1, remain));
      }
    } else if (remain < S.crossfade + 40) {
      prepare(nextTrack).catch(() => {});
    }
  }
});

listen('audio-ended', ({ payload }) => onEnded(payload));

function onEngine(s) {
  if (s.state === 'downloading') {
    toast('Setting up the YouTube engine (one time)…', 'info', { id: 'engine', sticky: true, progress: s.progress });
  } else if (s.state === 'ready') {
    if (toasts.has('engine')) toast('YouTube engine ready', 'ok', { id: 'engine' });
  } else if (s.state === 'failed') {
    toast(s.message, 'error', { id: 'engine', sticky: true });
  }
}
listen('engine', ({ payload }) => onEngine(payload));

listen('media-key', ({ payload: k }) => {
  switch (k.action) {
    case 'toggle': return togglePlay();
    case 'play': return !playing && togglePlay();
    case 'pause': return playing && togglePlay();
    case 'next': return next();
    case 'prev': return prev();
    case 'seekBy': return seekTo(pos + (k.value || 0));
    case 'setPos': return seekTo(k.value || 0);
    case 'volume': return setVolume(k.value ?? S.volume);
    case 'open': return k.uri && loadFromText(k.uri);
  }
});

const halo = document.querySelector('.halo');
let drawnBreath = -1;
function frame() {
  breath += (breathTarget - breath) * (breathTarget > breath ? 0.3 : 0.06);
  if (!playing) breathTarget *= 0.9;
  // Set the halo directly (a CSS variable on #deck restyled the whole deck every frame).
  if (!ambient.lite && Math.abs(breath - drawnBreath) > 0.002) {
    drawnBreath = breath;
    halo.style.opacity = (0.38 + breath * 0.5).toFixed(3);
    halo.style.transform = `scale(${(0.96 + breath * 0.05).toFixed(4)})`;
  }
  ambient.breath = breath;
  queue.pulse(lastLevel, playing);
  requestAnimationFrame(frame);
}
requestAnimationFrame(frame);

// ------------------------------------------------------------------ library

function renderShelf(popId) {
  const shelf = $('shelf');
  shelf.innerHTML = '';
  if (!S.library.length) {
    shelf.innerHTML = '<div class="shelf-ghost"></div><div class="shelf-ghost"></div><div class="shelf-ghost"></div>';
    return;
  }
  S.library.forEach((col, i) => {
    const item = document.createElement('button');
    item.className = 'sleeve-item' + (col.id === S.currentId ? ' active' : '');
    item.style.setProperty('--i', i);
    item.title = col.name;
    item.innerHTML = `
      <div class="sleeve-cover"></div>
      <div class="sleeve-info"><div class="sleeve-name"></div><div class="sleeve-count">${col.tracks.length} songs</div></div>
      <span class="sleeve-remove" role="button" aria-label="Remove from library"><svg viewBox="0 0 12 12"><path d="M3 3l6 6M9 3l-6 6"/></svg></span>`;
    item.querySelector('.sleeve-cover').style.backgroundImage = col.cover ? `url("${col.cover}")` : '';
    item.querySelector('.sleeve-name').textContent = col.name;
    item.addEventListener('click', (e) => {
      if (e.target.closest('.sleeve-remove')) return removeCollection(col.id);
      if (col.id !== S.currentId) openCollection(col.id, { index: col.lastIndex || 0 });
    });
    shelf.append(item);
    if (col.id === popId) {
      const { easing, duration } = spring({ stiffness: 220, damping: 14 });
      item.querySelector('.sleeve-cover').animate(
        [
          { transform: 'scale(.2) rotate(-25deg)', opacity: 0, filter: 'blur(6px)' },
          { transform: 'none', opacity: 1, filter: 'blur(0px)' },
        ],
        { duration, easing, delay: 200 }
      );
    }
  });
}

function removeCollection(id) {
  const i = S.library.findIndex((c) => c.id === id);
  if (i < 0) return;
  S.library.splice(i, 1);
  if (id === S.currentId) {
    ++playToken;
    if (deck) invoke('audio_stop');
    deck = 0;
    autoplayIntent = false;
    S.currentId = null;
    S.index = -1;
    setPlayingUI(false);
    tt.clear();
    showEmpty();
    queue.render(null);
    lyrics.empty();
  }
  renderShelf();
  save();
}

function openCollection(id, { index = 0, autoplay = true, cascade = true, start = 0 } = {}) {
  const col = S.library.find((c) => c.id === id);
  if (!col) return;
  S.currentId = id;
  S.index = -1;
  shReset();
  renderShelf();
  if (!col.tracks.length) {
    queue.render(col, -1, { cascade });
    return idle();
  }
  index = clamp(index, 0, col.tracks.length - 1); // the saved position can be stale after edits
  queue.render(col, index, { cascade });
  playIndex(index, { autoplay, start });
}

/** Nothing is playing and nothing is lined up (the last song was removed, or the list is empty). */
function idle() {
  ++playToken;
  if (deck) invoke('audio_stop');
  deck = 0;
  autoplayIntent = false;
  S.index = -1;
  S.position = 0;
  pos = 0;
  setPlayingUI(false);
  tt.clear();
  setStatus('');
  lyrics.empty();
  showEmpty('Nothing playing', 'Pick a song from the list');
  save();
}

// ------------------------------------------------------------------ editing the queue

/** The list changed: keep the playing song as the current one (by key, not by position). */
function afterEdit(col, key, { counts = false } = {}) {
  S.index = key == null ? -1 : col.tracks.findIndex((t) => t.k === key);
  if (S.index >= 0) col.lastIndex = S.index;
  sh.pick = null; // the order changed: the song picked to follow may no longer be the right one
  queue.sync(col, S.index);
  if (counts) renderShelf();
  save();
  prefetchNext(); // and so the next song's audio must be ready, not the old neighbour's
}

function moveTrack(from, to) {
  const col = current();
  if (!col || !col.tracks[from] || to < 0 || to >= col.tracks.length || from === to) return;
  const key = currentTrack()?.k ?? null;
  col.tracks.splice(to, 0, col.tracks.splice(from, 1)[0]);
  afterEdit(col, key);
}

function removeTrack(i) {
  const col = current();
  const track = col?.tracks[i];
  if (!track) return;
  const wasCurrent = i === S.index;
  const wasPlaying = playing || (autoplayIntent && !deck);
  let onto = null; // the song to move on to when the playing one is removed
  if (wasCurrent) {
    const n = nextIndex();
    if (n >= 0 && n !== i) onto = col.tracks[n].k;
  }
  const key = wasCurrent ? null : (currentTrack()?.k ?? null);
  col.tracks.splice(i, 1);
  shForget(track.k);
  if (wasCurrent) {
    S.index = -1;
    sh.pick = null;
    queue.sync(col, -1);
    renderShelf();
    const ni = onto ? col.tracks.findIndex((t) => t.k === onto) : -1;
    if (ni >= 0) playIndex(ni, { autoplay: wasPlaying });
    else idle();
  } else afterEdit(col, key, { counts: true });
  const id = `rm:${track.k}`;
  toast(`Removed “${track.title}”`, 'info', {
    id,
    ttl: 8000,
    action: {
      label: 'Undo',
      run: () => {
        dismissToast(toasts.get(id), id);
        restoreTrack(col, track, i, wasCurrent);
      },
    },
  });
}

function restoreTrack(col, track, at, wasCurrent) {
  if (!S.library.includes(col) || col.tracks.some((t) => t.k === track.k)) return;
  col.tracks.splice(Math.min(at, col.tracks.length), 0, track);
  if (col.id !== S.currentId) {
    renderShelf();
    save();
    return;
  }
  afterEdit(col, currentTrack()?.k ?? null, { counts: true });
  // If removing it had stopped everything, bring it back as the song on the platter.
  if (wasCurrent && S.index < 0) playIndex(col.tracks.indexOf(track), { autoplay: false });
}

/** Puts a song directly after the one playing. With shuffle on the list stays as it is and the
 *  song jumps the shuffle order instead. */
function playNext(i) {
  const col = current();
  const track = col?.tracks[i];
  if (!track || S.index < 0 || i === S.index) return;
  if (S.shuffle) {
    sh.up = [track.k, ...sh.up.filter((k) => k !== track.k)];
    sh.pick = null;
    prefetchNext();
  } else {
    const key = currentTrack().k;
    col.tracks.splice(i, 1);
    col.tracks.splice(col.tracks.findIndex((t) => t.k === key) + 1, 0, track);
    afterEdit(col, key);
  }
  toast(`“${track.title}” plays next`, 'info', { id: 'playnext' });
}

// Right-click (or press and hold, then let go) a song: Play next, Remove.
const rowMenu = $('rowMenu');
let rowMenuKey = null;
function showRowMenu(on, x = 0, y = 0, touch = false) {
  if (!on && !rowMenu.classList.contains('open')) return;
  rowMenu.classList.toggle('open', on);
  rowMenu.setAttribute('aria-hidden', String(!on));
  if (!on) return;
  const a = app.getBoundingClientRect();
  const w = rowMenu.offsetWidth, h = rowMenu.offsetHeight;
  rowMenu.style.left = `${clamp(x - a.left - (touch ? w / 2 : 0), 8, a.width - w - 8)}px`;
  rowMenu.style.top = `${clamp(y - a.top - (touch ? h + 28 : 0), 8, a.height - h - 8)}px`;
}
function openRowMenu(i, x, y) {
  const track = current()?.tracks[i];
  if (!track) return;
  closePopovers();
  showFxMenu(false);
  rowMenuKey = track.k;
  rowMenu.querySelector('.ctx-title').textContent = track.title;
  const alreadyNext = S.shuffle ? sh.up[0] === track.k : i === S.index + 1;
  rowMenu.querySelector('[data-act="next"]').classList.toggle('disabled', S.index < 0 || i === S.index || alreadyNext);
  showRowMenu(true, x, y, queue.lastType !== 'mouse');
}
rowMenu.addEventListener('click', (e) => {
  const act = e.target.closest('.mi')?.dataset.act;
  if (!act) return;
  showRowMenu(false);
  const i = current()?.tracks.findIndex((t) => t.k === rowMenuKey) ?? -1;
  if (i < 0) return;
  if (act === 'next') playNext(i);
  else removeTrack(i);
});
document.addEventListener('pointerdown', (e) => {
  if (!e.target.closest('#rowMenu')) showRowMenu(false);
});
$('queueList').addEventListener('scroll', () => showRowMenu(false), { passive: true });

async function loadFromText(text, point) {
  menubar.close();
  if (options.isOpen) options.close();
  if (!/spotify/i.test(text || '')) {
    toast("That doesn't look like a Spotify link", 'error');
    return;
  }
  if (point) ambient.ripple(point.x, point.y, 2.4);
  const t = toast('Reading from Spotify…', 'info', { id: 'read', sticky: true });
  let col;
  try {
    col = await invoke('fetch_collection', { input: text });
  } catch (e) {
    toast(String(e), 'error', { id: 'read' });
    return;
  }
  dismissToast(t, 'read');
  ensureKeys(col);
  const existing = S.library.findIndex((c) => c.id === col.id);
  if (existing >= 0) {
    const old = S.library.splice(existing, 1)[0];
    col.lastIndex = old.lastIndex;
  }
  col.addedAt = Date.now();
  S.library.unshift(col);
  renderShelf(col.id);
  openCollection(col.id, { index: 0, cascade: true });
}

function showEmpty(title, sub) {
  app.classList.add('is-empty');
  setTitle(title || (PLAT.mobile ? 'Share a Spotify playlist' : 'Drop a Spotify playlist'));
  $('sub').textContent = sub || (PLAT.mobile
    ? 'In Spotify, tap Share → Plouk, or tap + to paste a link'
    : `Drag it straight out of the Spotify app, or paste a link with ${MOD()}V`);
  showProgress(0, 0);
  applyPalette(null);
  app.style.setProperty('--accent', `rgb(${DEFAULT_PALETTE.accent.join(',')})`);
}

// ------------------------------------------------------------------ drag & drop

let dragDepth = 0;
let lastRipple = { x: -999, y: -999, t: 0 };

function dragMove(e) {
  const r = app.getBoundingClientRect();
  $('dropOverlay').style.setProperty('--mx', `${e.clientX - r.left}px`);
  $('dropOverlay').style.setProperty('--my', `${e.clientY - r.top}px`);
  const now = performance.now();
  const moved = Math.hypot(e.clientX - lastRipple.x, e.clientY - lastRipple.y);
  if (moved > 28 || now - lastRipple.t > 520) {
    ambient.ripple(e.clientX, e.clientY, moved > 28 ? 0.9 : 0.55);
    lastRipple = { x: e.clientX, y: e.clientY, t: now };
  }
}

function setDragging(on) {
  if (on) menubar.close();
  app.classList.toggle('dragging', on);
  ambient.setDragging(on);
}

window.addEventListener('dragenter', (e) => {
  e.preventDefault();
  if (++dragDepth === 1) setDragging(true);
});
window.addEventListener('dragover', (e) => {
  e.preventDefault();
  if (e.dataTransfer) e.dataTransfer.dropEffect = 'copy';
  if (!app.classList.contains('dragging')) setDragging(true);
  dragMove(e);
});
window.addEventListener('dragleave', (e) => {
  if (--dragDepth <= 0 || (e.clientX <= 0 && e.clientY <= 0)) {
    dragDepth = 0;
    setDragging(false);
  }
});
window.addEventListener('drop', (e) => {
  e.preventDefault();
  dragDepth = 0;
  setDragging(false);
  const dt = e.dataTransfer;
  const parts = [];
  for (const type of dt?.types || []) {
    if (type === 'Files') continue;
    try {
      const v = dt.getData(type);
      if (v) parts.push(v);
    } catch {}
  }
  loadFromText(parts.join('\n'), { x: e.clientX, y: e.clientY });
});

document.addEventListener('paste', (e) => {
  if (e.target.closest?.('input')) return;
  const text = e.clipboardData?.getData('text') || '';
  if (/spotify/i.test(text)) loadFromText(text, { x: innerWidth / 2, y: innerHeight / 2 });
});

// ------------------------------------------------------------------ controls

$('playBtn').addEventListener('click', togglePlay);
$('nextBtn').addEventListener('click', next);
$('prevBtn').addEventListener('click', prev);
$('muteBtn').addEventListener('click', () => {
  S.muted = !S.muted;
  applyVolume();
  save();
});

function bindBar(bar, { onMove, onEnd }) {
  bar.addEventListener('pointerdown', (e) => {
    bar.setPointerCapture(e.pointerId);
    bar.classList.add('dragging');
    const at = (ev) => {
      const r = bar.getBoundingClientRect();
      return clamp((ev.clientX - r.left) / r.width, 0, 1);
    };
    onMove(at(e));
    const move = (ev) => onMove(at(ev));
    const up = (ev) => {
      bar.classList.remove('dragging');
      bar.removeEventListener('pointermove', move);
      bar.removeEventListener('pointerup', up);
      onEnd(at(ev));
    };
    bar.addEventListener('pointermove', move);
    bar.addEventListener('pointerup', up);
  });
}

bindBar($('bar'), {
  onMove: (f) => {
    scrubbingBar = true;
    showProgress(f * duration, duration);
  },
  onEnd: (f) => {
    scrubbingBar = false;
    seekTo(f * duration);
  },
});
bindBar($('volBar'), { onMove: setVolume, onEnd: setVolume });
$('volBar').addEventListener('wheel', (e) => setVolume(S.volume - Math.sign(e.deltaY) * 0.05), { passive: true });

// crossfade popover
function renderXf() {
  $('xfLabel').textContent = S.crossfade ? `${S.crossfade}s` : 'Off';
  $('xfValue').textContent = S.crossfade ? `${S.crossfade} s` : 'Off';
  $('xfBtn').classList.toggle('off', !S.crossfade);
  $('xfRange').value = S.crossfade;
  $('xfRange').style.setProperty('--fill', `${(S.crossfade / 12) * 100}%`);
}
function closePopovers(except) {
  showRowMenu(false);
  document.querySelectorAll('.pop-wrap').forEach((w) => {
    if (w === except) return;
    w.querySelector('.popover').classList.remove('open');
    w.querySelector('.chip').classList.remove('open');
  });
}
document.querySelectorAll('.pop-wrap .chip').forEach((chip) =>
  chip.addEventListener('click', (e) => {
    e.stopPropagation();
    const wrap = chip.closest('.pop-wrap');
    closePopovers(wrap);
    wrap.querySelector('.popover').classList.toggle('open');
    chip.classList.toggle('open');
  })
);
$('xfRange').addEventListener('input', (e) => {
  S.crossfade = +e.target.value;
  renderXf();
  save();
});
document.addEventListener('pointerdown', (e) => {
  if (!e.target.closest('.pop-wrap')) closePopovers();
});

// tabs
function setTab(tab) {
  S.tab = tab;
  $('tabs').dataset.active = tab;
  document.querySelectorAll('.tab').forEach((b) => b.classList.toggle('active', b.dataset.tab === tab));
  $('queueView').classList.toggle('active', tab === 'queue');
  $('queueView').classList.toggle('left', tab !== 'queue');
  $('lyricsView').classList.toggle('active', tab === 'lyrics');
  if (tab === 'lyrics') requestAnimationFrame(() => lyrics.position());
  save();
}
document.querySelectorAll('.tab').forEach((b) => b.addEventListener('click', () => setTab(b.dataset.tab)));

// paste dialog & help
function openModal(id, on = true) {
  $(id).classList.toggle('open', on);
  if (id === 'pasteModal' && on) setTimeout(() => $('pasteInput').focus(), 50);
}
$('addBtn').addEventListener('click', () => openModal('pasteModal'));
$('pasteCancel').addEventListener('click', () => openModal('pasteModal', false));
$('pasteForm').addEventListener('submit', (e) => {
  e.preventDefault();
  const v = $('pasteInput').value.trim();
  openModal('pasteModal', false);
  $('pasteInput').value = '';
  if (v) loadFromText(v, { x: innerWidth / 2, y: innerHeight / 2 });
});
document.querySelectorAll('.modal').forEach((m) => m.addEventListener('pointerdown', (e) => e.target === m && openModal(m.id, false)));

// keyboard
window.addEventListener('keydown', (e) => {
  if (e.target.closest?.('input')) {
    if (e.key === 'Escape') openModal('pasteModal', false);
    return;
  }
  const k = e.key;
  if (e.ctrlKey || e.metaKey) {
    const key = k.toLowerCase();
    const run = { ',': () => options.toggle(), q: quit, w: hideToTray, l: () => openModal('pasteModal'), arrowright: next, arrowleft: prev }[key];
    if (run) {
      e.preventDefault();
      run();
    }
    return; // Ctrl+V falls through to the paste handler
  }
  if (options.isOpen) return;
  if (k === ' ') {
    e.preventDefault();
    if (!e.repeat) togglePlay();
  } else if (k === 'ArrowRight' && (e.shiftKey || e.ctrlKey)) next();
  else if (k === 'ArrowLeft' && (e.shiftKey || e.ctrlKey)) prev();
  else if (k === 'ArrowRight') seekTo(pos + 5);
  else if (k === 'ArrowLeft') seekTo(pos - 5);
  else if (k === 'ArrowUp') {
    e.preventDefault();
    setVolume(S.volume + 0.05);
  } else if (k === 'ArrowDown') {
    e.preventDefault();
    setVolume(S.volume - 0.05);
  } else if (k === 'm' || k === 'M') $('muteBtn').click();
  else if (k === 'r' || k === 'R') $('loopBtn').click();
  else if (k === 's' || k === 'S') $('shuffleBtn').click();
  else if (k === '[') setRate(S.rate - 0.05);
  else if (k === ']') setRate(S.rate + 0.05);
  else if (k === '\\') {
    S.pitch = 0;
    setRate(1);
  }
  else if (k === 'l' || k === 'L') setTab('lyrics');
  else if (k === 'q' || k === 'Q') setTab('queue');
  else if (k === '?') openModal('helpModal', !$('helpModal').classList.contains('open'));
  else if (k === 'Escape') document.querySelectorAll('.modal.open').forEach((m) => openModal(m.id, false));
});

// Space belongs to play/pause: never let it also "click" whichever button has focus.
window.addEventListener('keyup', (e) => {
  if (e.key === ' ' && !e.target.closest?.('input')) e.preventDefault();
});
document.addEventListener('pointerup', (e) => {
  const b = e.target.closest?.('button');
  if (b && e.pointerType === 'mouse') setTimeout(() => b.blur(), 0);
});

// window controls
document.querySelectorAll('.win-btn').forEach((b) =>
  b.addEventListener('click', () => {
    if (!appWindow) return;
    const w = b.dataset.win;
    if (w === 'min') appWindow.minimize();
    if (w === 'max') appWindow.toggleMaximize();
    if (w === 'close') S.settings.closeAction === 'quit' ? quit() : hideToTray();
  })
);
async function syncMaximized() {
  if (appWindow && !PLAT.mobile) app.classList.toggle('maximized', await appWindow.isMaximized().catch(() => false));
}
window.addEventListener('resize', () => {
  syncMaximized();
  if (S.tab === 'lyrics') lyrics.position(true);
});
window.addEventListener('beforeunload', () => save(0));

// ------------------------------------------------------------------ phone bottom sheet

function setupSheet() {
  const panel = $('panel');
  const handle = panel.querySelector('.sheet-handle');
  const setOpen = (on) => panel.classList.toggle('expanded', on);
  handle.addEventListener('click', () => setOpen(!panel.classList.contains('expanded')));
  document.querySelectorAll('.tab').forEach((b) => b.addEventListener('click', () => setOpen(true)));
  // Drag the handle to pull the sheet up or push it down.
  let startY = 0, dy = 0, dragging = false;
  handle.addEventListener('pointerdown', (e) => {
    dragging = true;
    startY = e.clientY;
    dy = 0;
    handle.setPointerCapture(e.pointerId);
    panel.classList.add('dragging');
  });
  handle.addEventListener('pointermove', (e) => {
    if (!dragging) return;
    dy = e.clientY - startY;
    panel.style.setProperty('--drag', `${dy}px`);
  });
  const end = () => {
    if (!dragging) return;
    dragging = false;
    panel.classList.remove('dragging');
    panel.style.removeProperty('--drag');
    if (Math.abs(dy) > 40) setOpen(dy < 0);
  };
  handle.addEventListener('pointerup', end);
  handle.addEventListener('pointercancel', end);
}

// ------------------------------------------------------------------ boot

async function boot() {
  Object.assign(PLAT, (await invoke('platform').catch(() => null)) || {});
  app.classList.add(`os-${PLAT.os}`);
  app.classList.toggle('mobile', !!PLAT.mobile);
  if (isMac()) document.querySelectorAll('kbd').forEach((k) => k.textContent === 'Ctrl' && (k.textContent = '⌘'));
  if (PLAT.mobile) setupSheet();
  const saved = await invoke('load_state').catch(() => null);
  if (saved && saved.v === 1) Object.assign(S, saved, { muted: false });
  S.settings = { ...DEFAULT_SETTINGS, ...(saved?.settings || {}) };
  S.library.forEach(ensureKeys);
  if (S.settings.lite == null) S.settings.lite = !!PLAT.mobile;
  if (S.settings.launchVolume !== 'last') S.volume = S.settings.launchVolume;
  applySettings();
  setLoop(S.loop);
  setShuffle(S.shuffle, { quiet: true });
  setRate(S.rate || 1);
  setTimeout(checkUpdates, 20000);
  setInterval(checkUpdates, 6 * 3600 * 1000);
  renderXf();
  applyVolume();
  renderShelf();
  setTab(S.tab || 'queue');
  syncMaximized();

  const col = current();
  const track = currentTrack();
  if (col && track) {
    queue.render(col, S.index, { cascade: false });
    showTrack(track, col);
    pos = S.position || 0;
    showProgress(pos, duration);
    loadLyrics(track);
    const art = await artFor(track);
    applyPalette(art);
    tt.placeRecord(art, duration ? pos / duration : 0);
    invoke('media_meta', { title: track.title, artist: track.artist, album: col.name, cover: art, duration }).catch(() => {});
    pushMedia(true);
    prepare(track).catch(() => {});
    if (S.settings.autoplay) setTimeout(() => !deck && togglePlay(), 900);
  } else {
    S.currentId = null;
    queue.render(null);
    lyrics.empty();
    showEmpty();
  }

  invoke('engine_state').then(onEngine).catch(() => {});
  invoke('take_launch_uri')
    .then((uri) => uri && setTimeout(() => loadFromText(uri, { x: innerWidth / 2, y: innerHeight / 2 }), 900))
    .catch(() => {});

  const hidden = (await invoke('launch_hidden').catch(() => false)) && S.settings.startHidden;
  requestAnimationFrame(() =>
    requestAnimationFrame(() => {
      if (!hidden && !PLAT.mobile) appWindow?.show();
      app.classList.remove('booting');
      setTimeout(() => app.classList.add('ready'), 1400);
    })
  );
  if (!isTauri) console.info('[plouk] design preview: running with a mock backend');
}

boot();
