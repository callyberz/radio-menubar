#!/usr/bin/env node
// Generates the menu bar (tray) template images and the app icon source.
// No dependencies: a tiny PNG encoder using node:zlib.
//
//   node scripts/gen-icons.mjs
//
// Output:
//   src-tauri/icons/tray/bars-paused.png      static, all bars at 35%
//   src-tauri/icons/tray/bars-<n>.png         animation frames (36x36 px = 18pt @2x)
//   src-tauri/icons/tray/*@1x.png             18x18 px variants (reference only)
//   src-tauri/icons/app-icon.png              1024x1024 source for `pnpm tauri icon`

import { deflateSync } from "node:zlib";
import { mkdirSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const trayDir = join(root, "src-tauri", "icons", "tray");
mkdirSync(trayDir, { recursive: true });

// ---------- PNG encoder ----------
const CRC_TABLE = new Uint32Array(256).map((_, n) => {
  let c = n;
  for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
  return c >>> 0;
});
function crc32(buf) {
  let c = 0xffffffff;
  for (const b of buf) c = CRC_TABLE[(c ^ b) & 0xff] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
}
function chunk(type, data) {
  const len = Buffer.alloc(4);
  len.writeUInt32BE(data.length);
  const td = Buffer.concat([Buffer.from(type, "ascii"), data]);
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(crc32(td));
  return Buffer.concat([len, td, crc]);
}
/** rgba: Float32Array-ish of w*h*4 values in 0..1 (straight alpha) */
function encodePNG(w, h, rgba) {
  const raw = Buffer.alloc((w * 4 + 1) * h);
  for (let y = 0; y < h; y++) {
    raw[y * (w * 4 + 1)] = 0; // filter: none
    for (let x = 0; x < w * 4; x++) {
      raw[y * (w * 4 + 1) + 1 + x] = Math.round(Math.max(0, Math.min(1, rgba[y * w * 4 + x])) * 255);
    }
  }
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(w, 0);
  ihdr.writeUInt32BE(h, 4);
  ihdr[8] = 8; // bit depth
  ihdr[9] = 6; // RGBA
  return Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    chunk("IHDR", ihdr),
    chunk("IDAT", deflateSync(raw, { level: 9 })),
    chunk("IEND", Buffer.alloc(0)),
  ]);
}

// ---------- tiny rasteriser (supersampled rounded rects) ----------
function inRoundRect(px, py, r) {
  const { x, y, w, h, rad } = r;
  if (px < x || px > x + w || py < y || py > y + h) return false;
  const rr = Math.min(rad, w / 2, h / 2);
  const cx = Math.max(x + rr, Math.min(px, x + w - rr));
  const cy = Math.max(y + rr, Math.min(py, y + h - rr));
  return (px - cx) ** 2 + (py - cy) ** 2 <= rr * rr;
}
/** Coverage of a set of rects in a w*h canvas; units are px. */
function coverage(w, h, rects, ss = 8) {
  const out = new Float32Array(w * h);
  for (let y = 0; y < h; y++)
    for (let x = 0; x < w; x++) {
      let hit = 0;
      for (let sy = 0; sy < ss; sy++)
        for (let sx = 0; sx < ss; sx++) {
          const px = x + (sx + 0.5) / ss, py = y + (sy + 0.5) / ss;
          if (rects.some((r) => inRoundRect(px, py, r))) hit++;
        }
      out[y * w + x] = hit / (ss * ss);
    }
  return out;
}

// ---------- tray template frames ----------
// Spec: 3 bars, 2pt wide, 11pt tall, 2pt gap, bottom-aligned, 35%..100% height.
function barsImage(scale, heights) {
  const size = 18 * scale;
  const barW = 2 * scale, gap = 2 * scale, maxH = 11 * scale;
  const totalW = barW * 3 + gap * 2;
  const x0 = (size - totalW) / 2;
  const bottom = (size + maxH) / 2;
  const rects = heights.map((f, i) => {
    const hgt = maxH * f;
    return { x: x0 + i * (barW + gap), y: bottom - hgt, w: barW, h: hgt, rad: 0.75 * scale };
  });
  const cov = coverage(size, size, rects);
  const rgba = new Float32Array(size * size * 4);
  for (let i = 0; i < size * size; i++) rgba[i * 4 + 3] = cov[i]; // black, alpha = coverage
  return encodePNG(size, size, rgba);
}

const FRAMES = 24; // played at ~120 ms/frame => 2.88 s loop
const PERIODS = [8, 6, 12]; // frames per bounce cycle per bar (~0.96 s, 0.72 s, 1.44 s)
const PHASES = [0, 1, 2]; // stagger, like the prototype's 0.12 s offsets
const ease = (t) => 0.5 - 0.5 * Math.cos(2 * Math.PI * t); // 0 -> 1 -> 0, ease-in-out
for (let f = 0; f < FRAMES; f++) {
  const hs = PERIODS.map((p, i) => 0.35 + 0.65 * ease(((f + PHASES[i]) % p) / p));
  writeFileSync(join(trayDir, `bars-${f}.png`), barsImage(2, hs));
  if (f === 0) writeFileSync(join(trayDir, `bars-${f}@1x.png`), barsImage(1, hs));
}
writeFileSync(join(trayDir, "bars-paused.png"), barsImage(2, [0.35, 0.35, 0.35]));
writeFileSync(join(trayDir, "bars-paused@1x.png"), barsImage(1, [0.35, 0.35, 0.35]));

// ---------- app icon (1024x1024) ----------
{
  const S = 1024;
  const rgba = new Float32Array(S * S * 4);
  // macOS icon grid: 824px rounded square centred, radius ~185
  const bg = coverage(S, S, [{ x: 100, y: 100, w: 824, h: 824, rad: 185 }], 2);
  const barW = 96, gap = 72, heights = [0.55, 1, 0.75], maxH = 440;
  const x0 = (S - (barW * 3 + gap * 2)) / 2, bottom = S / 2 + maxH / 2;
  const bars = coverage(
    S, S,
    heights.map((f, i) => ({ x: x0 + i * (barW + gap), y: bottom - maxH * f, w: barW, h: maxH * f, rad: 36 })),
    2,
  );
  for (let y = 0; y < S; y++)
    for (let x = 0; x < S; x++) {
      const i = y * S + x, t = y / S;
      // vertical gradient: red (#E5342B) -> deeper red
      const br = 0.93 - 0.18 * t, bgc = 0.24 - 0.1 * t, bb = 0.2 - 0.08 * t;
      const a = bars[i];
      rgba[i * 4 + 0] = br * (1 - a) + a;
      rgba[i * 4 + 1] = bgc * (1 - a) + a;
      rgba[i * 4 + 2] = bb * (1 - a) + a;
      rgba[i * 4 + 3] = bg[i];
    }
  writeFileSync(join(root, "src-tauri", "icons", "app-icon.png"), encodePNG(S, S, rgba));
}

console.log(`wrote ${FRAMES} tray frames + paused frame to ${trayDir}, and app-icon.png`);
