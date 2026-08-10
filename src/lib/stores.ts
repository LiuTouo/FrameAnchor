import { writable } from 'svelte/store';
import type { AppliedProcess, Rule, Settings, Topology, UpdateState } from './types';

export const topology = writable<Topology | null>(null);
export const rules = writable<Rule[]>([]);
export const settings = writable<Settings | null>(null);
export const applied = writable<AppliedProcess[]>([]);
/// 每 LP 使用率 0..1，index = LP index
export const usage = writable<number[]>([]);
/// 更新狀態
export const updateState = writable<UpdateState | null>(null);
/// 是否為可攜版（由 get_update_info 設定）
export const isPortable = writable<boolean>(false);
