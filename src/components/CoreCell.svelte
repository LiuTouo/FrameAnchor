<script lang="ts">
  import type { LogicalProcessor } from '../lib/types';

  let {
    lp,
    usageValue = null,
    covered = false,
    showHt = true,
    interactive = false,
    checked = false,
    ontoggle,
  }: {
    lp: LogicalProcessor;
    usageValue?: number | null;
    covered?: boolean;
    showHt?: boolean;
    interactive?: boolean;
    checked?: boolean;
    ontoggle?: (index: number) => void;
  } = $props();

  let pct = $derived(usageValue == null ? null : Math.round(usageValue * 100));
</script>

{#if interactive}
  <label class="cell interactive" class:covered class:checked>
    <input type="checkbox" {checked} onchange={() => ontoggle?.(lp.index)} />
    <span class="idx">LP{lp.index}</span>
    {#if showHt && lp.isSmtSibling}
      <span class="badge ht">HT</span>
    {/if}
  </label>
{:else}
  <div class="cell" class:covered title="LP{lp.index}{lp.isSmtSibling ? ' (HT)' : ''}">
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
    min-width: 64px;
    background: var(--panel-2);
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 4px 6px;
  }
  .cell.covered {
    border-color: var(--accent);
  }
  .cell.interactive {
    display: flex;
    align-items: center;
    gap: 5px;
    cursor: pointer;
    flex: 0 0 auto;
    min-width: 84px;
  }
  .cell.interactive:hover {
    border-color: var(--accent);
  }
  .cell.interactive.checked {
    border-color: var(--accent);
    background: rgba(79, 140, 255, 0.12);
  }
  .row {
    display: flex;
    align-items: center;
    gap: 4px;
  }
  .idx {
    font-size: 11px;
    font-weight: 600;
  }
  .pct {
    margin-left: auto;
    font-size: 10px;
    color: var(--muted);
  }
  .badge.ht {
    background: rgba(255, 190, 80, 0.18);
    color: #ffbe50;
  }
  .bar {
    height: 4px;
    margin-top: 4px;
    background: var(--bg);
    border-radius: 2px;
    overflow: hidden;
  }
  .fill {
    height: 100%;
    background: var(--accent);
    transition: width 0.3s;
  }
</style>
