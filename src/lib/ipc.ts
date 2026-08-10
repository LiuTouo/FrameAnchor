import { invoke } from '@tauri-apps/api/core';
import type { AppliedProcess, Rule, Settings, Topology, UpdateInfo, WindowInfo } from './types';

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