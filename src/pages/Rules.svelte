<script lang="ts">
  import { onMount } from 'svelte';
  import { t } from 'svelte-i18n';
  import RuleCard from '../components/RuleCard.svelte';
  import BrowseDialog from '../components/BrowseDialog.svelte';
  import ConfirmDialog from '../components/ConfirmDialog.svelte';
  import * as ipc from '../lib/ipc';
  import { rules, topology, settings } from '../lib/stores';
  import type { Rule, SessionSummary, WindowInfo } from '../lib/types';

  let browseOpen = $state(false);
  // 尚未套用的新規則草稿（瀏覽新增）
  let pending = $state<Rule[]>([]);
  // 可匯入的基準測試 session + 目前 CPU 指紋（所有 RuleCard 共用）
  let importableSessions = $state<SessionSummary[]>([]);
  let currentCpuFingerprint = $state('');
  let deleteTarget = $state<Rule | null>(null);

  async function reload() {
    rules.set(await ipc.getRules());
  }

  async function reloadImportData() {
    try {
      importableSessions = await ipc.listImportableSessions();
      currentCpuFingerprint = await ipc.getCurrentCpuFingerprint();
    } catch {
      // 無相容 session / IPC 失敗 → 保持空清單，import 區塊顯示 emptyList
    }
  }

  onMount(() => {
    void reloadImportData();
  });

  async function onApply(rule: Rule) {
    try {
      await ipc.saveRule(rule);
      pending = pending.filter((r) => r.id !== rule.id);
      await reload();
    } catch (e) {
      console.error('save_rule failed', e);
    }
  }

  async function onDelete(id: string) {
    // 草稿規則：直接丟棄，不碰後端
    if (pending.some((r) => r.id === id)) {
      pending = pending.filter((r) => r.id !== id);
      return;
    }
    const rule = $rules.find((r) => r.id === id);
    if (!rule) return;
    deleteTarget = rule;
  }

  async function confirmDelete() {
    if (!deleteTarget) return;
    const id = deleteTarget.id;
    deleteTarget = null;
    await ipc.deleteRule(id);
    await reload();
  }

  function onBrowseSelect(w: WindowInfo) {
    if (!w.exePath) return;
    const base = w.exeName.replace(/\.exe$/i, '');
    const rule: Rule = {
      id: crypto.randomUUID(),
      name: base,
      exePath: w.exePath,
      matchBy: 'FullPath',
      enabled: true,
      affinity: { mode: 'All', cores: [] },
      priority: 'High',
      advanced: { ioPriority: null, memoryPriority: null },
    };
    browseOpen = false;
    pending = [...pending, rule];
  }
</script>

<div class="toolbar">
  <button class="primary" onclick={() => (browseOpen = true)}>{$t('rules.addRule')}</button>
  <button onclick={() => ipc.reapplyAll()}>{$t('rules.reapply')}</button>
</div>

{#if $rules.length === 0 && pending.length === 0}
  <div class="empty hint">{$t('rules.empty')}</div>
{:else if $topology}
  <div class="list">
    {#each [...$rules, ...pending] as rule (rule.id)}
      <RuleCard
        {rule}
        topology={$topology}
        showAdvanced={$settings?.showAdvancedPriorities ?? false}
        isNew={pending.some((r) => r.id === rule.id)}
        {onApply}
        {onDelete}
        importableSessions={importableSessions}
        currentCpuFingerprint={currentCpuFingerprint}
      />
    {/each}
  </div>
{/if}

<div class="hint disclaimer">{$t('rules.disclaimer')}</div>

<BrowseDialog bind:open={browseOpen} onselect={onBrowseSelect} />
<ConfirmDialog
  open={deleteTarget !== null}
  title={$t('rules.deleteTitle') as string}
  message={$t('rules.deleteConfirm', { values: { name: deleteTarget?.name ?? '' } }) as string}
  confirmLabel={$t('rules.delete') as string}
  cancelLabel={$t('common.cancel') as string}
  danger
  onconfirm={confirmDelete}
  oncancel={() => (deleteTarget = null)}
/>

<style>
  .toolbar {
    display: flex;
    gap: 8px;
    margin-bottom: 14px;
  }
  .empty {
    padding: 24px;
    text-align: center;
    background: var(--panel);
    border: 1px dashed var(--border);
    border-radius: 8px;
  }
  .list {
    display: flex;
    flex-direction: column;
    gap: 12px;
  }
  .disclaimer {
    margin-top: 16px;
  }
</style>
