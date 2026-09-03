<script lang="ts">
  import { t } from 'svelte-i18n';
  import type { LogicalProcessor } from '../lib/types';

  let {
    lp,
    usageValue = null,
    covered = false,
    showHt = true,
    interactive = false,
    checked = false,
    recBest = false,
    recSevere = false,
    recExcluded = false,
    ontoggle,
  }: {
    lp: LogicalProcessor;
    usageValue?: number | null;
    covered?: boolean;
    showHt?: boolean;
    interactive?: boolean;
    checked?: boolean;
    recBest?: boolean;
    recSevere?: boolean;
    recExcluded?: boolean;
    ontoggle?: (index: number) => void;
  } = $props();

  let pct = $derived(usageValue == null ? null : Math.round(usageValue * 100));
  let interactiveLabel = $derived(
    `LP${lp.index}${lp.isSmtSibling && showHt ? ' HT' : ''}`,
  );
  let displayLabel = $derived.by(() => {
    const parts: string[] = [`LP${lp.index}`];
    if (lp.isSmtSibling && showHt) parts.push('HT');
    if (covered) parts.push($t('dashboard.coreCellCovered') as string);
    if (pct != null) parts.push(`${pct}%`);
    return parts.join(', ');
  });
</script>

{#if interactive}
  <label
    class="cell interactive"
    class:covered
    class:checked
    class:rec-excluded={recExcluded}
    aria-label={interactiveLabel}
  >
    <input type="checkbox" {checked} onchange={() => ontoggle?.(lp.index)} />
    <span class="idx">LP{lp.index}</span>
    {#if recBest}
      <span class="badge rec-best">{$t('gpuTest.bestTag')}</span>
    {/if}
    {#if showHt && lp.isSmtSibling}
      <span class="badge ht">HT</span>
    {/if}
    {#if recExcluded}
      <span class="badge rec-x" title={$t('ruleImport.excludedLabel')}>✕</span>
    {/if}
  </label>
{:else}
  <div
    class="cell"
    class:covered
    aria-label={displayLabel}
  >
    <div class="row">
      <span class="idx">LP{lp.index}</span>
      {#if showHt && lp.isSmtSibling}
        <span class="badge ht">HT</span>
      {/if}
      {#if pct != null}
        <span class="pct">{pct}%</span>
      {/if}
    </div>
    {#if pct != null}
      <div class="bar"><div class="fill" style:width="{pct}%"></div></div>
    {/if}
  </div>
{/if}

<style>
  .cell {
    flex: 1;
    min-width: 68px;
    background: var(--surface-2);
    border: 1px solid var(--border-subtle);
    border-radius: var(--radius-sm);
    padding: 6px 8px;
  }

  .cell.covered {
    border-color: var(--accent);
    border-width: 2px;
    padding: 5px 7px;
  }

  .cell.interactive {
    display: flex;
    align-items: center;
    gap: 6px;
    cursor: pointer;
    flex: 0 0 auto;
    min-width: 88px;
    transition: border-color var(--transition-fast), background var(--transition-fast);
  }

  .cell.interactive:hover {
    border-color: var(--accent);
  }

  .cell.interactive.checked {
    border-color: var(--accent);
    background: var(--accent-muted);
  }

  .cell.interactive.rec-excluded {
    opacity: 0.45;
    border-style: dashed;
  }

  .badge.rec-best {
    background: var(--accent);
    color: var(--accent-text);
  }

  .badge.rec-x {
    background: var(--surface-0);
    color: var(--text-secondary);
  }

  .row {
    display: flex;
    align-items: center;
    gap: 4px;
  }

  .idx {
    font-size: 11px;
    font-weight: var(--font-weight-medium);
  }

  .pct {
    margin-left: auto;
    font-size: 10px;
    color: var(--text-secondary);
  }

  .badge.ht {
    background: var(--warning-muted);
    color: var(--warning);
  }

  .bar {
    height: 4px;
    margin-top: 4px;
    background: var(--surface-0);
    border-radius: var(--radius-full);
    overflow: hidden;
  }

  .fill {
    height: 100%;
    background: var(--accent);
    transition: width 0.3s;
  }
</style>
