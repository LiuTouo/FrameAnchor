// 與 PLAN §5 對應的 TS 型別。enum 字串值採 PascalCase（與 serde 序列化一致）。

export type MatchBy = 'FullPath' | 'FileName';
export type AffinityMode = 'All' | 'NoSmtSibling' | 'PCoresOnly' | 'Custom' | 'Prefer';
export type CpuPriority = 'Idle' | 'BelowNormal' | 'Normal' | 'AboveNormal' | 'High';
export type IoPriority = 'VeryLow' | 'Low' | 'Normal' | 'High';
export type MemPriority = 'VeryLow' | 'Low' | 'Medium' | 'BelowNormal' | 'Normal';

export interface AffinitySpec {
  mode: AffinityMode;
  cores: number[]; // LP indices，僅 mode=Custom 使用
}

export interface AdvancedSpec {
  ioPriority: IoPriority | null;
  memoryPriority: MemPriority | null;
}

export interface Rule {
  id: string;
  name: string;
  exePath: string;
  matchBy: MatchBy;
  enabled: boolean;
  affinity: AffinitySpec;
  priority: CpuPriority;
  advanced: AdvancedSpec;
}

export interface Settings {
  language: string; // 'zh-TW' | 'en'
  startWithWindows: boolean;
  startMinimized: boolean;
  closeToTray: boolean;
  pollIntervalMs: number;
  showAdvancedPriorities: boolean;
}

export interface LogicalProcessor {
  index: number;
  coreId: number;
  isSmtSibling: boolean;
  efficiencyClass: number;
}

export interface PhysicalCore {
  id: number;
  lpIndices: number[];
  efficiencyClass: number;
  isPCore: boolean;
}

export interface Topology {
  logicalProcessors: LogicalProcessor[];
  physicalCores: PhysicalCore[];
  hasSmt: boolean;
  hasHybrid: boolean;
  totalLp: number;
}

export interface AppliedProcess {
  pid: number;
  exeName: string;
  ruleId: string;
  ruleName: string;
  affinityOk: boolean;
  priorityOk: boolean;
  ioOk: boolean | null;
  memOk: boolean | null;
  error: string | null; // 錯誤代碼，查 i18n errors.*
  appliedAt: string;
  currentCores: number[];
  currentPriority: string;
  softAffinity: boolean; // true = 軟綁定，currentCores 為偏好清單
}

export interface WindowInfo {
  hwnd: number;
  pid: number;
  title: string;
  exeName: string;
  exePath: string | null;
  iconPng: string | null; // base64 PNG
  alreadyHasRule: boolean;
}
