import type { AffinitySpec, Topology } from './types';

/// affinity 模式 → LP index 集合（前端版 resolve_mask，用於面板涵蓋高亮與勾選格狀態）
export function resolveCores(spec: AffinitySpec, topo: Topology): Set<number> {
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
