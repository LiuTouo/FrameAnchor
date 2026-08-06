// 產生 FrameAnchor 圖示：icon.png (256) / 128x128.png / 32x32.png / icon.ico
// 無第三方依賴，手寫 PNG encoder + ICO 容器（PNG-compressed ICO，Vista+ 合法）。
// 設計：深色底 + 藍色錨點框（frame = 邊框，anchor = 中央錨點）。
import { deflateSync } from 'node:zlib';
import { writeFileSync, mkdirSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const outDir = join(dirname(fileURLToPath(import.meta.url)), '..', 'src-tauri', 'icons');
mkdirSync(outDir, { recursive: true });

// ---- CRC32 ----
const crcTable = new Uint32Array(256).map((_, n) => {
  let c = n;
  for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
  return c >>> 0;
});
const crc32 = (buf) => {
  let c = 0xffffffff;
  for (const b of buf) c = crcTable[(c ^ b) & 0xff] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
};

function chunk(type, data) {
  const len = Buffer.alloc(4);
  len.writeUInt32BE(data.length);
  const body = Buffer.concat([Buffer.from(type, 'ascii'), data]);
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(crc32(body));
  return Buffer.concat([len, body, crc]);
}

function encodePng(size, rgba) {
  const sig = Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]);
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(size, 0);
  ihdr.writeUInt32BE(size, 4);
  ihdr[8] = 8; // bit depth
  ihdr[9] = 6; // RGBA
  const raw = Buffer.alloc(size * (size * 4 + 1));
  for (let y = 0; y < size; y++) {
    raw[y * (size * 4 + 1)] = 0; // filter: none
    rgba.copy(raw, y * (size * 4 + 1) + 1, y * size * 4, (y + 1) * size * 4);
  }
  return Buffer.concat([sig, chunk('IHDR', ihdr), chunk('IDAT', deflateSync(raw, { level: 9 })), chunk('IEND', Buffer.alloc(0))]);
}

// 在 256 座標系畫圖，再依比例縮放取樣
function render(size) {
  const rgba = Buffer.alloc(size * size * 4);
  const S = 256;
  const px = (x, y, r, g, b, a) => {
    const sx = Math.floor((x / S) * size), sy = Math.floor((y / S) * size);
    if (sx < 0 || sy < 0 || sx >= size || sy >= size) return;
    const i = (sy * size + sx) * 4;
    rgba[i] = r; rgba[i + 1] = g; rgba[i + 2] = b; rgba[i + 3] = a;
  };
  const rect = (x0, y0, x1, y1, [r, g, b, a]) => {
    for (let y = y0; y < y1; y++) for (let x = x0; x < x1; x++) px(x, y, r, g, b, a);
  };
  const BG = [15, 23, 36, 255];       // #0F1724
  const ACCENT = [79, 140, 255, 255]; // #4F8CFF
  rect(0, 0, S, S, BG);
  // 邊框（frame）
  const m = 28, t = 20;
  rect(m, m, S - m, m + t, ACCENT);           // top
  rect(m, S - m - t, S - m, S - m, ACCENT);   // bottom
  rect(m, m, m + t, S - m, ACCENT);           // left
  rect(S - m - t, m, S - m, S - m, ACCENT);   // right
  // 中央錨點（anchor dot）
  const c = S / 2, rad = 26;
  for (let y = 0; y < S; y++) for (let x = 0; x < S; x++) {
    if ((x - c) ** 2 + (y - c) ** 2 <= rad * rad) px(x, y, ...ACCENT);
  }
  return rgba;
}

function encodeIco(pngBuf, size) {
  const header = Buffer.alloc(6);
  header.writeUInt16LE(0, 0); header.writeUInt16LE(1, 2); header.writeUInt16LE(1, 4);
  const entry = Buffer.alloc(16);
  entry[0] = size >= 256 ? 0 : size; // 256 以 0 表示
  entry[1] = size >= 256 ? 0 : size;
  entry.writeUInt16LE(1, 4);  // planes
  entry.writeUInt16LE(32, 6); // bpp
  entry.writeUInt32LE(pngBuf.length, 8);
  entry.writeUInt32LE(22, 12);
  return Buffer.concat([header, entry, pngBuf]);
}

for (const size of [256, 128, 32]) {
  const png = encodePng(size, render(size));
  const name = size === 256 ? 'icon.png' : `${size}x${size}.png`;
  writeFileSync(join(outDir, name), png);
  if (size === 256) writeFileSync(join(outDir, 'icon.ico'), encodeIco(png, size));
}
console.log('icons written to', outDir);
