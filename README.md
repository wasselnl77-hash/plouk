<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/brand/ehs-logo-dark.svg">
    <img src="docs/brand/ehs-logo-light.svg" alt="EHS studio" width="150">
  </picture>
</p>

<h1 align="center">Plouk</h1>

<p align="center">
  A little record player for your Linux desktop.<br>
  Drag a playlist out of Spotify, drop it on the window, and it plays.
</p>

<p align="center">
  <img src="docs/screenshots/plouk.png" alt="Plouk playing Daft Punk's Discovery" width="880">
</p>

We made Plouk because music apps started to feel like spreadsheets. We wanted
something closer to putting on a record: you pick something, the needle drops,
and the rest of the screen gets out of the way.

So that's what it is. A turntable, a queue, and not much else.

## What it does

- **Drop anything from Spotify.** Playlists, albums, one song or a handful of
  them. Drag them straight out of the Spotify app, paste a link, or run
  `plouk <link>`. You don't need a Spotify account or an API key.
- **It behaves like a turntable.** The tonearm is the progress bar. It creeps
  inward as the song plays, and you can grab it to seek. Changing songs swaps
  the record: the arm lifts, the old record slides away, the new one comes out
  of its sleeve, and the music starts when the needle lands. Pausing spins it
  down.
- **The colors come from the album.** The glow behind the frosted glass picks
  up the cover's colors and breathes a little with the bass.
- **Even volume.** Every song gets measured once and played at the same
  loudness, so a quiet acoustic track and a loud pop single sit nicely next to
  each other.
- **The small stuff you'd expect.** Synced lyrics, crossfade, a shelf with
  everything you've dropped in, media keys, a tray icon, and it remembers where
  you left off.

<p align="center">
  <img src="docs/screenshots/options.png" alt="The options panel" width="49%">
  <img src="docs/screenshots/menus.png" alt="The Playback menu" width="49%">
</p>

Opening the options bends the whole window out of the way with a radial blur,
then puts everything back when you close them. It's a silly amount of effort
for a settings screen and we're not sorry.

<p align="center">
  <img src="docs/screenshots/warp.png" alt="The window warping as the options open" width="49%">
  <img src="docs/screenshots/first-launch.png" alt="Plouk on first launch, waiting for a playlist" width="49%">
</p>

## Getting it

Grab the latest build from the [releases page](../../releases), unpack it and
run the installer:

```bash
tar xzf plouk-0.1.0-linux-x86_64.tar.gz
cd plouk-0.1.0
./install.sh
```

That puts Plouk in your app menu. If you'd rather not install anything, run
`./dist/plouk` from the same folder.

Plouk needs WebKitGTK 4.1 (`libwebkit2gtk-4.1-0`), which most recent distros
already have. The tray icon uses `libayatana-appindicator3-1`.

The first time you play something, Plouk downloads its own copy of
[yt-dlp](https://github.com/yt-dlp/yt-dlp) (about 40 MB) from the official
releases and keeps it up to date by itself.

## How it works

A drag out of Spotify only carries links, never audio. Plouk reads the song
list from Spotify's public embed page, finds each song on YouTube Music with
yt-dlp, and plays just the audio stream, so there are no ads. Songs are kept
after the first play, and the next two get fetched ahead of time, so skipping
is instant.

It's written in Rust with [Tauri](https://tauri.app). The audio engine
(decoding, crossfades, loudness), the Spotify and YouTube parts and the system
integration live in `src-tauri/src`. The interface is plain HTML, CSS and
JavaScript with no build step, in `ui/`.

## Keyboard

| Key | What it does |
| --- | --- |
| Space | Play / pause |
| ← → | Seek 5 seconds |
| Shift + ← → | Previous / next song |
| ↑ ↓ | Volume |
| M | Mute |
| Q / L | Queue / lyrics |
| Ctrl + V | Play a copied Spotify link |
| Ctrl + , | Options |
| F10 | Open the menus |
| Ctrl + W / Ctrl + Q | Hide to tray / quit |

## Good to know

- Spotify's public page only shows the first 100 songs of a playlist, and
  private playlists can't be read.
- We build and test Plouk on Linux Mint (Cinnamon, X11). Other desktops should
  be fine. On Wayland the options animation falls back to a simpler version.
- The matching usually lands on the official audio, but now and then a live
  version sneaks in.
- Plouk is for listening at home. If you love an artist, go support them where
  it counts.

## Building it yourself

You'll need Rust and the usual Tauri libraries for Linux (webkit2gtk-4.1,
libayatana-appindicator3, plus the ALSA and D-Bus dev packages).

```bash
cd src-tauri
cargo build --release
```

The binary ends up in `src-tauri/target/release/plouk`. Copy it to
`dist/plouk` and `./install.sh` will pick it up.

Want to fiddle with the design without the backend? Serve `ui/` with any
static file server and open it in a browser. It falls back to a fake player
with a few demo songs.

## License

MIT. Do what you like with it, see [LICENSE](LICENSE).

<p align="center"><sub>Made by EHS studio.</sub></p>
