<script lang="ts">
  import { t } from 'svelte-i18n';
  import AffinityPicker from './AffinityPicker.svelte';
  import type {
    AffinitySpec,
    CpuPriority,
    IoPriority,
    MemPriority,
    MatchBy,
    Rule,
    Topology,
  } from '../lib/types';

  let {
    rule,
    topology,
    showAdvanced,
    isNew,
    onApply,
    onDelete,
  }: {
    rule: Rule;
    topology: Topology;
    showAdvanced: boolean;
    isNew: boolean;
    onApply: (rule: Rule) => void;
    onDelete: (id: string) => void;
  } = $props();

  // 深拷貝：Rule 是純 JSON 資料，用 JSON round-trip。
  // 不能用 structuredClone — $state 的 proxy 無法被 structuredClone（DataCloneError）
  const clone = <T,>(v: T): T => JSON.parse(JSON.stringify(v));

  // 本地編輯草稿：props 變更（reload）時由下方 $effect 重置
  // svelte-ignore state_referenced_locally
  let draft = $state<Rule>(clone(rule));
  // svelte-ignore state_referenced_locally
  let original = $state<Rule>(clone(rule));
  $effect(() => {
    draft = clone(rule);
    original = clone(rule);
  });

  // 草稿與已儲存版本不同 → 顯示未儲存並啟用套用
  let dirty = $derived(JSON.stringify(draft) !== JSON.stringify(original));

  let advancedOpen = $state(false);
  let settingsOpen = $state(false);

  const PRIORITIES: CpuPriority[] = ['Idle', 'BelowNormal', 'Normal', 'AboveNormal', 'High'];
  const PRIO_I18N: Record<CpuPriority, string> = {
    Idle: 'rules.prioIdle',
    BelowNormal: 'rules.prioBelowNormal',
    Normal: 'rules.prioNormal',
    AboveNormal: 'rules.prioAboveNormal',
    High: 'rules.prioHigh',
  };
  const IO_OPTIONS: Array<IoPriority | null> = [null, 'VeryLow', 'Low', 'Normal', 'High'];
  const IO_I18N: Record<string, string> = {
    VeryLow: 'rules.ioVeryLow',
    Low: 'rules.ioLow',
    Normal: 'rules.ioNormal',
    High: 'rules.ioHigh',
  };
  const MEM_OPTIONS: Array<MemPriority | null> = [null, 'VeryLow', 'Low', 'Medium', 'BelowNormal', 'Normal'];
  const MEM_I18N: Record<string, string> = {
    VeryLow: 'rules.memVeryLow',
    Low: 'rules.memLow',
    Medium: 'rules.memMedium',
    BelowNormal: 'rules.memBelowNormal',
    Normal: 'rules.memNormal',
  };

</script>

<div class="card" class:disabled={!draft.enabled}>
  <div class="head">
    <label class="enable">
      <input type="checkbox" bind:checked={draft.enabled} />
      <span>{$t('rules.enabled')}</span>
    </label>
    <input
      class="name"
      type="text"
      bind:value={draft.name}
      aria-label={$t('rules.name')}
    />
    <span class="exe hint" title={draft.exePath}>{draft.exePath}</span>
    <button class="settings-toggle" onclick={() => (settingsOpen = !settingsOpen)}>
      {settingsOpen ? '▾' : '▸'} {$t('rules.settings')}
    </button>
    {#if isNew || dirty}
      <span class="unsaved" title={$t('rules.unsaved')}>{$t('rules.unsaved')}</span>
    {/if}
    <button class="del" onclick={() => onDelete(draft.id)}>{$t('rules.delete')}</button>
  </div>

  {#if settingsOpen}
  <div class="section">
    <div class="label">{$t('rules.affinity')}</div>
    <AffinityPicker
      {topology}
      spec={draft.affinity}
      onchange={(spec: AffinitySpec) => {
        draft.affinity = spec;
      }}
    />
    {#if draft.affinity.mode === 'Prefer'}
      <div class="hint">{$t('rules.preferHint')}</div>
    {/if}
  </div>

  <div class="row">
    <div class="section grow">
      <div class="label">{$t('rules.priority')}</div>
      <select bind:value={draft.priority}>
        {#each PRIORITIES as p}
          <option value={p}>{$t(PRIO_I18N[p])}</option>
        {/each}
      </select>
      <div class="hint">{$t('rules.priorityHint')}</div>
    </div>

    <div class="section grow">
      <div class="label">{$t('rules.matchBy')}</div>
      <label class="radio">
        <input
          type="radio"
          name="matchby-{draft.id}"
          checked={draft.matchBy === 'FullPath'}
          onchange={() => (draft.matchBy = 'FullPath')}
        />
        {$t('rules.matchFullPath')}
      </label>
      <label class="radio">
        <input
          type="radio"
          name="matchby-{draft.id}"
          checked={draft.matchBy === 'FileName'}
          onchange={() => (draft.matchBy = 'FileName')}
        />
        {$t('rules.matchFileName')}
      </label>
      {#if draft.matchBy === 'FileName'}
        <div class="hint">{$t('rules.fileNameWarn')}</div>
      {/if}
    </div>
  </div>

  {#if showAdvanced}
    <div class="section">
      <button class="adv-toggle" onclick={() => (advancedOpen = !advancedOpen)}>
        {advancedOpen ? '▾' : '▸'} {$t('rules.advanced')}
      </button>
      {#if advancedOpen}
        <div class="row">
          <div class="grow">
            <div class="label">{$t('rules.ioPriority')}</div>
            <select
              value={draft.advanced.ioPriority ?? ''}
              onchange={(e) => {
                const v = e.currentTarget.value;
                draft.advanced.ioPriority = (v === '' ? null : v) as IoPriority | null;
              }}
            >
              {#each IO_OPTIONS as o}
                <option value={o ?? ''}>{o === null ? $t('rules.noChange') : $t(IO_I18N[o])}</option>
              {/each}
            </select>
          </div>
          <div class="grow">
            <div class="label">{$t('rules.memPriority')}</div>
            <select
              value={draft.advanced.memoryPriority ?? ''}
              onchange={(e) => {
                const v = e.currentTarget.value;
                draft.advanced.memoryPriority = (v === '' ? null : v) as MemPriority | null;
              }}
            >
              {#each MEM_OPTIONS as o}
                <option value={o ?? ''}>{o === null ? $t('rules.noChange') : $t(MEM_I18N[o])}</option>
              {/each}
            </select>
          </div>
        </div>
        <div class="hint">{$t('rules.advancedHint')}</div>
      {/if}
    </div>
  {/if}
  {/if}

  <div class="foot">
    <button
      class="primary"
      disabled={!isNew && !dirty}
      onclick={() => onApply(clone(draft))}
    >
      {$t('rules.apply')}
    </button>
  </div>
</div>

<style>
  .card {
    background: var(--panel);
    border: 1px solid var(--border);
    border-radius: 8px;
    padding: 12px 14px;
  }
  .card.disabled {
    opacity: 0.55;
  }
  .head {
    display: flex;
    align-items: center;
    gap: 10px;
    margin-bottom: 10px;
  }
  .enable {
    display: flex;
    align-items: center;
    gap: 5px;
    flex-shrink: 0;
  }
  .name {
    width: 160px;
    font-weight: 600;
  }
  .exe {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    direction: rtl; /* 長路徑靠右顯示檔名 */
    text-align: left;
  }
  .del {
    flex-shrink: 0;
    color: var(--danger);
  }
  .unsaved {
    flex-shrink: 0;
    font-size: 11px;
    color: var(--accent);
    border: 1px solid var(--accent);
    border-radius: 4px;
    padding: 1px 6px;
  }
  .foot {
    display: flex;
    justify-content: flex-end;
    margin-top: 6px;
  }
  .section {
    margin-bottom: 10px;
  }
  .label {
    color: var(--muted);
    font-size: 11px;
    font-weight: 600;
    margin-bottom: 5px;
  }
  .row {
    display: flex;
    gap: 20px;
  }
  .grow {
    flex: 1;
  }
  .radio {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    margin-right: 14px;
    cursor: pointer;
  }
  .settings-toggle {
    flex-shrink: 0;
    background: none;
    border: none;
    color: var(--muted);
    padding: 0;
    font-size: 12px;
  }
  .settings-toggle:hover {
    color: var(--text);
  }
  .adv-toggle {
    background: none;
    border: none;
    color: var(--muted);
    padding: 0;
    margin-bottom: 6px;
  }
  .adv-toggle:hover {
    color: var(--text);
  }
  select {
    min-width: 130px;
  }
</style>
