<script lang="ts">
  import { t } from 'svelte-i18n';
  import type { AppliedProcess } from '../lib/types';

  let { applied }: { applied: AppliedProcess[] } = $props();

  function fmtCores(cores: number[]): string {
    if (!cores.length) return '—';
    const sorted = [...cores].sort((a, b) => a - b);
    const contiguous = sorted.every((v, i) => i === 0 || v === sorted[i - 1] + 1);
    if (contiguous && sorted.length > 4) {
      return `LP ${sorted[0]}–${sorted[sorted.length - 1]} (${sorted.length})`;
    }
    return sorted.map((i) => `LP${i}`).join(', ');
  }

  const KNOWN_ERRORS = [
    'ACCESS_DENIED',
    'OPEN_FAILED',
    'SET_AFFINITY_FAILED',
    'SET_PRIORITY_FAILED',
    'IO_FAILED',
    'MEM_FAILED',
  ];

  function errText(code: string): string {
    return $t(`errors.${KNOWN_ERRORS.includes(code) ? code : 'unknown'}`);
  }
</script>

{#if applied.length === 0}
  <div class="empty-state">
    <p>{$t('dashboard.emptyApplied')}</p>
  </div>
{:else}
  <div class="table-scroll">
    <table>
      <thead>
        <tr>
          <th>{$t('dashboard.colGame')}</th>
          <th>{$t('dashboard.colPid')}</th>
          <th>{$t('dashboard.colRule')}</th>
          <th>{$t('dashboard.colAffinity')}</th>
          <th>{$t('dashboard.colPriority')}</th>
          <th>{$t('dashboard.colStatus')}</th>
        </tr>
      </thead>
      <tbody>
        {#each applied as a (a.pid)}
          {@const allOk = !a.error && a.affinityOk && a.priorityOk && a.ioOk !== false && a.memOk !== false}
          {@const isPartial = !a.error && !allOk}
          <tr class:row-error={a.error != null} class:row-partial={isPartial}>
            <td class="cell-exe" title={a.exeName}>{a.exeName}</td>
            <td class="cell-pid">{a.pid}</td>
            <td class="cell-rule" title={a.ruleName}>{a.ruleName}</td>
            <td>
              {#if a.softAffinity}
                <span class="badge soft">{$t('dashboard.softAffinity')}</span>
              {/if}
              <span class="mono">{fmtCores(a.currentCores)}</span>
            </td>
            <td>{a.currentPriority || '—'}</td>
            <td class="cell-status">
              {#if a.error}
                <span
                  class="status-marker fail"
                  title={['ACCESS_DENIED', 'SET_AFFINITY_FAILED'].includes(a.error ?? '') ? $t('errors.anticheatNote') : ''}
                  role="status"
                >
                  <svg viewBox="0 0 24 24" width="14" height="14" aria-hidden="true"><path d="M12 2C6.48 2 2 6.48 2 12s4.48 10 10 10 10-4.48 10-10S17.52 2 12 2zm1 15h-2v-2h2v2zm0-4h-2V7h2v6z" fill="currentColor"/></svg>
                  <span>{errText(a.error)}</span>
                </span>
              {:else if allOk}
                <span class="status-marker ok" role="status">
                  <svg viewBox="0 0 24 24" width="14" height="14" aria-hidden="true"><path d="M12 2C6.48 2 2 6.48 2 12s4.48 10 10 10 10-4.48 10-10S17.52 2 12 2zm-2 15l-5-5 1.41-1.41L10 14.17l7.59-7.59L19 8l-9 9z" fill="currentColor"/></svg>
                  <span>{$t('dashboard.statusOk')}</span>
                </span>
              {:else}
                <span class="status-marker warn" role="status">
                  <svg viewBox="0 0 24 24" width="14" height="14" aria-hidden="true"><path d="M1 21h22L12 2 1 21zm12-3h-2v-2h2v2zm0-4h-2v-4h2v4z" fill="currentColor"/></svg>
                  <span>{$t('dashboard.partialWarning')}</span>
                </span>
                {#if !a.affinityOk && !a.error}
                  <div class="sub-fail">{$t('dashboard.statusFail')}</div>
                {/if}
                {#if !a.priorityOk}
                  <div class="sub-fail">{$t('dashboard.priorityFail')}</div>
                {/if}
                {#if a.ioOk === false}
                  <div class="sub-fail">{$t('dashboard.ioFail')}</div>
                {/if}
                {#if a.memOk === false}
                  <div class="sub-fail">{$t('dashboard.memFail')}</div>
                {/if}
              {/if}
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
  </div>
{/if}

<style>
  .empty-state {
    text-align: center;
    color: var(--text-secondary);
    padding: var(--space-6);
  }

  .table-scroll {
    overflow-x: auto;
  }

  table {
    width: 100%;
    border-collapse: collapse;
    font-variant-numeric: tabular-nums;
  }

  th {
    text-align: left;
    padding: 7px 10px;
    color: var(--text-secondary);
    font-size: 11px;
    font-weight: var(--font-weight-medium);
    border-bottom: 2px solid var(--border-subtle);
  }

  td {
    padding: 8px 10px;
    border-bottom: 1px solid var(--border-subtle);
    vertical-align: top;
  }

  tr:last-child td {
    border-bottom: none;
  }

  .row-error td {
    background: var(--danger-muted);
  }

  .row-partial td {
    background: var(--warning-muted);
  }

  .cell-exe {
    max-width: 160px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-weight: var(--font-weight-medium);
  }

  .cell-pid {
    white-space: nowrap;
  }

  .cell-rule {
    max-width: 140px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .cell-status {
    min-width: 150px;
  }

  .badge.soft {
    background: var(--accent-muted);
    color: var(--accent);
    margin-right: 4px;
    vertical-align: middle;
  }

  .status-marker {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    font-size: 12px;
    font-weight: var(--font-weight-medium);
    white-space: nowrap;
  }

  .status-marker.ok {
    color: var(--success);
  }

  .status-marker.warn {
    color: var(--warning);
  }

  .status-marker.fail {
    color: var(--danger);
  }

  .sub-fail {
    font-size: 11px;
    color: var(--text-muted);
    margin-top: 2px;
  }

  .mono {
    font-family: 'IBM Plex Sans TC', monospace;
    font-size: 11px;
  }
</style>
