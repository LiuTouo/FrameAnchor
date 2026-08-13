<script lang="ts">
  import { t } from 'svelte-i18n';
  import AffinityPicker from './AffinityPicker.svelte';
  import * as ipc from '../lib/ipc';
  import {
    maskToLp,
    nextAdjustedAfterManualChange,
    recommendationSourceTime,
  } from '../lib/affinity';
  import type {
    AffinitySpec,
    CpuPriority,
    IoPriority,
    MemPriority,
    MatchBy,
    Rule,
    SessionSummary,
    Topology,
  } from '../lib/types';

  let {
    rule,
    topology,
    showAdvanced,
    isNew,
    onApply,
    onDelete,
    importableSessions = [],
    currentCpuFingerprint = '',
  }: {
    rule: Rule;
    topology: Topology;
    showAdvanced: boolean;
    isNew: boolean;
    onApply: (rule: Rule) => void;
    onDelete: (id: string) => void;
    importableSessions?: SessionSummary[];
    currentCpuFingerprint?: string;
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

  // ── 基準測試推薦匯入 ──
  let importOpen = $state(false);
  let importSel = $state<string>('');
  let importBusy = $state(false);
  let importPreview = $state<{
    recommended: number[];
    session: SessionSummary;
    policyLp: number | null;
  } | null>(null);

  // 預設選取最新可匯入 session（list 依 startedAt 降冪）
  $effect(() => {
    if (importableSessions.length && !importSel) {
      const first = importableSessions[0];
      importSel = first.id;
      void loadImportPreview(first);
    }
  });

  async function loadImportPreview(s: SessionSummary) {
    if (s.bestLp == null) return;
    importBusy = true;
    try {
      const recommended = await ipc.computeRecommendedCores(s.bestLp, s.severeLps ?? []);
      const policy = await ipc.getGpuAffinityPolicy(s.gpuInstanceId).catch(() => null);
      const policyLp = policy ? maskToLp(policy.assignmentSetOverride?.bytes ?? null) : null;
      importPreview = { recommended, session: s, policyLp };
    } catch {
      importPreview = null;
    } finally {
      importBusy = false;
    }
  }

  function onImportSelect(id: string) {
    importSel = id;
    const s = importableSessions.find((x) => x.id === id);
    if (s) void loadImportPreview(s);
  }

  const importNoCores = $derived(importPreview ? importPreview.recommended.length === 0 : false);
  const coreDiff = $derived.by(() => {
    const p = importPreview;
    if (!p) return null;
    return {
      add: p.recommended.filter((i) => !draft.affinity.cores.includes(i)),
      remove: draft.affinity.cores.filter((i) => !p.recommended.includes(i)),
    };
  });

  function doImport() {
    const p = importPreview;
    if (!p || p.recommended.length === 0) return;
    draft.affinity = { mode: 'Custom', cores: p.recommended };
    draft.recommendation = {
      sessionId: p.session.id,
      // 來源時間：session.finishedAt ?? session.startedAt，非匯入當下
      generatedAt: recommendationSourceTime(p.session),
      cpuFingerprint: p.session.cpuFingerprint,
      gpuInstanceId: p.session.gpuInstanceId,
      bestLp: p.session.bestLp,
      severeLps: p.session.severeLps ?? [],
      recommendedCores: p.recommended,
      adjusted: false, // 只有重匯入會重置
    };
    importOpen = false;
  }

  // 使用者手動改親和性 → adjusted 維持 true 且不清除：
  // 即使之後手動回到精確的推薦 Custom 集合也一樣。只有重匯入（doImport 設 false）重置。
  function onAffinityChange(spec: AffinitySpec) {
    draft.affinity = spec;
    if (draft.recommendation && !draft.recommendation.adjusted) {
      draft.recommendation = {
        ...draft.recommendation,
        adjusted: nextAdjustedAfterManualChange(draft.recommendation.adjusted),
      };
    }
  }

  // 過時硬體警告：目前 CPU 指紋與儲存推薦不符 → 保留資料但提示
  const staleHardware = $derived(
    !!draft.recommendation?.cpuFingerprint &&
      !!currentCpuFingerprint &&
      draft.recommendation.cpuFingerprint !== currentCpuFingerprint,
  );

  function policyStatusText(p: { policyLp: number | null; session: SessionSummary }): string {
    if (p.policyLp == null) return '—';
    if (p.policyLp === p.session.bestLp) return $t('ruleImport.policyMatch') as string;
    return $t('ruleImport.policyMismatch', {
      values: { current: p.policyLp, best: p.session.bestLp },
    }) as string;
  }

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
    {#if staleHardware}
      <div class="hint stale-warn">{$t('ruleImport.staleHardware')}</div>
    {/if}
    <AffinityPicker
      {topology}
      spec={draft.affinity}
      recommendation={draft.recommendation}
      onchange={onAffinityChange}
    />
    {#if draft.affinity.mode === 'Prefer'}
      <div class="hint">{$t('rules.preferHint')}</div>
    {/if}
  </div>

  <div class="section">
    <button class="adv-toggle" onclick={() => (importOpen = !importOpen)}>
      {importOpen ? '▾' : '▸'} {$t('ruleImport.open')}
    </button>
    {#if importOpen}
      {#if importableSessions.length === 0}
        <div class="hint">{$t('ruleImport.emptyList')}</div>
      {:else}
        <div class="import-row">
          <label class="grow">
            <span class="label-inline">{$t('ruleImport.source')}</span>
            <select value={importSel} onchange={(e) => onImportSelect(e.currentTarget.value)}>
              {#each importableSessions as s (s.id)}
                <option value={s.id}>
                  {s.startedAt.replace('T', ' ').slice(0, 16)} — {s.gpuName || s.gpuInstanceId} —
                  {s.bestLp ?? '—'}
                </option>
              {/each}
            </select>
          </label>
        </div>
        {#if importPreview}
          <dl class="import-meta">
            <div>
              <dt>{$t('ruleImport.date')}</dt>
              <dd>{importPreview.session.startedAt.replace('T', ' ').slice(0, 19)}</dd>
            </div>
            <div>
              <dt>{$t('ruleImport.gpu')}</dt>
              <dd>{importPreview.session.gpuName || importPreview.session.gpuInstanceId}</dd>
            </div>
            <div>
              <dt>{$t('ruleImport.best')}</dt>
              <dd>{importPreview.session.bestLp ?? '—'}</dd>
            </div>
            <div>
              <dt>{$t('gpuTest.policyCurrent')}</dt>
              <dd>{policyStatusText(importPreview)}</dd>
            </div>
            {#if coreDiff}
              <div>
                <dt>{$t('gpuTest.colLp')}</dt>
                <dd>
                  {$t('ruleImport.coreDiff', {
                    values: { add: coreDiff.add.join(', ') || '0', remove: coreDiff.remove.join(', ') || '0' },
                  })}
                </dd>
              </div>
            {/if}
          </dl>
          {#if importNoCores}
            <div class="hint no-cores">{$t('ruleImport.noCores')}</div>
          {/if}
          <div class="toolbar">
            <button
              class="primary"
              disabled={importBusy || importNoCores}
              onclick={doImport}
            >
              {$t('ruleImport.import')}
            </button>
          </div>
        {/if}
      {/if}
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
    font-weight: var(--font-weight-medium);
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
    font-weight: var(--font-weight-medium);
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
  .stale-warn {
    color: #f0a33c;
    margin-bottom: 6px;
  }
  .import-row {
    display: flex;
    align-items: flex-end;
    gap: 10px;
    margin-bottom: 8px;
  }
  .label-inline {
    display: block;
    color: var(--muted);
    font-size: 11px;
    margin-bottom: 3px;
  }
  .import-row select {
    width: 100%;
  }
  .import-meta {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(150px, 1fr));
    gap: 4px 14px;
    margin: 0 0 8px;
  }
  .import-meta div {
    display: flex;
    gap: 8px;
  }
  .import-meta dt {
    color: var(--muted);
    font-size: 12px;
  }
  .import-meta dd {
    margin: 0;
  }
  .no-cores {
    color: var(--danger);
    margin-bottom: 8px;
  }
  .toolbar {
    display: flex;
    gap: 8px;
    margin-top: 6px;
  }
</style>
