import { invoke } from '@tauri-apps/api/core';
import type {
  AffinityPolicy,
  AppliedProcess,
  ApplyStatus,
  BenchmarkConfig,
  BenchmarkState,
  GpuDevice,
  Rule,
  SessionDetail,
  SessionSummary,
  Settings,
  StorageInfo,
  Topology,
  UpdateInfo,
  WindowInfo,
} from './types';

export const getTopology = () => invoke<Topology>('get_topology');
export const listWindows = () => invoke<WindowInfo[]>('list_windows');
export const getRules = () => invoke<Rule[]>('get_rules');
export const saveRule = (rule: Rule) => invoke<void>('save_rule', { rule });
export const deleteRule = (id: string) => invoke<void>('delete_rule', { id });
export const getSettings = () => invoke<Settings>('get_settings');
export const saveSettings = (settings: Settings) => invoke<void>('save_settings', { settings });
export const setAutostart = (enable: boolean) => invoke<void>('set_autostart', { enable });
export const getApplied = () => invoke<AppliedProcess[]>('get_applied');
export const reapplyAll = () => invoke<void>('reapply_all');
export const setUsageStreaming = (active: boolean) => invoke<void>('set_usage_streaming', { active });
export const openDataFolder = () => invoke<void>('open_data_folder');

// 更新相關
export const getUpdateInfo = () => invoke<UpdateInfo>('get_update_info');
export const checkPortableUpdate = () => invoke<void>('check_portable_update');
export const performPortableUpdate = () => invoke<void>('perform_portable_update');

// GPU 基準測試相關
export const enumerateGpus = () => invoke<GpuDevice[]>('enumerate_gpus');
export const getBenchmarkState = () => invoke<BenchmarkState>('get_benchmark_state');
export const listBenchmarkSessions = () => invoke<SessionSummary[]>('list_benchmark_sessions');
export const getBenchmarkSession = (id: string) =>
  invoke<SessionDetail>('get_benchmark_session', { id });
export const deleteBenchmarkSession = (id: string) =>
  invoke<void>('delete_benchmark_session', { id });
export const getBenchmarkStorageInfo = () => invoke<StorageInfo>('get_benchmark_storage_info');
export const getGpuAffinityPolicy = (instanceId: string) =>
  invoke<AffinityPolicy>('get_gpu_affinity_policy', { instanceId });
export const applyBestGpuAffinity = (sessionId: string) =>
  invoke<void>('apply_best_gpu_affinity', { sessionId });
export const getBenchmarkApplyStatus = (sessionId: string) =>
  invoke<ApplyStatus>('get_benchmark_apply_status', { sessionId });
export const listImportableSessions = () => invoke<SessionSummary[]>('list_importable_sessions');
export const computeRecommendedCores = (bestLp: number, severeLps: number[]) =>
  invoke<number[]>('compute_recommended_cores', { bestLp, severeLps });
export const getCurrentCpuFingerprint = () => invoke<string>('get_current_cpu_fingerprint');
export const restorePreviousGpuAffinity = () => invoke<void>('restore_previous_gpu_affinity');
export const applyGpuAffinity = (instanceId: string, lp: number) =>
  invoke<void>('apply_gpu_affinity', { instanceId, lp });
export const startGpuBenchmark = (config: BenchmarkConfig) =>
  invoke<void>('start_gpu_benchmark', { config });
export const cancelBenchmark = () => invoke<void>('cancel_benchmark');