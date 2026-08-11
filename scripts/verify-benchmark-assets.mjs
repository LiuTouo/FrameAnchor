// 驗證基準測試內建資源：
//   1) SHA256SUMS 內每個固定 hash 的檔案必須存在且 sha256 相符。
//   2) d3d9-workload.exe（自行編譯）必須存在。
// 任一不符 → exit 1（build 前後呼叫，確保 installer/portable 打包的是正確工具）。
import { createHash } from 'node:crypto';
import { existsSync, readFileSync } from 'node:fs';
import path from 'node:path';

const DIR = 'src-tauri/resources/benchmark';
const MANIFEST = path.join(DIR, 'SHA256SUMS');

let failed = false;

const manifest = readFileSync(MANIFEST, 'utf8');
for (const line of manifest.split('\n')) {
  const t = line.trim();
  if (!t || t.startsWith('#')) continue;
  const [hash, ...rest] = t.split(/\s+/);
  const file = rest.join(' ');
  if (!/^[0-9a-f]{64}$/.test(hash)) {
    console.error(`[FAIL] SHA256SUMS hash 格式錯誤: ${hash}`);
    failed = true;
    continue;
  }
  const p = path.join(DIR, file);
  if (!existsSync(p)) {
    console.error(`[FAIL] 資源缺失: ${p}（請先執行 npm run fetch:benchmark-assets）`);
    failed = true;
    continue;
  }
  const got = createHash('sha256').update(readFileSync(p)).digest('hex');
  if (got !== hash) {
    console.error(`[FAIL] 資源 hash 不符: ${p}\n  want=${hash}\n  got =${got}`);
    failed = true;
    continue;
  }
  console.log(`[OK] ${file}`);
}

const d3d9 = path.join(DIR, 'd3d9-workload.exe');
if (!existsSync(d3d9)) {
  console.error(`[FAIL] 資源缺失: ${d3d9}（請先執行 npm run build:benchmark-assets）`);
  failed = true;
}

if (failed) {
  console.error('基準測試資源驗證失敗');
  process.exit(1);
}
console.log('基準測試資源驗證通過');
