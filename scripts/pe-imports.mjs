// 極簡 PE import table 解析器（診斷 0xC0000139 用）
// 用法: node scripts/pe-imports.mjs <exe-path>
import { readFileSync } from 'node:fs';

const buf = readFileSync(process.argv[2]);
const u16 = (o) => buf.readUInt16LE(o);
const u32 = (o) => buf.readUInt32LE(o);
const u64 = (o) => buf.readBigUInt64LE(o);

const peOff = u32(0x3c);
if (buf.toString('ascii', peOff, peOff + 4) !== 'PE\0\0') throw new Error('not PE');
const numSections = u16(peOff + 6);
const optSize = u16(peOff + 20);
const optOff = peOff + 24;
const magic = u16(optOff);
if (magic !== 0x20b) throw new Error('not PE32+');
// DataDirectory[1] = Import Table（PE32+ 目錄從 +112 開始）
const importRva = u32(optOff + 112 + 8);

const sections = [];
const secOff = optOff + optSize;
for (let i = 0; i < numSections; i++) {
  const o = secOff + i * 40;
  sections.push({
    name: buf.toString('ascii', o, o + 8).replace(/\0.*$/, ''),
    vsize: u32(o + 8),
    vaddr: u32(o + 12),
    rawOff: u32(o + 20),
  });
}
const rva2off = (rva) => {
  const s = sections.find((s) => rva >= s.vaddr && rva < s.vaddr + Math.max(s.vsize, 1));
  if (!s) throw new Error(`RVA ${rva.toString(16)} not in any section`);
  return s.rawOff + (rva - s.vaddr);
};
const cstr = (o) => {
  let end = o;
  while (buf[end] !== 0) end++;
  return buf.toString('ascii', o, end);
};

let descOff = rva2off(importRva);
while (true) {
  const iltRva = u32(descOff);
  const nameRva = u32(descOff + 12);
  if (iltRva === 0 && nameRva === 0) break;
  const dll = cstr(rva2off(nameRva));
  const funcs = [];
  let thunkOff = rva2off(iltRva);
  while (true) {
    const t = u64(thunkOff);
    if (t === 0n) break;
    if ((t & 0x8000000000000000n) === 0n) {
      funcs.push(cstr(rva2off(Number(t)) + 2)); // 跳過 hint
    } else {
      funcs.push(`#ord${t & 0xffffn}`);
    }
    thunkOff += 8;
  }
  console.log(`\n${dll} (${funcs.length})`);
  console.log('  ' + funcs.join(', '));
  descOff += 20;
}
