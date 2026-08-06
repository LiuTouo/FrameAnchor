import { writable } from 'svelte/store';
import type { AppliedProcess, Rule, Settings, Topology } from './types';

export const topology = writable<Topology | null>(null);
export const rules = writable<Rule[]>([]);
export const settings = writable<Settings | null>(null);
export const applied = writable<AppliedProcess[]>([]);
/// 每 LP 使用率 0..1，index = LP index
export const usage = writable<number[]>([]);
