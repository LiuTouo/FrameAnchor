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
  recommendation?: Recommendation; // GPU 基準測試推薦元資料（可選，舊 config 可能沒有）
}

/** GPU 基準測試推薦元資料（Rule 的證據欄位） */
export interface Recommendation {
  sessionId: string | null;
  generatedAt: string | null;
  cpuFingerprint: string | null;
  gpuInstanceId: string | null;
  bestLp: number | null;
  severeLps: number[];
  recommendedCores: number[];
  adjusted: boolean;
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

// ── 更新相關型別 ──

export type UpdateStatus =
  | 'Idle'
  | 'Checking'
  | 'UpToDate'
  | 'Available'
  | 'Downloading'
  | 'Installing'
  | 'Error';

export interface UpdateState {
  status: UpdateStatus;
  latestVersion: string | null;
  currentVersion: string;
  progress: number | null; // 0..100，僅 Downloading 有意義
  error: string | null;
}

export interface UpdateInfo {
  version: string;
  portable: boolean;
}

// ── GPU 基準測試相關型別 ──

export type SessionStatus = 'Pending' | 'Running' | 'Completed' | 'Failed' | 'Cancelled';
export type BenchmarkStage = 'Init' | 'Warmup' | 'Collecting' | 'Finalizing';
export type WorkloadKind = 'Vulkan' | 'D3D9';

export interface GpuDevice {
  instanceId: string; // 穩定 PnP 身分
  friendlyName: string;
}

/** 基準測試參數 */
export interface BenchmarkConfig {
  candidateLps: number[]; // 要逐一測試的候選 LP；空 = 全部支援 LP
  gpuInstanceId: string | null;
  workload: WorkloadKind;
  warmUpSecs: number; // 預設 5
  sampleSecs: number; // 預設 30
  repetitions: number; // 1..3，預設 1；round 順序 asc/desc/asc
  syncWorkloadAffinity: boolean; // 已棄用；固定 false。保留供舊 session 向後相容。
  fullscreen: boolean; // 預設 true
  width: number; // 640
  height: number; // 480
  fpsCap: number; // 0 = 不限
  tripleBuffer: boolean;
  vulkanArgs: string[]; // workload=Vulkan 時必須非空
  workloadExePath: string | null; // 覆寫（測試/除錯）
  presentmonPath: string | null; // 覆寫（測試/除錯）
  gamePath: string | null; // 相容舊欄位
  windowTitle: string | null;
}

/** 單一候選 LP 的最終測試結果 */
export interface LpResult {
  lp: number;
  avgFps: number | null;
  maxFps: number | null;
  minFps: number | null;
  stdevFps: number | null; // Bessel（n-1）
  p1Low: number | null; // 1% low（time-weighted）
  p01Low: number | null; // 0.1% low
  p001Low: number | null; // 0.01% low
  p0005Low: number | null; // 0.005% low
  sampleCount: number;
  avgFrameTimeMs: number | null;
  completed: boolean;
  error: string | null; // 錯誤代碼，查 i18n errors.*
}

/** 執行期 progress 事件（`gpu-benchmark-progress`） */
export interface BenchmarkProgress {
  sessionId: string;
  stage: string; // starting/applying/launching/collecting/collected/finalizing
  round: number | null;
  lp: number | null;
  percentage: number;
  etaSecs: number | null;
  error: string | null;
}

/** 執行期間的原始取樣 */
export interface CoreSample {
  lp: number;
  fps: number;
  frameTimeMs: number;
}

export interface SessionSummary {
  id: string;
  status: SessionStatus;
  startedAt: string;
  finishedAt: string | null;
  gpuName: string;
  gpuInstanceId: string;
  cpuFingerprint: string;
  bestLp: number | null;
  severeLps: number[]; // 嚴重 LP（後端判定）
  sampleCount: number;
  totalBytes: number; // 整個 session 資料夾位元組數（即時計算）
  config: BenchmarkConfig;
  error: string | null; // 終結失敗原因（i18n errors.* 代碼）；成功/取消為 null
}

/** 歷史 session 的「可否套用」狀態（相容性判定只在後端） */
export interface ApplyStatus {
  canApply: boolean;
  reason: string | null; // null = 可套用；否則為 i18n errors.* 代碼
}

export interface SessionDetail {
  summary: SessionSummary;
  results: LpResult[];
  samples: CoreSample[];
}

/** 執行期狀態（get_benchmark_state） */
export interface BenchmarkState {
  status: SessionStatus;
  sessionId: string | null;
  currentLp: number | null;
  stage: BenchmarkStage;
  progressPct: number;
  elapsedSecs: number;
  cancelRequested: boolean;
  recoveryRequired: boolean; // 啟動還原失敗 → 封鎖 test/apply
}

/** 單一註冊表值的精確快照（presence + 型別 + 原始位元組） */
export interface RegistryValueSnapshot {
  present: boolean;
  valueType: number | null; // 原生 REG_VALUE_TYPE 數值（REG_DWORD=4、REG_BINARY=3 …）
  bytes: number[] | null; // 原始位元組（little-endian）
}

/** GPU 中斷親和性策略（DevicePolicy + AssignmentSetOverride） */
export interface AffinityPolicy {
  instanceId: string;
  devicePolicy: RegistryValueSnapshot;
  assignmentSetOverride: RegistryValueSnapshot;
}

export interface StorageInfo {
  totalBytes: number;
  sessionCount: number;
}
