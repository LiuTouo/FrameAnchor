// 重新取得基準測試內建資源並更新 SHA256SUMS：
//   - PresentMon-2.5.1-x64.exe（GameTechDev/PresentMon，MIT）
//   - lava-triangle.exe + LICENSE（valleyofdoom/AutoGpuAffinity 1.0.0 內附的
//     liblava Vulkan workload，MIT）
// 資產已 vendor 在 git 內；此 script 供重新取得/校驗 manifest。
import { createHash } from 'node:crypto';
import { existsSync, mkdirSync, readFileSync, writeFileSync, rmSync } from 'node:fs';
import path from 'node:path';
import { spawnSync } from 'node:child_process';

const DIR = 'src-tauri/resources/benchmark';
const TMP = 'scripts/.assets-tmp';
mkdirSync(TMP, { recursive: true });

const PRESENTMON_URL =
  'https://github.com/GameTechDev/PresentMon/releases/download/v2.5.1/PresentMon-2.5.1-x64.exe';
const PRESENTMON_LICENSE_URL =
  'https://raw.githubusercontent.com/GameTechDev/PresentMon/v2.5.1/LICENSE.txt';
const AGA_ZIP_URL =
  'https://github.com/valleyofdoom/AutoGpuAffinity/releases/download/1.0.0/AutoGpuAffinity.zip';

function sha256(file) {
  return createHash('sha256').update(readFileSync(file)).digest('hex');
}

function download(url, dest) {
  console.log(`下載 ${url}`);
  const res = spawnSync('curl', ['-sL', '--max-time', '180', '-o', dest, url], { stdio: 'inherit' });
  if (res.status !== 0 || !existsSync(dest)) {
    throw new Error(`下載失敗: ${url}`);
  }
}

try {
  const pm = path.join(TMP, 'PresentMon-2.5.1-x64.exe');
  download(PRESENTMON_URL, pm);
  const pmLic = path.join(TMP, 'LICENSE-PresentMon.txt');
  download(PRESENTMON_LICENSE_URL, pmLic);

  const zip = path.join(TMP, 'aga.zip');
  download(AGA_ZIP_URL, zip);
  const ext = path.join(TMP, 'aga_ext');
  rmSync(ext, { recursive: true, force: true });
  mkdirSync(ext, { recursive: true });
  const unzip = spawnSync('powershell', ['-NoProfile', '-Command', `Expand-Archive -Path '${path.resolve(zip)}' -DestinationPath '${path.resolve(ext)}' -Force`], { stdio: 'inherit' });
  if (unzip.status !== 0) throw new Error('解壓 AutoGpuAffinity.zip 失敗');
  const lava = path.join(ext, 'AutoGpuAffinity/bin/liblava/lava-triangle.exe');
  if (!existsSync(lava)) throw new Error(`找不到 ${lava}`);
  const lavaLic = path.join(ext, 'AutoGpuAffinity/bin/liblava/LICENSE.txt');

  mkdirSync(DIR, { recursive: true });
  const copy = (src, dst) => {
    writeFileSync(dst, readFileSync(src));
    console.log(`[OK] ${dst}`);
  };
  copy(pm, path.join(DIR, 'PresentMon-2.5.1-x64.exe'));
  copy(lava, path.join(DIR, 'lava-triangle.exe'));
  if (existsSync(lavaLic)) copy(lavaLic, path.join(DIR, 'LICENSE-liblava.txt'));
  copy(pmLic, path.join(DIR, 'LICENSE-PresentMon.txt'));
  rmSync(path.join(DIR, 'PresentMon-1.10.0-x64.exe'), { force: true });

  const manifest = [
    '# FrameAnchor GPU 基準測試內建資源的固定 SHA-256。',
    '# 驗證：npm run verify:benchmark-assets；執行基準測試前也會驗證。',
    '# 來源：',
    '#   PresentMon-2.5.1-x64.exe — https://github.com/GameTechDev/PresentMon/releases/tag/v2.5.1（MIT）',
    '#   lava-triangle.exe        — valleyofdoom/AutoGpuAffinity 1.0.0 內附的 liblava Vulkan workload（MIT）',
    `${sha256(path.join(DIR, 'PresentMon-2.5.1-x64.exe'))}  PresentMon-2.5.1-x64.exe`,
    `${sha256(path.join(DIR, 'lava-triangle.exe'))}  lava-triangle.exe`,
    '',
  ].join('\n');
  writeFileSync(path.join(DIR, 'SHA256SUMS'), manifest);
  console.log('SHA256SUMS 已更新');
} catch (e) {
  console.error(`[FAIL] ${e.message}`);
  process.exit(1);
} finally {
  rmSync(TMP, { recursive: true, force: true });
}
