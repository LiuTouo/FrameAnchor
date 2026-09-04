// 極簡 PE export table 名稱列表（診斷用）
// 用法: node scripts/pe-exports.mjs <dll-path> [name-filter]
import { readFileSync } from 'node:fs';

const buf = readFileSync(process.argv[2]);
const filter = process.argv[3];
const u16 = (o) => buf.readUInt16LE(o);
const u32 = (o) => buf.readUInt32LE(o);

const peOff = u32(0x3c);
const optOff = peOff + 24;
const magic = u16(optOff);
const isPe32Plus = magic === 0x20b;
const numSections = u16(peOff + 6);
const optSize = u16(peOff + 20);
// DataDirectory[0] = Export Table；PE32 從 +96、PE32+ 從 +112 開始
const exportRva = u32(optOff + (isPe32Plus ? 112 : 96));

const sections = [];
const secOff = optOff + optSize;
for (let i = 0; i < numSections; i++) {
  const o = secOff + i * 40;
  sections.push({ vsize: u32(o + 8), vaddr: u32(o + 12), rawOff: u32(o + 20) });
}
const rva2off = (rva) => {
  const s = sections.find((s) => rva >= s.vaddr && rva < s.vaddr + s.vsize);
  if (!s) throw new Error(`RVA ${rva.toString(16)} not in any section`);
  return s.rawOff + (rva - s.vaddr);
};
// 名稱中的控制字元（ESC/CSI/CR/BS 等）先中和成可見轉義，避免 terminal 解讀
const escapeCtl = (s) =>
  s.replace(/[\x00-\x1f\x7f]/g, (c) => `\\x${c.charCodeAt(0).toString(16).padStart(2, '0')}`);
// 以 buffer 為界掃描 C 字串；缺 NUL 或超過上限視為 invalid，
// 避免 `buf[end]` 越界回 undefined 造成無限迴圈
const cstr = (o) => {
  const nul = buf.indexOf(0, o);
  if (nul === -1 || nul - o > 512) return '<invalid-name>';
  return escapeCtl(buf.toString('ascii', o, nul));
};

const expOff = rva2off(exportRva);
const numNames = u32(expOff + 0x18);
const namesRva = u32(expOff + 0x20);
const namesOff = rva2off(namesRva);
const names = [];
for (let i = 0; i < numNames; i++) {
  names.push(cstr(rva2off(u32(namesOff + i * 4))));
}
const out = filter ? names.filter((n) => n.includes(filter)) : names;
console.log(out.join('\n'));
console.error(`total: ${names.length}${filter ? `, matched: ${out.length}` : ''}`);
