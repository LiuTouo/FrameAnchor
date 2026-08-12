<script lang="ts">
  import { onMount } from 'svelte';
  import { t } from 'svelte-i18n';
  import BrowseDialog from '../components/BrowseDialog.svelte';
  import ConfirmDialog from '../components/ConfirmDialog.svelte';
  import RuleEditor from '../components/RuleEditor.svelte';
  import * as ipc from '../lib/ipc';
  import { rules, topology, settings } from '../lib/stores';
  import type { Rule, SessionSummary, WindowInfo } from '../lib/types';

  const clone = <T,>(v: T): T => JSON.parse(JSON.stringify(v));

  // ── 瀏覽視窗新增規則 ──
  let browseOpen = $state(false);

  // ── 草稿管理：每個規則獨立一份草稿 ──
  let drafts = $state<Map<string, Rule>>(new Map());

  // ── 選擇狀態 ──
  let selectedId = $state<string | null>(null);

  // ── 匯入資料（所有規則共用） ──
  let importableSessions = $state<SessionSummary[]>([]);
  let currentCpuFingerprint = $state('');

  // ── 刪除確認 ──
  let deleteTarget = $state<Rule | null>(null);

  // ── 全部規則：已儲存規則優先（保留後端順序，有草稿則用草稿），pending 規則接在後面 ──
  let allRules = $derived.by(() => {
    const list: Rule[] = [];
    for (const r of $rules) {
      list.push(drafts.get(r.id) ?? r);
    }
    for (const [id, draft] of drafts) {
      if (!$rules.some((r) => r.id === id)) {
        list.push(draft);
      }
    }
    return list;
  });

  let selectedDraft = $derived(selectedId ? drafts.get(selectedId) ?? null : null);
  let selectedIsNew = $derived(selectedId ? !$rules.some((r) => r.id === selectedId) : false);
  let selectedOriginal = $derived(selectedId ? $rules.find((r) => r.id === selectedId) ?? null : null);
  let selectedDirty = $derived.by(() => {
    if (!selectedDraft || !selectedOriginal) return selectedIsNew;
    return JSON.stringify(selectedDraft) !== JSON.stringify(selectedOriginal);
  });

  // ── 初始化 ──
  onMount(() => {
    void reloadImportData();
  });

  async function reload(savedId?: string) {
    const fresh = await ipc.getRules();
    rules.set(fresh);
    const next = new Map(drafts);
    for (const r of fresh) {
      // 剛儲存的規則：一律用後端版本取代草稿（清除 dirty）
      if (savedId != null && r.id === savedId) {
        next.set(r.id, clone(r));
        continue;
      }
      const existing = next.get(r.id);
      if (!existing) {
        // 新規則（例如從其他 instance 新增的）：建立草稿
        next.set(r.id, clone(r));
      }
      // 已有草稿且非剛儲存：保留使用者的未儲存編輯
    }
    drafts = next;
  }

  async function reloadImportData() {
    try {
      importableSessions = await ipc.listImportableSessions();
      currentCpuFingerprint = await ipc.getCurrentCpuFingerprint();
    } catch {
      // 無相容 session → 保持空清單
    }
  }

  // ── 新增規則 ──
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
    drafts = new Map(drafts).set(rule.id, rule);
    selectedId = rule.id;
  }

  // ── 選擇 ──
  function selectRule(id: string) {
    if (!drafts.has(id)) {
      const rule = allRules.find((r) => r.id === id);
      if (rule) drafts = new Map(drafts).set(id, clone(rule));
    }
    selectedId = id;
  }

  // ── 套用 ──
  async function onApply(rule: Rule) {
    try {
      await ipc.saveRule(rule);
      // reload 會以 savedId 參數明確替換剛儲存規則的草稿
      await reload(rule.id);
      selectedId = rule.id;
    } catch (e) {
      console.error('save_rule failed', e);
    }
  }

  // ── 刪除 ──
  function onDeleteRequest(id: string) {
    if (!$rules.some((r) => r.id === id)) {
      // pending 規則：直接丟棄草稿
      const next = new Map(drafts);
      next.delete(id);
      drafts = next;
      selectNearestAfterDelete(id);
      return;
    }
    const rule = $rules.find((r) => r.id === id);
    if (rule) deleteTarget = rule;
  }

  /** 在 allRules 上找尋刪除後最近的規則（pre-delete 位置） */
  function selectNearestAfterDelete(deletedId: string) {
    if (selectedId !== deletedId) return;
    const remaining = allRules.filter((r) => r.id !== deletedId);
    if (remaining.length === 0) {
      selectedId = null;
      return;
    }
    const idx = allRules.findIndex((r) => r.id === deletedId);
    const nextIdx = Math.min(idx, remaining.length - 1);
    selectedId = remaining[nextIdx].id;
  }

  async function confirmDelete() {
    if (!deleteTarget) return;
    const id = deleteTarget.id;
    deleteTarget = null;
    try {
      await ipc.deleteRule(id);
      // 先移除草稿，再選取最近規則（此時 allRules 尚含已刪除項目的 pre-delete 位置）
      const nextDrafts = new Map(drafts);
      nextDrafts.delete(id);
      drafts = nextDrafts;
      selectNearestAfterDelete(id);
      // reload 會從後端取得最新清單
      await reload();
    } catch (e) {
      console.error('delete_rule failed', e);
    }
  }

  // ── 草稿變更回呼（從 RuleEditor） ──
  function onDraftChange(rule: Rule) {
    drafts = new Map(drafts).set(rule.id, rule);
  }

  // ── 親和性摘要文字 ──
  function affinitySummary(r: Rule): string {
    const mode = r.affinity.mode;
    switch (mode) {
      case 'All': return $t('rules.presetAll') as string;
      case 'NoSmtSibling': return $t('rules.presetNoSmt') as string;
      case 'PCoresOnly': return $t('rules.presetPCores') as string;
      case 'Prefer': return $t('rules.presetPrefer') as string;
      case 'Custom': return `LP ${r.affinity.cores.length > 0 ? r.affinity.cores.join(',') : '—'}`;
      default: return mode;
    }
  }
</script>

<!-- 頁首工具列 -->
<div class="page-header">
  <h2>{$t('nav.rules')}</h2>
  <div class="header-actions">
    <button class="primary" onclick={() => (browseOpen = true)}>
      <svg viewBox="0 0 24 24" width="16" height="16" aria-hidden="true"><path d="M19 13h-6v6h-2v-6H5v-2h6V5h2v6h6v2z" fill="currentColor"/></svg>
      {$t('rules.addRule')}
    </button>
    <button onclick={() => ipc.reapplyAll()}>
      {$t('rules.reapply')}
    </button>
  </div>
</div>

{#if allRules.length === 0}
  <div class="empty-state">
    <svg viewBox="0 0 24 24" width="36" height="36" aria-hidden="true"><path d="M14 2H6c-1.1 0-2 .9-2 2v16c0 1.1.9 2 2 2h12c1.1 0 2-.9 2-2V8l-6-6zm-1 2l5 5h-5V4zM6 20V4h5v7h7v9H6z" fill="currentColor" opacity="0.3"/></svg>
    <p>{$t('rules.empty')}</p>
  </div>
{:else if $topology}
  <div class="workspace">
    <!-- 左側主清單 -->
    <div class="master" role="list" aria-label={$t('nav.rules')}>
      {#each allRules as rule (rule.id)}
        {@const isNew = !$rules.some((r) => r.id === rule.id)}
        {@const original = $rules.find((r) => r.id === rule.id) ?? null}
        {@const dirty = isNew || (original ? JSON.stringify(rule) !== JSON.stringify(original) : false)}
        <button
          class="master-item"
          class:selected={selectedId === rule.id}
          class:disabled={!rule.enabled}
          aria-current={selectedId === rule.id ? 'true' : undefined}
          onclick={() => selectRule(rule.id)}
        >
          <div class="master-item-top">
            <span class="master-name">{rule.name}</span>
            {#if dirty}
              <span class="badge unsaved-badge">{$t('rules.unsaved')}</span>
            {/if}
          </div>
          <div class="master-exe" title={rule.exePath}>{rule.exePath}</div>
          <div class="master-meta">
            <span class="hint">{affinitySummary(rule)}</span>
            {#if !rule.enabled}
              <span class="badge disabled-badge">{$t('rules.enabled')} ✕</span>
            {/if}
          </div>
        </button>
      {/each}
    </div>

    <!-- 右側編輯器 -->
    <div class="detail">
      {#if selectedDraft}
        <RuleEditor
          rule={selectedDraft}
          topology={$topology!}
          showAdvanced={$settings?.showAdvancedPriorities ?? false}
          isNew={selectedIsNew}
          dirty={selectedDirty}
          {importableSessions}
          {currentCpuFingerprint}
          onchange={onDraftChange}
          onapply={onApply}
          ondelete={onDeleteRequest}
        />
      {:else}
        <div class="detail-empty">
          <p class="hint">{$t('rules.selectHint')}</p>
        </div>
      {/if}
    </div>
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
  .page-header {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-3);
    margin-bottom: var(--space-4);
  }

  .page-header h2 {
    margin: 0;
    font-size: 15px;
  }

  .header-actions {
    display: flex;
    gap: var(--space-2);
  }

  .empty-state {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: var(--space-3);
    padding: var(--space-8);
    color: var(--text-secondary);
  }

  .workspace {
    display: flex;
    gap: 0;
    border: 1px solid var(--border-subtle);
    border-radius: var(--radius-md);
    overflow: hidden;
    min-height: 400px;
  }

  .master {
    width: 260px;
    flex-shrink: 0;
    display: flex;
    flex-direction: column;
    background: var(--surface-1);
    border-right: 1px solid var(--border-subtle);
    overflow-y: auto;
  }

  .master-item {
    display: flex;
    flex-direction: column;
    gap: 2px;
    width: 100%;
    text-align: left;
    background: transparent;
    border: none;
    border-bottom: 1px solid var(--border-subtle);
    border-radius: 0;
    padding: var(--space-2) var(--space-3);
    height: auto;
    cursor: pointer;
    color: var(--text-primary);
  }

  .master-item:hover {
    background: var(--surface-2);
  }

  .master-item.selected {
    background: var(--accent-muted);
    border-left: 3px solid var(--accent);
    padding-left: calc(var(--space-3) - 3px);
  }

  .master-item.disabled {
    opacity: 0.55;
  }

  .master-item-top {
    display: flex;
    align-items: center;
    gap: var(--space-2);
  }

  .master-name {
    font-weight: 500;
    font-size: 13px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .master-exe {
    font-size: 11px;
    color: var(--text-secondary);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    direction: rtl;
    text-align: left;
  }

  .master-meta {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    margin-top: 2px;
  }

  .unsaved-badge {
    background: var(--accent);
    color: var(--accent-text);
    font-size: 10px;
    flex-shrink: 0;
  }

  .disabled-badge {
    background: var(--surface-3);
    color: var(--text-muted);
    font-size: 10px;
  }

  .detail {
    flex: 1;
    overflow-y: auto;
    background: var(--surface-0);
  }

  .detail-empty {
    display: flex;
    align-items: center;
    justify-content: center;
    height: 100%;
    min-height: 300px;
  }

  .disclaimer {
    margin-top: var(--space-4);
  }
</style>
