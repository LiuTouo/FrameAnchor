import type { AffinitySpec, Recommendation, Topology } from './types';

/// 遮罩位元組（LE）→ 單一 LP；多 bit 或空 → null
export function maskToLp(bytes: number[] | null): number | null {
  if (!bytes || bytes.length === 0) return null;
  let v = 0n;
  for (let i = 0; i < bytes.length; i++) v |= BigInt(bytes[i]) << BigInt(i * 8);
  let found: number | null = null;
  for (let i = 0; i < 64; i++) {
    if ((v & (1n << BigInt(i))) !== 0n) {
      if (found !== null) return null;
      found = i;
    }
  }
  return found;
}

/// 依 recommendation 的 best/severe LP 計算標註集合（純顯示，不重算排除邏輯）：
/// best / severe 直接來自 metadata；excluded = 全部支援 LP − recommendedCores。
/// severe 標註不代表會從 CPU 親和性推薦中排除。
export interface RecAnnotations {
  best: Set<number>;
  severe: Set<number>;
  recommended: Set<number>;
  excluded: Set<number>;
  has: boolean;
}

export function recommendationAnnotations(
  topo: Topology,
  rec: Recommendation | null | undefined,
): RecAnnotations {
  if (!rec) {
    return { best: new Set(), severe: new Set(), recommended: new Set(), excluded: new Set(), has: false };
  }
  const best = new Set<number>([rec.bestLp].filter((x): x is number => x != null));
  const severe = new Set<number>(rec.severeLps ?? []);
  const recommended = new Set<number>(rec.recommendedCores ?? []);
  const all = new Set(topo.logicalProcessors.map((lp) => lp.index));
  const excluded = new Set<number>([...all].filter((i) => !recommended.has(i)));
  return { best, severe, recommended, excluded, has: best.size > 0 || severe.size > 0 };
}

/// 推薦 metadata 的來源時間：優先取來源 session 的 finishedAt，退回 startedAt。
/// generatedAt 必須反映來源基準測試的時間，不是匯入當下。
export function recommendationSourceTime(session: {
  finishedAt: string | null;
  startedAt: string;
}): string {
  return session.finishedAt ?? session.startedAt;
}

/// 調整語意：任何使用者觸發的親和性改動 → adjusted 維持 true 且不清除
/// （即使之後手動回到精確的推薦 Custom 集合也一樣）。只有重匯入（呼叫端直接
/// 設 adjusted=false）會重置。此函式把「一旦變動即永久黏滯」的規則寫成顯式純函式。
export function nextAdjustedAfterManualChange(_prev: boolean): boolean {
  return true;
}

/// affinity 模式 → LP index 集合（前端版 resolve_mask，用於面板涵蓋高亮與勾選格狀態）
export function resolveCores(
  spec: Pick<AffinitySpec, 'mode' | 'cores'>,
  topo: Topology,
): Set<number> {
  switch (spec.mode) {
    case 'All':
      return new Set(topo.logicalProcessors.map((lp) => lp.index));
    case 'NoSmtSibling':
      return new Set(topo.logicalProcessors.filter((lp) => !lp.isSmtSibling).map((lp) => lp.index));
    case 'PCoresOnly': {
      const pCores = new Set(topo.physicalCores.filter((c) => c.isPCore).map((c) => c.id));
      return new Set(topo.logicalProcessors.filter((lp) => pCores.has(lp.coreId)).map((lp) => lp.index));
    }
    case 'Custom':
    case 'Prefer':
      return new Set(spec.cores.filter((i) => i < topo.totalLp));
  }
}

/// 偵測目前勾選集合是否恰好等於某 preset（用於 preset 按鈕高亮）
export function detectMode(cores: Set<number>, topo: Topology): AffinitySpec['mode'] {
  const eq = (a: Set<number>, b: Set<number>) => a.size === b.size && [...a].every((x) => b.has(x));
  if (eq(cores, resolveCores({ mode: 'All', cores: [] }, topo))) return 'All';
  if (eq(cores, resolveCores({ mode: 'NoSmtSibling', cores: [] }, topo))) return 'NoSmtSibling';
  if (eq(cores, resolveCores({ mode: 'PCoresOnly', cores: [] }, topo))) return 'PCoresOnly';
  return 'Custom';
}
