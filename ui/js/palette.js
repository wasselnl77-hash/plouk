// Pulls a mood palette out of album art: one readable accent color plus four
// darker "light source" colors for the ambient background.

const cache = new Map();

export const DEFAULT_PALETTE = {
  accent: [180, 162, 255],
  glow: [
    [0.36, 0.27, 0.78],
    [0.1, 0.42, 0.55],
    [0.5, 0.18, 0.5],
    [0.18, 0.22, 0.6],
  ],
};

function rgbToHsl(r, g, b) {
  r /= 255; g /= 255; b /= 255;
  const max = Math.max(r, g, b), min = Math.min(r, g, b);
  const l = (max + min) / 2;
  if (max === min) return [0, 0, l];
  const d = max - min;
  const s = l > 0.5 ? d / (2 - max - min) : d / (max + min);
  let h;
  if (max === r) h = (g - b) / d + (g < b ? 6 : 0);
  else if (max === g) h = (b - r) / d + 2;
  else h = (r - g) / d + 4;
  return [h / 6, s, l];
}

function hslToRgb(h, s, l) {
  if (s === 0) return [l * 255, l * 255, l * 255];
  const q = l < 0.5 ? l * (1 + s) : l + s - l * s;
  const p = 2 * l - q;
  const f = (t) => {
    if (t < 0) t += 1;
    if (t > 1) t -= 1;
    if (t < 1 / 6) return p + (q - p) * 6 * t;
    if (t < 1 / 2) return q;
    if (t < 2 / 3) return p + (q - p) * (2 / 3 - t) * 6;
    return p;
  };
  return [f(h + 1 / 3) * 255, f(h) * 255, f(h - 1 / 3) * 255];
}

function loadImage(url) {
  return new Promise((resolve, reject) => {
    const img = new Image();
    img.crossOrigin = 'anonymous';
    img.decoding = 'async';
    img.onload = () => resolve(img);
    img.onerror = reject;
    img.src = url;
  });
}

export async function extractPalette(url) {
  if (!url) return DEFAULT_PALETTE;
  if (cache.has(url)) return cache.get(url);
  try {
    const img = await loadImage(url);
    const n = 40;
    const c = document.createElement('canvas');
    c.width = c.height = n;
    const ctx = c.getContext('2d', { willReadFrequently: true });
    ctx.drawImage(img, 0, 0, n, n);
    const data = ctx.getImageData(0, 0, n, n).data;

    // Bucket by hue, weighting vivid mid-tones the most.
    const buckets = Array.from({ length: 18 }, () => ({ w: 0, r: 0, g: 0, b: 0, s: 0 }));
    let vivid = 0;
    for (let i = 0; i < data.length; i += 4) {
      const r = data[i], g = data[i + 1], b = data[i + 2];
      const [h, s, l] = rgbToHsl(r, g, b);
      const w = s * s * (1 - Math.abs(l - 0.5) * 1.6) + 0.002;
      if (w <= 0) continue;
      const k = buckets[Math.floor(h * 18) % 18];
      k.w += w; k.r += r * w; k.g += g * w; k.b += b * w; k.s += s * w;
      vivid += s;
    }
    const total = buckets.reduce((a, k) => a + k.w, 0) || 1;
    const ranked = buckets
      .filter((k) => k.w > total * 0.03)
      .sort((a, b) => b.w - a.w)
      .map((k) => [k.r / k.w, k.g / k.w, k.b / k.w]);
    const colorful = vivid / (n * n) > 0.12 && ranked.length > 0;

    let accent, glow;
    if (!colorful) {
      accent = [226, 226, 236];
      glow = [[0.3, 0.3, 0.36], [0.2, 0.22, 0.3], [0.26, 0.26, 0.3], [0.16, 0.17, 0.22]];
    } else {
      const [h, s] = rgbToHsl(...ranked[0]);
      accent = hslToRgb(h, Math.min(0.9, Math.max(0.55, s)), 0.7);
      const base = ranked.slice(0, 4);
      while (base.length < 4) {
        const [bh, bs, bl] = rgbToHsl(...base[base.length - 1]);
        base.push(hslToRgb((bh + 0.07 * base.length) % 1, bs, bl));
      }
      glow = base.map((rgb) => {
        const [gh, gs] = rgbToHsl(...rgb);
        return hslToRgb(gh, Math.min(0.85, gs * 1.1 + 0.1), 0.42).map((v) => v / 255);
      });
    }
    const out = { accent: accent.map(Math.round), glow };
    cache.set(url, out);
    return out;
  } catch {
    return DEFAULT_PALETTE;
  }
}
