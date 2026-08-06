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
  <div class="empty hint">{$t('dashboard.emptyApplied')}</div>
{:else}
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
        <tr class:failed={a.error != null}>
          <td>{a.exeName}</td>
          <td>{a.pid}</td>
          <td>{a.ruleName}</td>
          <td>{a.softAffinity ? $t('dashboard.softAffinity') + ' ' : ''}{fmtCores(a.currentCores)}</td>
          <td>{a.currentPriority || '—'}</td>
          <td>
            {#if a.error}
              <span class="status fail" title={['ACCESS_DENIED', 'SET_AFFINITY_FAILED'].includes(a.error ?? '') ? $t('errors.anticheatNote') : ''}>
                ✖ {errText(a.error)}
              </span>
            {:else if a.affinityOk && (!a.priorityOk || a.ioOk === false || a.memOk === false)}
              <span class="status warn">⚠ {$t('dashboard.partialWarning')}</span>
              {#if !a.priorityOk}
                <span class="sub fail">{$t('dashboard.priorityFail')}</span>
              {/if}
              {#if a.ioOk === false}
                <span class="sub fail">{$t('dashboard.ioFail')}</span>
              {/if}
              {#if a.memOk === false}
                <span class="sub fail">{$t('dashboard.memFail')}</span>
              {/if}
            {:else}
              <span class="status ok">✔ {$t('dashboard.statusOk')}</span>
            {/if}
          </td>
        </tr>
      {/each}
    </tbody>
  </table>
{/if}

<style>
  .empty {
    padding: 20px;
    text-align: center;
    background: var(--panel);
    border: 1px dashed var(--border);
    border-radius: 8px;
  }
  table {
    width: 100%;
    border-collapse: collapse;
    background: var(--panel);
    border: 1px solid var(--border);
    border-radius: 8px;
    overflow: hidden;
  }
  th,
  td {
    text-align: left;
    padding: 7px 10px;
    border-bottom: 1px solid var(--border);
  }
  th {
    color: var(--muted);
    font-size: 11px;
    font-weight: 600;
  }
  tr:last-child td {
    border-bottom: none;
  }
  tr.failed td {
    background: rgba(255, 95, 107, 0.06);
  }
  .status.ok {
    color: var(--ok);
  }
  .status.warn {
    color: var(--warn, #e6a817);
  }
  .status.fail {
    color: var(--danger);
    cursor: help;
  }
  .sub {
    display: block;
    font-size: 10px;
  }
  .sub.fail {
    color: var(--danger);
  }
</style>
