// 編譯 D3D9 workload sidecar（Rust + windows Direct3D9 API）並複製到資源目錄。
// Tauri build（installer/portable）前必須先執行，否則 verify:benchmark-assets 失敗。
import { spawnSync } from 'node:child_process';
import { copyFileSync, existsSync, mkdirSync } from 'node:fs';
import path from 'node:path';

const SRC_MANIFEST = 'src-tauri/d3d9-workload/Cargo.toml';
const SRC_EXE = 'src-tauri/d3d9-workload/target/release/d3d9-workload.exe';
const DST_EXE = 'src-tauri/resources/benchmark/d3d9-workload.exe';

const res = spawnSync('cargo', ['build', '--release', '--manifest-path', SRC_MANIFEST], {
  stdio: 'inherit',
});
if (res.status !== 0) {
  console.error(`[FAIL] D3D9 workload 編譯失敗 (exit ${res.status})`);
  process.exit(res.status ?? 1);
}
if (!existsSync(SRC_EXE)) {
  console.error(`[FAIL] 找不到編譯產物: ${SRC_EXE}`);
  process.exit(1);
}
mkdirSync(path.dirname(DST_EXE), { recursive: true });
copyFileSync(SRC_EXE, DST_EXE);
console.log(`[OK] D3D9 workload 已複製: ${DST_EXE}`);
