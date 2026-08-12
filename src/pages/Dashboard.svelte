<script lang="ts">
  import { onMount } from 'svelte';
  import { t } from 'svelte-i18n';
  import TopologyGrid from '../components/TopologyGrid.svelte';
  import AppliedTable from '../components/AppliedTable.svelte';
  import * as ipc from '../lib/ipc';
  import { topology, usage, applied } from '../lib/stores';
  import type { AppliedProcess } from '../lib/types';

  let selectedPid = $state<number | null>(null);

  let sortedApplied = $derived(
    [...$applied].sort((a, b) =>
      a.exeName.localeCompare(b.exeName) || a.pid - b.pid,
    ),
  );

  let selected = $derived(
    selectedPid != null
      ? sortedApplied.find((p) => p.pid === selectedPid) ?? null
      : null,
  );

  // 執行中進程數量摘要
  let runningCount = $derived(sortedApplied.length);
  // 互斥分類：OK = 完全成功，Partial = 無錯誤但非全部 OK，Failed = 有錯誤
  let okCount = $derived(sortedApplied.filter((p) => !p.error && p.affinityOk && p.priorityOk && p.ioOk !== false && p.memOk !== false).length);
  let failCount = $derived(sortedApplied.filter((p) => p.error != null).length);
  let partialCount = $derived(runningCount - okCount - failCount);

  // 自動選擇第一個 PID；當前選擇消失時切至下一個；清單為空時清除
  $effect(() => {
    const list = sortedApplied;
    if (list.length === 0) {
      selectedPid = null;
      return;
    }
    if (selectedPid != null && list.some((p) => p.pid === selectedPid)) {
      return;
    }
    selectedPid = list[0].pid;
  });

  // usage streaming 僅在有 applied 程序時開啟（避免重複 invoke）
  let wasActive = $state(false);
  $effect(() => {
    const active = sortedApplied.length > 0;
    if (active !== wasActive) {
      wasActive = active;
      ipc.setUsageStreaming(active);
      if (!active) {
        usage.set([]);
      }
    }
  });

  // 面板卸載時關閉 streaming 並清除 usage
  onMount(() => {
    return () => {
      ipc.setUsageStreaming(false);
      usage.set([]);
    };
  });

  // covered = 所選程序目前套用的核心
  let covered = $derived(
    selected ? new Set(selected.currentCores) : new Set<number>(),
  );
</script>

<!-- 頁首：執行狀態摘要 -->
<div class="page-header">
  <h2>{$t('dashboard.cpuTitle')}</h2>
  {#if runningCount > 0}
    <div class="summary-strip" role="status" aria-label={$t('dashboard.summaryLabel', { values: { count: runningCount } })}>
      <span class="summary-badge ok">
        <svg viewBox="0 0 24 24" width="12" height="12" aria-hidden="true"><path d="M9 16.17L4.83 12l-1.42 1.41L9 19 21 7l-1.41-1.41z" fill="currentColor"/></svg>
        {$t('dashboard.summaryOk', { values: { count: okCount } })}
      </span>
      {#if partialCount > 0}
        <span class="summary-badge warn">
          <svg viewBox="0 0 24 24" width="12" height="12" aria-hidden="true"><path d="M12 2L2 22h20L12 2zm0 3.99L19.53 20H4.47L12 5.99zM11 16h2v2h-2zm0-6h2v4h-2z" fill="currentColor"/></svg>
          {$t('dashboard.summaryPartial', { values: { count: partialCount } })}
        </span>
      {/if}
      {#if failCount > 0}
        <span class="summary-badge fail">
          <svg viewBox="0 0 24 24" width="12" height="12" aria-hidden="true"><path d="M12 2C6.48 2 2 6.48 2 12s4.48 10 10 10 10-4.48 10-10S17.52 2 12 2zm1 15h-2v-2h2v2zm0-4h-2V7h2v6z" fill="currentColor"/></svg>
          {$t('dashboard.summaryFail', { values: { count: failCount } })}
        </span>
      {/if}
    </div>
  {/if}
</div>

<!-- CPU 使用率面板 -->
{#if sortedApplied.length > 0 && $topology}
  <div class="selector-row">
    <label for="pid-select" class="selector-label">{$t('dashboard.selectProcess')}</label>
    <select id="pid-select" bind:value={selectedPid}>
      {#each sortedApplied as p (p.pid)}
        <option value={p.pid}>{p.exeName} ({p.pid}) — {p.ruleName}</option>
      {/each}
    </select>
    <span class="hint">{$t('dashboard.systemPerCoreNote')}</span>
  </div>
  <TopologyGrid topology={$topology} usage={$usage} {covered} />
  <div class="legend-row" aria-label={$t('dashboard.coveredLegend')}>
    <span class="legend-dot covered"></span>
    <span class="hint">{$t('dashboard.coveredLegend')}</span>
    <span class="legend-dot smt"></span>
    <span class="hint">{$t('dashboard.smtLegend')}</span>
  </div>
{:else}
  <div class="panel empty-state">
    <svg viewBox="0 0 24 24" width="32" height="32" aria-hidden="true"><path d="M3 3h18v18H3V3zm2 2v14h14V5H5zm2 2h10v2H7V7zm0 4h6v2H7v-2z" fill="currentColor" opacity="0.3"/></svg>
    <p>{$t('dashboard.emptyCpuPanel')}</p>
  </div>
{/if}

<!-- 已套用進程表格 -->
<h2>{$t('dashboard.appliedTitle')}</h2>
<AppliedTable applied={$applied} />

<style>
  .page-header {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: var(--space-3);
    margin-bottom: var(--space-4);
  }

  .page-header h2 {
    margin: 0;
    font-size: 15px;
  }

  .summary-strip {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-2);
  }

  .summary-badge {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    padding: 1px 8px;
    border-radius: var(--radius-full);
    font-size: 12px;
    font-weight: 500;
    line-height: 20px;
  }

  .summary-badge.ok {
    background: var(--success-muted);
    color: var(--success);
  }

  .summary-badge.warn {
    background: var(--warning-muted);
    color: var(--warning);
  }

  .summary-badge.fail {
    background: var(--danger-muted);
    color: var(--danger);
  }

  h2 {
    font-size: 14px;
    margin: var(--space-6) 0 var(--space-3);
  }

  .selector-row {
    display: flex;
    align-items: center;
    gap: var(--space-3);
    margin-bottom: var(--space-3);
    flex-wrap: wrap;
  }

  .selector-label {
    color: var(--text-secondary);
    font-size: 12px;
    white-space: nowrap;
  }

  .selector-row select {
    max-width: 340px;
  }

  .legend-row {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    margin-top: var(--space-2);
  }

  .legend-dot {
    width: 12px;
    height: 12px;
    border-radius: 3px;
    flex-shrink: 0;
  }

  .legend-dot.covered {
    border: 2px solid var(--accent);
    background: transparent;
  }

  .legend-dot.smt {
    border: 1px solid var(--border-default);
    background: var(--surface-2);
  }

  .empty-state {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: var(--space-3);
    color: var(--text-secondary);
  }
</style>
