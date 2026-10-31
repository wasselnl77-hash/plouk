// Bridge to the Rust core. Opened in a plain browser (design preview), it falls
// back to a small mock so the UI can be explored without the desktop app.

const T = window.__TAURI__;
export const isTauri = !!T;

export const invoke = T ? T.core.invoke : mockInvoke;
export const listen = T ? T.event.listen : mockListen;
export const appWindow = T ? T.window.getCurrentWindow() : null;

// ------------------------------------------------------------------ mock

const bus = new Map();
function mockListen(name, cb) {
  if (!bus.has(name)) bus.set(name, new Set());
  bus.get(name).add(cb);
  return Promise.resolve(() => bus.get(name).delete(cb));
}
function emit(name, payload) {
  bus.get(name)?.forEach((cb) => cb({ payload }));
}

const ART = [
  'https://i.scdn.co/image/ab67616d0000b273255e131abc1410833be95673',
  'https://i.scdn.co/image/ab67616d0000b273db216ca805faf5fe35df4ee6',
];
const DEMO = {
  id: 'playlist:demo',
  kind: 'playlist',
  name: 'Late Night Drive',
  subtitle: 'Plouk demo',
  cover: 'https://i.scdn.co/image/ab67706f00000003960019ea66cba66a44fdd4c2',
  url: null,
  tracks: [
    ['Midnight Glass', 'Aurora Vale', 214],
    ['Low Orbit', 'The Quiet Hours', 247],
    ['Neon Tide', 'Kasumi', 198],
    ['Paper Satellites', 'Hollow Pines', 231],
    ['Static Bloom', 'Mira Sol', 205],
    ['Velvet Signal', 'Northwake', 262],
    ['Afterglow Avenue', 'Juno Park', 189],
    ['Cassette Heart', 'Lumen & Ash', 223],
    ['Slow Motion City', 'Evening Standard', 240],
    ['Blue Hour', 'Sable', 201],
    ['Glasshouse', 'Oda Rye', 216],
    ['Drift', 'Pale Harbor', 254],
  ].map(([title, artist, s], i) => ({
    id: `demo${i}`,
    title,
    artist,
    durationMs: s * 1000,
    explicit: i === 3,
    art: ART[i % 2],
  })),
};

let mock = { deck: 0, pos: 0, dur: 0, playing: false, timer: 0, vol: 0.8 };
function mockTick() {
  if (mock.playing) mock.pos += 0.033;
  const t = performance.now() / 1000;
  const beat = Math.pow(Math.max(0, Math.sin(t * Math.PI * 2 * 1.05)), 6);
  emit('audio', {
    deck: mock.deck,
    pos: mock.pos,
    dur: mock.dur,
    playing: mock.playing,
    level: mock.playing ? 0.12 + beat * 0.1 : 0,
    bass: mock.playing ? 0.04 + beat * 0.22 : 0,
  });
  if (mock.pos >= mock.dur && mock.playing) {
    mock.playing = false;
    clearInterval(mock.timer);
    emit('audio-ended', mock.deck);
  }
}
function mockStart(dur, start, autoplay) {
  clearInterval(mock.timer);
  mock = { ...mock, deck: mock.deck + 1, pos: start, dur, playing: autoplay };
  mock.timer = setInterval(mockTick, 33);
  return { deck: mock.deck, duration: dur };
}

const wait = (ms) => new Promise((r) => setTimeout(r, ms));
let mockState = null;

async function mockInvoke(cmd, args = {}) {
  switch (cmd) {
    case 'fetch_collection':
      await wait(450);
      return structuredClone(DEMO);
    case 'track_art':
      return ART[0];
    case 'prepare_track':
      await wait(500 + Math.random() * 600);
      return { path: `mock:${args.track.id}:${args.track.durationMs}` };
    case 'engine_state':
      return { state: 'ready', version: 'mock' };
    case 'lyrics':
      await wait(300);
      return {
        synced: Array.from({ length: 40 }, (_, i) => {
          const t = 4 + i * 4.2;
          const m = Math.floor(t / 60);
          const s = (t % 60).toFixed(2).padStart(5, '0');
          const lines = ['Neon on the window, rain in slow motion', 'I keep the city humming in my chest', '', 'We were glass and static, nothing else', 'Turn it up, the night is ours again', 'Every light a record spinning'];
          return `[${String(m).padStart(2, '0')}:${s}] ${lines[i % lines.length]}`;
        }).join('\n'),
      };
    case 'audio_load': {
      const dur = +String(args.path).split(':')[2] / 1000 || 200;
      return mockStart(dur, args.start || 0, args.autoplay);
    }
    case 'audio_crossfade': {
      const dur = +String(args.path).split(':')[2] / 1000 || 200;
      return mockStart(dur, 0, true);
    }
    case 'audio_play':
      mock.playing = true;
      return;
    case 'audio_pause':
      mock.playing = false;
      return;
    case 'audio_seek':
      mock.pos = args.secs;
      return;
    case 'audio_stop':
      clearInterval(mock.timer);
      mock.playing = false;
      return;
    case 'load_state':
      try {
        return JSON.parse(localStorage.getItem('plouk-state'));
      } catch {
        return mockState;
      }
    case 'save_state':
      mockState = args.state;
      try {
        localStorage.setItem('plouk-state', JSON.stringify(args.state));
      } catch {}
      return;
    case 'app_info':
      return { version: '0.1.0', engine: { state: 'ready', version: 'mock' } };
    case 'cache_info':
      return { bytes: 734003200, songs: 142 };
    case 'clear_cache':
      return 734003200;
    case 'autostart_state':
      return { enabled: false, hidden: false };
    case 'update_engine':
      await wait(1200);
      return 'mock';
    case 'launch_hidden':
      return false;
    case 'platform':
      return location.search.includes('mobile') ? { os: 'android', mobile: true } : null;
    default:
      return null;
  }
}
