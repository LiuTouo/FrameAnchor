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
    Rule,
    SessionSummary,
    Topology,
  } from '../lib/types';

  let {
    rule,
    topology,
    showAdvanced,
    isNew,
    dirty,
    importableSessions = [],
    currentCpuFingerprint = '',
    onchange,
    onapply,
    ondelete,
  }: {
    rule: Rule;
    topology: Topology;
    showAdvanced: boolean;
    isNew: boolean;
    dirty: boolean;
    importableSessions?: SessionSummary[];
    currentCpuFingerprint?: string;
    onchange: (rule: Rule) => void;
    onapply: (rule: Rule) => void;
    ondelete: (id: string) => void;
  } = $props();

  const clone = <T,>(v: T): T => JSON.parse(JSON.stringify(v));

  // ── 展開狀態 ──
  let importOpen = $state(false);
  let advancedOpen = $state(false);

  // ── 輔助：更新草稿 ──
  function update(patch: Partial<Rule>) {
    onchange({ ...rule, ...patch });
  }

  // ── 基準測試推薦匯入 ──
  let importSel = $state<string>('');
  let importBusy = $state(false);
  let importPreview = $state<{
    recommended: number[];
    session: SessionSummary;
    policyLp: number | null;
  } | null>(null);

  $effect(() => {
    if (importOpen && importableSessions.length && !importSel) {
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
      add: p.recommended.filter((i) => !rule.affinity.cores.includes(i)),
      remove: rule.affinity.cores.filter((i) => !p.recommended.includes(i)),
    };
  });

  function doImport() {
    const p = importPreview;
    if (!p || p.recommended.length === 0) return;
    update({
      affinity: { mode: 'Custom', cores: p.recommended },
      recommendation: {
        sessionId: p.session.id,
        generatedAt: recommendationSourceTime(p.session),
        cpuFingerprint: p.session.cpuFingerprint,
        gpuInstanceId: p.session.gpuInstanceId,
        bestLp: p.session.bestLp,
        severeLps: p.session.severeLps ?? [],
        recommendedCores: p.recommended,
        adjusted: false,
      },
    });
    importOpen = false;
  }

  function onAffinityChange(spec: AffinitySpec) {
    const next = { ...rule, affinity: spec };
    if (rule.recommendation && !rule.recommendation.adjusted) {
      next.recommendation = {
        ...rule.recommendation,
        adjusted: nextAdjustedAfterManualChange(rule.recommendation.adjusted),
      };
    }
    onchange(next);
  }

  const staleHardware = $derived(
    !!rule.recommendation?.cpuFingerprint &&
      !!currentCpuFingerprint &&
      rule.recommendation.cpuFingerprint !== currentCpuFingerprint,
  );

  function policyStatusText(p: { policyLp: number | null; session: SessionSummary }): string {
    if (p.policyLp == null) return '—';
    if (p.policyLp === p.session.bestLp) return $t('ruleImport.policyMatch') as string;
    return $t('ruleImport.policyMismatch', {
      values: { current: p.policyLp, best: p.session.bestLp },
    }) as string;
  }

  // ── 常數 ──
  const PRIORITIES: CpuPriority[] = ['Idle', 'BelowNormal', 'Normal', 'AboveNormal', 'High'];
  const PRIO_I18N: Record<CpuPriority, string> = {
    Idle: 'rules.prioIdle', BelowNormal: 'rules.prioBelowNormal', Normal: 'rules.prioNormal',
    AboveNormal: 'rules.prioAboveNormal', High: 'rules.prioHigh',
  };
  const IO_OPTIONS: Array<IoPriority | null> = [null, 'VeryLow', 'Low', 'Normal', 'High'];
  const IO_I18N: Record<string, string> = {
    VeryLow: 'rules.ioVeryLow', Low: 'rules.ioLow', Normal: 'rules.ioNormal', High: 'rules.ioHigh',
  };
  const MEM_OPTIONS: Array<MemPriority | null> = [null, 'VeryLow', 'Low', 'Medium', 'BelowNormal', 'Normal'];
  const MEM_I18N: Record<string, string> = {
    VeryLow: 'rules.memVeryLow', Low: 'rules.memLow', Medium: 'rules.memMedium',
    BelowNormal: 'rules.memBelowNormal', Normal: 'rules.memNormal',
  };
</script>

<div class="editor">
  <!-- ── 基本資訊 ── -->
  <section class="editor-section">
    <h3 class="section-title">{$t('rules.settings')}</h3>
    <div class="basic-grid">
      <label class="field">
        <span class="field-label">{$t('rules.name')}</span>
        <input
          type="text"
          value={rule.name}
          oninput={(e) => update({ name: e.currentTarget.value })}
        />
      </label>
      <label class="field check">
        <input
          type="checkbox"
          checked={rule.enabled}
          onchange={(e) => update({ enabled: e.currentTarget.checked })}
        />
        <span>{$t('rules.enabled')}</span>
      </label>
    </div>
    <div class="exe-path" title={rule.exePath}>
      <span class="hint">{$t('rules.matchFullPath')}:</span>
      <span class="mono">{rule.exePath}</span>
    </div>
  </section>

  <!-- ── CPU 親和性 ── -->
  <section class="editor-section">
    <h3 class="section-title">{$t('rules.affinity')}</h3>
    {#if staleHardware}
      <div class="stale-warn" role="alert">
        <svg viewBox="0 0 24 24" width="14" height="14" aria-hidden="true"><path d="M1 21h22L12 2 1 21zm12-3h-2v-2h2v2zm0-4h-2v-4h2v4z" fill="currentColor"/></svg>
        {$t('ruleImport.staleHardware')}
      </div>
    {/if}
    <AffinityPicker
      {topology}
      spec={rule.affinity}
      recommendation={rule.recommendation}
      onchange={onAffinityChange}
    />
    {#if rule.affinity.mode === 'Prefer'}
      <div class="hint" style="margin-top: var(--space-2)">{$t('rules.preferHint')}</div>
    {/if}
  </section>

  <!-- ── CPU 優先級 ── -->
  <section class="editor-section">
    <h3 class="section-title">{$t('rules.priority')}</h3>
    <div class="field">
      <select value={rule.priority} onchange={(e) => update({ priority: e.currentTarget.value as CpuPriority })}>
        {#each PRIORITIES as p}
          <option value={p}>{$t(PRIO_I18N[p])}</option>
        {/each}
      </select>
    </div>
    <div class="hint" style="margin-top: var(--space-1)">{$t('rules.priorityHint')}</div>
  </section>

  <!-- ── 比對方式 ── -->
  <section class="editor-section">
    <h3 class="section-title">{$t('rules.matchBy')}</h3>
    <div class="radio-group">
      <label class="radio">
        <input type="radio" name={`matchby-${rule.id}`} checked={rule.matchBy === 'FullPath'} onchange={() => update({ matchBy: 'FullPath' })} />
        <span>{$t('rules.matchFullPath')}</span>
      </label>
      <label class="radio">
        <input type="radio" name={`matchby-${rule.id}`} checked={rule.matchBy === 'FileName'} onchange={() => update({ matchBy: 'FileName' })} />
        <span>{$t('rules.matchFileName')}</span>
      </label>
    </div>
    {#if rule.matchBy === 'FileName'}
      <div class="hint" style="margin-top: var(--space-2)">{$t('rules.fileNameWarn')}</div>
    {/if}
  </section>

  <!-- ── GPU 基準測試推薦匯入 ── -->
  <section class="editor-section">
    <button
      class="section-toggle"
      onclick={() => (importOpen = !importOpen)}
      aria-expanded={importOpen}
    >
      <svg viewBox="0 0 24 24" width="12" height="12" aria-hidden="true" class:rotated={importOpen}><path d="M8 5l8 7-8 7" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round"/></svg>
      <span>{$t('ruleImport.open')}</span>
    </button>
    {#if importOpen}
      {#if importableSessions.length === 0}
        <div class="hint" style="margin-top: var(--space-2)">{$t('ruleImport.emptyList')}</div>
      {:else}
        <div class="import-panel">
          <div class="field">
            <span class="field-label">{$t('ruleImport.source')}</span>
            <select value={importSel} onchange={(e) => onImportSelect(e.currentTarget.value)}>
              {#each importableSessions as s (s.id)}
                <option value={s.id}>
                  {s.startedAt.replace('T', ' ').slice(0, 16)} — {s.gpuName || s.gpuInstanceId} — {s.bestLp ?? '—'}
                </option>
              {/each}
            </select>
          </div>
          {#if importPreview}
            <dl class="import-meta">
              <div><dt>{$t('ruleImport.date')}</dt><dd>{importPreview.session.startedAt.replace('T', ' ').slice(0, 19)}</dd></div>
              <div><dt>{$t('ruleImport.gpu')}</dt><dd>{importPreview.session.gpuName || importPreview.session.gpuInstanceId}</dd></div>
              <div><dt>{$t('ruleImport.best')}</dt><dd>{importPreview.session.bestLp ?? '—'}</dd></div>
              <div><dt>{$t('gpuTest.policyCurrent')}</dt><dd>{policyStatusText(importPreview)}</dd></div>
              {#if coreDiff}
                <div><dt>{$t('gpuTest.colLp')}</dt><dd>{$t('ruleImport.coreDiff', { values: { add: coreDiff.add.join(', ') || '0', remove: coreDiff.remove.join(', ') || '0' } })}</dd></div>
              {/if}
            </dl>
            {#if importNoCores}
              <div class="hint no-cores">{$t('ruleImport.noCores')}</div>
            {/if}
            <button class="primary small" disabled={importBusy || importNoCores} onclick={doImport}>
              {$t('ruleImport.import')}
            </button>
          {/if}
        </div>
      {/if}
    {/if}
  </section>

  <!-- ── 進階優先級 ── -->
  {#if showAdvanced}
    <section class="editor-section">
      <button
        class="section-toggle"
        onclick={() => (advancedOpen = !advancedOpen)}
        aria-expanded={advancedOpen}
      >
        <svg viewBox="0 0 24 24" width="12" height="12" aria-hidden="true" class:rotated={advancedOpen}><path d="M8 5l8 7-8 7" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round"/></svg>
        <span>{$t('rules.advanced')}</span>
      </button>
      {#if advancedOpen}
        <div class="advanced-grid">
          <div class="field">
            <span class="field-label">{$t('rules.ioPriority')}</span>
            <select
              value={rule.advanced.ioPriority ?? ''}
              onchange={(e) => {
                const v = e.currentTarget.value;
                update({ advanced: { ...rule.advanced, ioPriority: (v === '' ? null : v) as IoPriority | null } });
              }}
            >
              {#each IO_OPTIONS as o}
                <option value={o ?? ''}>{o === null ? $t('rules.noChange') : $t(IO_I18N[o])}</option>
              {/each}
            </select>
          </div>
          <div class="field">
            <span class="field-label">{$t('rules.memPriority')}</span>
            <select
              value={rule.advanced.memoryPriority ?? ''}
              onchange={(e) => {
                const v = e.currentTarget.value;
                update({ advanced: { ...rule.advanced, memoryPriority: (v === '' ? null : v) as MemPriority | null } });
              }}
            >
              {#each MEM_OPTIONS as o}
                <option value={o ?? ''}>{o === null ? $t('rules.noChange') : $t(MEM_I18N[o])}</option>
              {/each}
            </select>
          </div>
        </div>
        <div class="hint" style="margin-top: var(--space-2)">{$t('rules.advancedHint')}</div>
      {/if}
    </section>
  {/if}

  <!-- ── 操作列 ── -->
  <div class="action-bar">
    <button
      class="danger"
      onclick={() => ondelete(rule.id)}
    >
      <svg viewBox="0 0 24 24" width="14" height="14" aria-hidden="true"><path d="M6 19c0 1.1.9 2 2 2h8c1.1 0 2-.9 2-2V7H6v12zM19 4h-3.5l-1-1h-5l-1 1H5v2h14V4z" fill="currentColor"/></svg>
      {$t('rules.delete')}
    </button>
    <button
      class="primary"
      disabled={!isNew && !dirty}
      onclick={() => onapply(clone(rule))}
    >
      {$t('rules.apply')}
    </button>
  </div>
</div>

<style>
  .editor {
    padding: var(--space-4);
    display: flex;
    flex-direction: column;
    gap: 0;
  }

  .editor-section {
    padding: var(--space-3) 0;
    border-bottom: 1px solid var(--border-subtle);
  }

  .editor-section:first-child {
    padding-top: 0;
  }

  .section-title {
    margin: 0 0 var(--space-2);
    font-size: 12px;
    font-weight: var(--font-weight-medium);
    color: var(--text-secondary);
    text-transform: uppercase;
    letter-spacing: 0.05em;
  }

  .section-toggle {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    background: none;
    border: none;
    color: var(--text-primary);
    font: inherit;
    font-weight: var(--font-weight-medium);
    font-size: 13px;
    padding: 0;
    cursor: pointer;
    width: 100%;
    text-align: left;
  }

  .section-toggle:hover {
    color: var(--accent);
  }

  .section-toggle svg {
    transition: transform var(--transition-fast);
    flex-shrink: 0;
  }

  .section-toggle svg.rotated {
    transform: rotate(90deg);
  }

  .basic-grid {
    display: flex;
    align-items: flex-end;
    gap: var(--space-4);
    flex-wrap: wrap;
  }

  .field {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }

  .field.check {
    flex-direction: row;
    align-items: center;
    gap: var(--space-2);
    padding-bottom: 4px;
  }

  .field-label {
    color: var(--text-muted);
    font-size: 11px;
    font-weight: var(--font-weight-medium);
  }

  .exe-path {
    margin-top: var(--space-2);
    display: flex;
    gap: var(--space-2);
    align-items: baseline;
    overflow: hidden;
  }

  .exe-path .mono {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: 11px;
    color: var(--text-secondary);
  }

  .mono {
    font-family: 'IBM Plex Sans TC', monospace;
  }

  .radio-group {
    display: flex;
    gap: var(--space-4);
  }

  .radio {
    display: inline-flex;
    align-items: center;
    gap: var(--space-2);
    cursor: pointer;
  }

  .stale-warn {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    color: var(--warning);
    font-size: 12px;
    margin-bottom: var(--space-2);
  }

  /* ── 匯入面板 ── */
  .import-panel {
    margin-top: var(--space-3);
    padding: var(--space-3);
    background: var(--surface-1);
    border: 1px solid var(--border-subtle);
    border-radius: var(--radius-sm);
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
  }

  .import-meta {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(150px, 1fr));
    gap: 4px 14px;
    margin: 0;
  }

  .import-meta div {
    display: flex;
    gap: var(--space-2);
  }

  .import-meta dt {
    color: var(--text-secondary);
    font-size: 11px;
  }

  .import-meta dd {
    margin: 0;
    font-size: 12px;
  }

  .no-cores {
    color: var(--danger);
  }

  /* ── 進階優先級 ── */
  .advanced-grid {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: var(--space-3);
    margin-top: var(--space-3);
  }

  /* ── 操作列 ── */
  .action-bar {
    display: flex;
    justify-content: flex-end;
    gap: var(--space-2);
    padding: var(--space-3) 0 0;
  }
</style>
