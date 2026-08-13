// 驗證基準測試內建資源：
//   1) SHA256SUMS 內每個固定 hash 的檔案必須存在且 sha256 相符。
//   2) d3d9-workload.exe（自行編譯）必須存在。
//   3) lava-triangle.exe 內嵌 manifest 必須宣告 PerMonitorV2 DPI 感知。
// 任一不符 → exit 1（build 前後呼叫，確保 installer/portable 打包的是正確工具）。
import { createHash } from 'node:crypto';
import { existsSync, mkdtempSync, readdirSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';

const DIR = 'src-tauri/resources/benchmark';
const SUMS = path.join(DIR, 'SHA256SUMS');
const LAVA = path.join(DIR, 'lava-triangle.exe');
const D3D9 = path.join(DIR, 'd3d9-workload.exe');

let failed = false;

// ── SHA256 驗證 ────────────────────────────────────────────────
const manifest = readFileSync(SUMS, 'utf8');
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

// ── d3d9-workload.exe ──────────────────────────────────────────
if (!existsSync(D3D9)) {
  console.error(`[FAIL] 資源缺失: ${D3D9}（請先執行 npm run build:benchmark-assets）`);
  failed = true;
}

// ── lava-triangle.exe DPI manifest 驗證 ────────────────────────
/** 在 Windows Kits 10 安裝中尋找 x64 mt.exe，取最高 SDK 版本。 */
function findMtExe() {
  const base = 'C:\\Program Files (x86)\\Windows Kits\\10\\bin';
  if (!existsSync(base)) return null;
  const dirs = readdirSync(base, { withFileTypes: true })
    .filter(d => d.isDirectory() && /^\d+\.\d+\.\d+\.\d+$/.test(d.name))
    .map(d => d.name)
    .sort()
    .reverse();
  for (const ver of dirs) {
    const mt = path.join(base, ver, 'x64', 'mt.exe');
    if (existsSync(mt)) return mt;
  }
  return null;
}

if (existsSync(LAVA)) {
  const mt = findMtExe();
  if (!mt) {
    console.error('[FAIL] 找不到 Windows SDK mt.exe，無法驗證 lava-triangle.exe DPI manifest');
    failed = true;
  } else {
    const tmp = mkdtempSync(path.join(tmpdir(), 'fa-verify-'));
    const xml = path.join(tmp, 'manifest.xml');
    try {
      const res = spawnSync(mt, ['-inputresource:' + LAVA, '-out:' + xml], { stdio: 'pipe' });
      if (res.status !== 0 || !existsSync(xml)) {
        console.error(`[FAIL] 無法從 lava-triangle.exe 提取 manifest（mt.exe exit=${res.status}）`);
        if (res.stderr?.length) console.error(res.stderr.toString());
        failed = true;
      } else {
        const text = readFileSync(xml, 'utf8');
        if (!text.includes('PerMonitorV2')) {
          console.error('[FAIL] lava-triangle.exe 內嵌 manifest 缺少 PerMonitorV2 DPI 宣告');
          failed = true;
        } else {
          console.log('[OK] lava-triangle.exe DPI manifest (PerMonitorV2)');
        }
      }
    } finally {
      rmSync(tmp, { recursive: true, force: true });
    }
  }
}

// ── 結果 ────────────────────────────────────────────────────────
if (failed) {
  console.error('基準測試資源驗證失敗');
  process.exit(1);
}
console.log('基準測試資源驗證通過');
