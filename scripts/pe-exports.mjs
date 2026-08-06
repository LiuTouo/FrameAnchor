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
const cstr = (o) => {
  let end = o;
  while (buf[end] !== 0) end++;
  return buf.toString('ascii', o, end);
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
