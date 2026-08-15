<script lang="ts">
  import { onMount } from 'svelte';
  import { t } from 'svelte-i18n';
  import * as ipc from '../lib/ipc';
  import {
    benchmarkProgress,
    benchmarkSessions,
    benchmarkState,
    gpuDevices,
    gpuPolicy,
    topology,
  } from '../lib/stores';
  import type {
    ApplyStatus,
    BenchmarkConfig,
    LpResult,
    SessionDetail,
    WorkloadKind,
  } from '../lib/types';
  import ConfirmDialog from '../components/ConfirmDialog.svelte';

  type Segment = 'test' | 'results' | 'manual';
  type ConfirmAction = 'start' | 'applyBest' | 'restore' | 'deleteHistory' | 'manualApply';

  // ── 區段控制 ──
  let segment = $state<Segment>('test');
  const isRunning = $derived($benchmarkState?.status === 'Running');
  const recoveryRequired = $derived($benchmarkState?.recoveryRequired ?? false);
  // 取消已請求但尚未終結：顯示「取消中」並避免重複點擊。
  // cancelRequested 來自後端 state；cancelSent 是本地立即回饋（不等下一個 progress 事件）。
  const cancelRequested = $derived($benchmarkState?.cancelRequested ?? false);
  let cancelSent = $state(false);
  const cancelPending = $derived(isRunning && (cancelRequested || cancelSent));

  function switchSegment(s: Segment) {
    if (isRunning && s !== segment) return; // 執行中禁止切換
    segment = s;
  }

  // 執行中強制切回 test 區段（reload / 外部狀態重建時保證進度 UI 可達）
  $effect(() => {
    if (isRunning) segment = 'test';
  });

  // ── 表單狀態 ──
  let selectedGpu = $state('');
  let lps = $state<number[]>([]);
  let lpsInitialized = $state(false);
  let workload = $state<WorkloadKind>('Vulkan');
  let warmUpSecs = $state(5);
  let sampleSecs = $state(30);
  let fullscreen = $state(false);
  let vulkanOptionsOpen = $state(true);
  let width = $state(1280);
  let height = $state(720);
  let manualLp = $state<number | null>(null);
  let fpsCap = $state(0);
  let tripleBuffer = $state(false);

  // ── 執行 / 結果狀態 ──
  let confirmAction = $state<ConfirmAction | null>(null);
  let deleteTargetId = $state<string | null>(null);
  let busy = $state(false);
  let errMsg = $state<string | null>(null);
  let detail = $state<SessionDetail | null>(null);
  let applyStatus = $state<ApplyStatus | null>(null);
  let policyLoading = $state(false);
  let handledTerminal = $state<string | null>(null);

  const group0Limit = $derived(Math.min($topology?.totalLp ?? 0, 64));
  const supportedLps = $derived(
    $topology
      ? $topology.logicalProcessors
          .map((p) => p.index)
          .filter((lp) => lp < group0Limit)
          .sort((a, b) => a - b)
      : [],
  );
  // 基準測試候選 LP：排除實體 Core 0（含其 SMT sibling），僅供測試晶片與「全部」按鈕使用。
  // 手動 GPU 親和性下拉選單仍使用上方完整的 supportedLps（含 Core 0）。
  const benchmarkLps = $derived(
    $topology
      ? $topology.logicalProcessors
          .filter((p) => p.index < group0Limit && p.coreId !== 0)
          .map((p) => p.index)
          .sort((a, b) => a - b)
      : [],
  );
  const lpList = $derived(lps);
  // 固定調適排程：前 3 round 測所有選定 LP，後 2 round 只重測篩選前 2 名。
  const restartCount = $derived(lpList.length * 3 + Math.min(lpList.length, 2) * 2);
  const estMinutes = $derived(
    Math.max(1, Math.round((restartCount * (sampleSecs + warmUpSecs + 19)) / 60)),
  );
  const policyGpu = $derived(detail?.summary.gpuInstanceId || selectedGpu);
  const policyLp = $derived(
    $gpuPolicy ? maskToLp($gpuPolicy.assignmentSetOverride?.bytes ?? null) : null,
  );
  const policyDevicePolicy = $derived(
    $gpuPolicy ? bytesToU32($gpuPolicy.devicePolicy?.bytes ?? null) : null,
  );
  const results = $derived(detail?.results ?? []);
  const reliability = $derived(detail?.summary.reliability ?? null);
  const relStatus = $derived(reliability?.status ?? 'Unassessed');
  const isPassed = $derived(relStatus === 'Passed');

  // ── 初始化 ──
  $effect(() => {
    const topo = $topology;
    if (topo && !lpsInitialized) {
      const limit = Math.min(topo.totalLp, 64);
      if (topo.hasHybrid) {
        lps = topo.physicalCores
          .filter((c) => c.isPCore && c.id !== 0)
          .flatMap((c) => c.lpIndices.slice(0, 1))
          .filter((lp) => lp < limit)
          .sort((a, b) => a - b);
      } else {
        lps = topo.physicalCores
          .filter((c) => c.id !== 0)
          .flatMap((c) => c.lpIndices.slice(0, 1))
          .filter((lp) => lp < limit)
          .sort((a, b) => a - b);
      }
      if (lps.length === 0) {
        // 回退僅使用基準測試候選 LP（仍排除 Core 0），不還原 Core 0。
        lps = [...benchmarkLps];
      }
      manualLp = lps[0] ?? null;
      lpsInitialized = true;
    }
  });
  $effect(() => { if (selectedGpu) void refreshPolicyFor(selectedGpu); });
  // 終結 transition → 載入結果、切換至 results 區段
  $effect(() => {
    const st = $benchmarkState;
    if (!st) return;
    if (
      (st.status === 'Completed' || st.status === 'Failed' || st.status === 'Cancelled') &&
      st.sessionId && st.sessionId !== handledTerminal
    ) {
      handledTerminal = st.sessionId;
      loadDetail(st.sessionId).then(() => {
        refreshHistory();
        segment = 'results';
      });
    }
  });

  onMount(() => { void init(); });

  async function init() {
    try {
      gpuDevices.set(await ipc.enumerateGpus());
      benchmarkSessions.set(await ipc.listBenchmarkSessions());
      const st = await ipc.getBenchmarkState();
      benchmarkState.set(st);
      if (!selectedGpu && $gpuDevices.length) selectedGpu = $gpuDevices[0].instanceId;
      if ((st.status === 'Completed' || st.status === 'Failed' || st.status === 'Cancelled') && st.sessionId) {
        handledTerminal = st.sessionId;
        await loadDetail(st.sessionId);
      }
    } catch (e) { errMsg = String(e); }
  }

  async function refreshHistory() { benchmarkSessions.set(await ipc.listBenchmarkSessions()); }

  async function loadDetail(id: string) {
    try {
      detail = await ipc.getBenchmarkSession(id);
      applyStatus = await ipc.getBenchmarkApplyStatus(id);
      if (detail) await refreshPolicyFor(detail.summary.gpuInstanceId || selectedGpu);
    } catch (e) { errMsg = String(e); }
  }

  async function refreshPolicyFor(instanceId: string) {
    if (!instanceId) return;
    policyLoading = true;
    try { gpuPolicy.set(await ipc.getGpuAffinityPolicy(instanceId)); }
    catch { gpuPolicy.set(null); }
    finally { policyLoading = false; }
  }

  function toggleLp(i: number) {
    lps = lps.includes(i) ? lps.filter((x) => x !== i) : [...lps, i].sort((a, b) => a - b);
  }

  // ── 開始/取消 ──
  function startClicked() {
    errMsg = null;
    if (!selectedGpu) return void (errMsg = $t('gpuTest.errSelectGpu') as string);
    if (lpList.length === 0) return void (errMsg = $t('gpuTest.errSelectLp') as string);
    if (sampleSecs <= 0) return void (errMsg = $t('gpuTest.errSample') as string);
    if (width <= 0 || height <= 0) return void (errMsg = $t('gpuTest.errDimensions') as string);
    confirmAction = 'start';
  }

  async function doStart() {
    confirmAction = null; busy = true; cancelSent = false;
    try {
      await ipc.startGpuBenchmark({ candidateLps: lps, gpuInstanceId: selectedGpu, workload, warmUpSecs, sampleSecs, repetitions: 5, syncWorkloadAffinity: false, fullscreen, width, height, fpsCap, tripleBuffer, vulkanArgs: buildVulkanArgs(), workloadExePath: null, presentmonPath: null, gamePath: null, windowTitle: null });
      errMsg = null;
    } catch (e) { errMsg = String(e); }
    finally { busy = false; }
  }

  async function doCancel() {
    if (cancelPending) return; // 已請求取消，避免重複送出
    busy = true;
    try { await ipc.cancelBenchmark(); cancelSent = true; }
    catch (e) { errMsg = String(e); }
    finally { busy = false; }
  }

  function buildVulkanArgs(): string[] {
    return [`--fullscreen=${fullscreen ? 1 : 0}`, `--width=${width}`, `--height=${height}`, `--fps_cap=${fpsCap}`, `--triple_buffering=${tripleBuffer ? 1 : 0}`];
  }

  // ── GPU 策略 ──
  async function applyBest() { if (detail?.summary.bestLp != null) confirmAction = 'applyBest'; }
  async function confirmApplyBest() {
    if (detail?.summary.bestLp == null) return;
    confirmAction = null; busy = true;
    try { await ipc.applyBestGpuAffinity(detail.summary.id); errMsg = null; await refreshPolicyFor(detail!.summary.gpuInstanceId || selectedGpu); }
    catch (e) { errMsg = String(e); }
    finally { busy = false; }
  }

  async function restorePrevious() { confirmAction = 'restore'; }
  async function confirmRestorePrevious() {
    confirmAction = null; busy = true;
    try { await ipc.restorePreviousGpuAffinity(); errMsg = null; await refreshPolicyFor(policyGpu); }
    catch (e) { errMsg = String(e); }
    finally { busy = false; }
  }

  // ── 手動套用 GPU 中斷親和性 ──
  async function manualApplyClicked() { if (manualLp != null && selectedGpu) confirmAction = 'manualApply'; }
  async function confirmManualApply() {
    if (manualLp == null || !selectedGpu) return;
    confirmAction = null; busy = true;
    try { await ipc.applyGpuAffinity(selectedGpu, manualLp); errMsg = null; await refreshPolicyFor(selectedGpu); }
    catch (e) { errMsg = String(e); }
    finally { busy = false; }
  }

  // ── 歷史 ──
  async function openHistory(id: string) { await loadDetail(id); }
  async function deleteHistory(id: string) { deleteTargetId = id; confirmAction = 'deleteHistory'; }
  async function confirmDeleteHistory() {
    if (!deleteTargetId) return;
    const id = deleteTargetId; confirmAction = null; deleteTargetId = null;
    try {
      await ipc.deleteBenchmarkSession(id);
      if (detail?.summary.id === id) { detail = null; applyStatus = null; }
      await refreshHistory();
    } catch (e) { errMsg = String(e); }
  }

  // ── 顯示輔助 ──
  function errText(code: string | null): string {
    if (!code) return '';
    const key = `errors.${code}`;
    const localized = $t(key) as string;
    return localized === key ? code : localized;
  }

  function statusLabel(s: string): string {
    switch (s) {
      case 'Completed': return $t('gpuTest.statusCompleted') as string;
      case 'Cancelled': return $t('gpuTest.statusCancelled') as string;
      case 'Failed': return $t('gpuTest.statusFailed') as string;
      default: return s;
    }
  }

  function relStatusLabel(s: string): string {
    switch (s) {
      case 'Passed': return $t('gpuTest.reliabilityPassed') as string;
      case 'Equivalent': return $t('gpuTest.reliabilityEquivalent') as string;
      case 'Inconclusive': return $t('gpuTest.reliabilityInconclusive') as string;
      default: return $t('gpuTest.reliabilityUnassessed') as string;
    }
  }

  function relSummaryText(s: string): string {
    switch (s) {
      case 'Passed': return $t('gpuTest.reliabilityPassedSummary') as string;
      case 'Equivalent': return $t('gpuTest.reliabilityEquivalentSummary') as string;
      case 'Inconclusive': return $t('gpuTest.reliabilityInconclusiveSummary') as string;
      default: return $t('gpuTest.reliabilityUnassessedSummary') as string;
    }
  }

  function fmtPct(v: number | null | undefined): string {
    if (v == null) return '—';
    return `${v > 0 ? '+' : ''}${v.toFixed(2)}%`;
  }

  function fmtDeltaPp(v: number | null | undefined): string {
    if (v == null) return '—';
    return `${v > 0 ? '+' : ''}${v.toFixed(2)} pp`;
  }

  function fmtPctVal(v: number | null | undefined): string {
    return v == null ? '—' : `${v.toFixed(2)}%`;
  }

  function stageLabel(stage: string | undefined): string {
    switch (stage) {
      case 'Init': return $t('gpuTest.stageStarting') as string;
      case 'Warmup': return $t('gpuTest.stageApplying') as string;
      case 'Collecting': return $t('gpuTest.stageCollecting') as string;
      case 'Finalizing': return $t('gpuTest.stageFinalizing') as string;
      default: return stage ?? '';
    }
  }

  function bytesToU32(bytes: number[] | null): number | null {
    if (!bytes || bytes.length !== 4) return null;
    return (bytes[0] | (bytes[1] << 8) | (bytes[2] << 16) | (bytes[3] << 24)) >>> 0;
  }

  function maskToLp(bytes: number[] | null): number | null {
    if (!bytes || bytes.length === 0) return null;
    let v = 0n;
    for (let i = 0; i < bytes.length; i++) v |= BigInt(bytes[i]) << BigInt(i * 8);
    let found: number | null = null;
    for (let i = 0; i < 64; i++) {
      if ((v & (1n << BigInt(i))) !== 0n) {
        if (found !== null) return null;
        found = i;
      }
    }
    return found;
  }

  function fmtBytes(n: number): string {
    if (n >= 1024 * 1024) return `${(n / (1024 * 1024)).toFixed(1)} MB`;
    if (n >= 1024) return `${(n / 1024).toFixed(1)} KB`;
    return `${n} B`;
  }

  // ── 指標排名顯示 ──
  function colBest(col: (r: LpResult) => number | null, higher: boolean) {
    const vals = results.map(col).filter((v): v is number => v != null);
    if (!vals.length) return { first: null as number | null, second: null as number | null };
    const sorted = [...vals].sort((a, b) => (higher ? b - a : a - b));
    return { first: sorted[0], second: sorted.find((v) => v !== sorted[0]) ?? null };
  }

  const bestAvg = $derived(colBest((r) => r.avgFps, true));
  const bestMax = $derived(colBest((r) => r.maxFps, true));
  const bestMin = $derived(colBest((r) => r.minFps, true));
  const bestStdev = $derived(colBest((r) => r.stdevFps, false));
  const bestP1 = $derived(colBest((r) => r.p1Low, true));
  const bestP01 = $derived(colBest((r) => r.p01Low, true));
  const bestP001 = $derived(colBest((r) => r.p001Low, true));
  const bestP0005 = $derived(colBest((r) => r.p0005Low, true));
  const bestP1Pct = $derived(colBest((r) => r.p1Percentile, true));
  const bestP01Pct = $derived(colBest((r) => r.p01Percentile, true));
  const bestP001Pct = $derived(colBest((r) => r.p001Percentile, true));
  const bestP0005Pct = $derived(colBest((r) => r.p0005Percentile, true));
  const bestMad = $derived(colBest((r) => r.frametimeMadPct, false));
  const bestSpike = $derived(colBest((r) => r.spikeRatePct, false));

  function median(values: number[]): number | null {
    if (!values.length) return null;
    const sorted = [...values].sort((a, b) => a - b);
    const mid = Math.floor(sorted.length / 2);
    return sorted.length % 2 ? sorted[mid] : (sorted[mid - 1] + sorted[mid]) / 2;
  }

  const unusualThresholds = $derived.by(() => {
    const complete = results.filter((r) => r.completed && r.avgFps != null && r.p1Low != null && r.p01Low != null && r.stdevFps != null);
    return {
      avgFps: median(complete.map((r) => r.avgFps as number)),
      p1Low: median(complete.map((r) => r.p1Low as number)),
      p01Low: median(complete.map((r) => r.p01Low as number)),
      stdevFps: median(complete.map((r) => r.stdevFps as number)),
    };
  });

  function isUnusual(r: LpResult, metric: 'avgFps' | 'p1Low' | 'p01Low' | 'stdevFps') {
    const value = r[metric];
    const threshold = unusualThresholds[metric];
    if (!r.completed || value == null || threshold == null) return false;
    if (metric === 'stdevFps') return threshold > 0 && value > threshold * 1.5;
    return value < threshold * 0.85;
  }

  const colMeta = $derived([
    { key: 'avgFps', label: 'colAvg', best: bestAvg },
    { key: 'maxFps', label: 'colMax', best: bestMax },
    { key: 'minFps', label: 'colMin', best: bestMin },
    { key: 'stdevFps', label: 'colStdev', best: bestStdev },
    { key: 'p1Percentile', label: 'colP1Pct', best: bestP1Pct },
    { key: 'p01Percentile', label: 'colP01Pct', best: bestP01Pct },
    { key: 'p001Percentile', label: 'colP001Pct', best: bestP001Pct },
    { key: 'p0005Percentile', label: 'colP0005Pct', best: bestP0005Pct },
    { key: 'p1Low', label: 'colP1', best: bestP1 },
    { key: 'p01Low', label: 'colP01', best: bestP01 },
    { key: 'p001Low', label: 'colP001', best: bestP001 },
    { key: 'p0005Low', label: 'colP0005', best: bestP0005 },
    { key: 'frametimeMadPct', label: 'colMad', best: bestMad },
    { key: 'spikeRatePct', label: 'colSpikeRate', best: bestSpike },
  ]);

  function cellClass(v: number | null | undefined, best: { first: number | null; second: number | null }, unusual = false) {
    if (v == null) return '';
    const rank = v === best.first ? 'best' : v === best.second ? 'second' : '';
    return unusual ? `${rank} unusual-value`.trim() : rank;
  }

  const progress = $derived($benchmarkProgress);
  const currentRound = $derived(progress?.round ?? null);
  const etaMin = $derived(progress?.etaSecs ? Math.max(1, Math.round(progress.etaSecs / 60)) : null);
  const canApply = $derived(applyStatus?.canApply ?? false);
</script>

<!-- ═══════════════════════════════════════════════════════════════════════ -->
<!-- 區段切換控制                                                           -->
<!-- ═══════════════════════════════════════════════════════════════════════ -->
<div class="gpu-test">
  <div class="segment-bar" role="tablist" aria-label={$t('nav.gpuTest')}>
    <button
      class="segment-btn"
      class:active={segment === 'test'}
      role="tab"
      aria-selected={segment === 'test'}
      disabled={isRunning}
      onclick={() => switchSegment('test')}
    >
      <svg viewBox="0 0 24 24" width="16" height="16" aria-hidden="true"><path d="M8 5v14l11-7z" fill="currentColor"/></svg>
      {$t('gpuTest.testTab')}
    </button>
    <button
      class="segment-btn"
      class:active={segment === 'results'}
      role="tab"
      aria-selected={segment === 'results'}
      disabled={isRunning}
      onclick={() => switchSegment('results')}
    >
      <svg viewBox="0 0 24 24" width="16" height="16" aria-hidden="true"><path d="M19 3H5c-1.1 0-2 .9-2 2v14c0 1.1.9 2 2 2h14c1.1 0 2-.9 2-2V5c0-1.1-.9-2-2-2zM9 17H7v-7h2v7zm4 0h-2V7h2v10zm4 0h-2v-4h2v4z" fill="currentColor"/></svg>
      {$t('gpuTest.resultsTab')}
    </button>
    <button
      class="segment-btn"
      class:active={segment === 'manual'}
      role="tab"
      aria-selected={segment === 'manual'}
      disabled={isRunning}
      onclick={() => switchSegment('manual')}
    >
      <svg viewBox="0 0 24 24" width="16" height="16" aria-hidden="true"><path d="M19.14 12.94c.04-.3.06-.61.06-.94 0-.32-.02-.64-.07-.94l2.03-1.58c.18-.14.23-.41.12-.61l-1.92-3.32c-.12-.22-.37-.29-.59-.22l-2.39.96c-.5-.38-1.03-.7-1.62-.94l-.36-2.54c-.04-.24-.24-.41-.48-.41h-3.84c-.24 0-.43.17-.47.41l-.36 2.54c-.59.24-1.13.57-1.62.94l-2.39-.96c-.22-.08-.47 0-.59.22L2.74 8.87c-.12.21-.08.47.12.61l2.03 1.58c-.05.3-.09.63-.09.94s.02.64.07.94l-2.03 1.58c-.18.14-.23.41-.12.61l1.92 3.32c.12.22.37.29.59.22l2.39-.96c.5.38 1.03.7 1.62.94l.36 2.54c.05.24.24.41.48.41h3.84c.24 0 .44-.17.47-.41l.36-2.54c.59-.24 1.13-.56 1.62-.94l2.39.96c.22.08.47 0 .59-.22l1.92-3.32c.12-.22.07-.47-.12-.61l-2.01-1.58zM12 15.6c-1.98 0-3.6-1.62-3.6-3.6s1.62-3.6 3.6-3.6 3.6 1.62 3.6 3.6-1.62 3.6-3.6 3.6z" fill="currentColor"/></svg>
      {$t('gpuTest.manualTab')}
    </button>
  </div>

  {#if recoveryRequired}
    <div class="recovery-banner" role="alert">
      <svg viewBox="0 0 24 24" width="16" height="16" aria-hidden="true"><path d="M1 21h22L12 2 1 21zm12-3h-2v-2h2v2zm0-4h-2v-4h2v4z" fill="currentColor"/></svg>
      {$t('gpuTest.recoveryBanner')}
    </div>
  {/if}

  <!-- ═══════════════════════════════════════════════════════════════════ -->
  <!-- 測試區段                                                             -->
  <!-- ═══════════════════════════════════════════════════════════════════ -->
  {#if segment === 'test'}
    {#if isRunning}
      <!-- 執行中：精簡進度 UI -->
      <section class="panel running-panel" aria-live="polite">
        <h2>{$t('gpuTest.runningTitle')}</h2>
        <div class="active-warning" role="alert">
          <strong>{$t('gpuTest.activeWarningZh')}</strong>
          <span class="hint">{$t('gpuTest.activeWarningEn')}</span>
        </div>
        <dl class="running-meta">
          <div><dt>{$t('gpuTest.gpuSelect')}</dt><dd>{$gpuDevices.find((d) => d.instanceId === selectedGpu)?.friendlyName ?? selectedGpu}</dd></div>
          {#if currentRound != null}<div><dt>{$t('gpuTest.repetitions')}</dt><dd>{$t('gpuTest.round', { values: { round: currentRound + 1 } })}</dd></div>{/if}
          <div><dt>{$t('gpuTest.currentLp')}</dt><dd>{$benchmarkState?.currentLp ?? '—'}</dd></div>
          <div><dt>{$t('gpuTest.progress')}</dt><dd>{$benchmarkState?.progressPct ?? 0}%</dd></div>
          {#if etaMin != null}<div><dt>{$t('gpuTest.eta')}</dt><dd>{$t('gpuTest.riskEstimate', { values: { minutes: etaMin } })}</dd></div>{/if}
          <div><dt>{$t('gpuTest.colStatus')}</dt><dd>{stageLabel($benchmarkState?.stage)}</dd></div>
        </dl>
        <div class="progress-track" aria-hidden="true"><div style="width: {($benchmarkState?.progressPct ?? 0)}%"></div></div>
        <div class="action-row"><button class="danger" disabled={busy || cancelPending} onclick={doCancel}>{cancelPending ? $t('gpuTest.cancelling') : $t('gpuTest.cancel')}</button></div>
      </section>
    {:else}
      <!-- 設定表單 -->
      <section class="panel">
        <h2>{$t('gpuTest.start')}</h2>
        <div class="form-grid">
          <!-- GPU + Workload -->
          <label class="field">
            <span class="field-label">{$t('gpuTest.gpuSelect')}</span>
            {#if $gpuDevices.length === 0}
              <span class="hint">{$t('gpuTest.noGpu')}</span>
            {:else}
              <select bind:value={selectedGpu}>
                {#each $gpuDevices as d (d.instanceId)}
                  <option value={d.instanceId}>{d.friendlyName}</option>
                {/each}
              </select>
            {/if}
          </label>
          <label class="field">
            <span class="field-label">{$t('gpuTest.workload')}</span>
            <select bind:value={workload}>
              <option value="Vulkan">Vulkan</option>
              <option value="D3D9">Direct3D 9</option>
            </select>
          </label>

          <!-- 核心選擇 -->
          <div class="field full-width">
            <span class="field-label">{$t('gpuTest.lpSelect')}</span>
            <div class="lp-chips" role="group" aria-label={$t('gpuTest.lpSelect')}>
              <button class:selected={lps.length === benchmarkLps.length} onclick={() => (lps = [...benchmarkLps])} type="button">{$t('gpuTest.allLps')}</button>
              {#each benchmarkLps as i (i)}
                <button class:selected={lps.includes(i)} onclick={() => toggleLp(i)} type="button" aria-pressed={lps.includes(i)}>{i}</button>
              {/each}
            </div>
          </div>

          <!-- 時間參數 -->
          <label class="field"><span class="field-label">{$t('gpuTest.warmup')}</span><input type="number" bind:value={warmUpSecs} min="0" /></label>
          <label class="field"><span class="field-label">{$t('gpuTest.sample')}</span><input type="number" bind:value={sampleSecs} min="1" /></label>
          <div class="field full-width"><span class="hint">{$t('gpuTest.adaptiveSchedule')}</span></div>

          <!-- Vulkan 專屬：可折疊次要選項 -->
          {#if workload === 'Vulkan'}
            <div class="field full-width vulkan-group">
              <button class="section-toggle" onclick={() => (vulkanOptionsOpen = !vulkanOptionsOpen)} type="button">
                <svg viewBox="0 0 24 24" width="12" height="12" aria-hidden="true" class:rotated={vulkanOptionsOpen}><path d="M8 5l8 7-8 7" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round"/></svg>
                {$t('gpuTest.vulkanOptions')}
              </button>
              {#if vulkanOptionsOpen}
                <div class="vulkan-opts">
                  <label class="field check"><input type="checkbox" bind:checked={fullscreen} /><span>{$t('gpuTest.fullscreen')}</span></label>
                  <label class="field"><span class="field-label">{$t('gpuTest.width')}</span><input type="number" bind:value={width} min="1" disabled={fullscreen} /></label>
                  <label class="field"><span class="field-label">{$t('gpuTest.height')}</span><input type="number" bind:value={height} min="1" disabled={fullscreen} /></label>
                  {#if fullscreen}<span class="hint full-width">{$t('gpuTest.fullscreenResolutionHint')}</span>{/if}
                  <label class="field"><span class="field-label">{$t('gpuTest.fpsCap')}</span><input type="number" bind:value={fpsCap} min="0" /></label>
                  <label class="field check"><input type="checkbox" bind:checked={tripleBuffer} /><span>{$t('gpuTest.tripleBuffer')}</span></label>
                </div>
              {/if}
            </div>
          {/if}
        </div>

        {#if errMsg}<div class="error-msg" role="alert"><svg viewBox="0 0 24 24" width="14" height="14" aria-hidden="true"><path d="M12 2C6.48 2 2 6.48 2 12s4.48 10 10 10 10-4.48 10-10S17.52 2 12 2zm1 15h-2v-2h2v2zm0-4h-2V7h2v6z" fill="currentColor"/></svg>{errText(errMsg)}</div>{/if}

        <div class="action-row">
          <span class="hint">{$t('gpuTest.riskEstimate', { values: { minutes: estMinutes } })} · {$t('gpuTest.restartCount', { values: { count: restartCount } })}</span>
          <button class="primary" disabled={busy || recoveryRequired || $gpuDevices.length === 0} onclick={startClicked}>{$t('gpuTest.start')}</button>
        </div>
      </section>
    {/if}
  {:else if segment === 'results'}
  <!-- ═══════════════════════════════════════════════════════════════════ -->
  <!-- 結果與歷史區段：master-detail                                      -->
  <!-- ═══════════════════════════════════════════════════════════════════ -->
    <div class="results-workspace">
      <!-- 左側：session 清單 -->
      <div class="session-list" role="list" aria-label={$t('gpuTest.historyTitle')}>
        <div class="session-list-head">
          <span class="hint">{$t('gpuTest.storageInfo', { values: { bytes: fmtBytes($benchmarkSessions.reduce((a, s) => a + s.totalBytes, 0)), count: $benchmarkSessions.length } })}</span>
        </div>
        {#if $benchmarkSessions.length === 0}
          <div class="empty-hint hint">{$t('gpuTest.emptyHistory')}</div>
        {:else}
          {#each $benchmarkSessions as s (s.id)}
            <div role="listitem">
              <button
                class="session-item"
                class:active={detail?.summary.id === s.id}
                onclick={() => openHistory(s.id)}
                aria-current={detail?.summary.id === s.id ? 'true' : undefined}
              >
                <div class="session-item-top">
                  <span class="session-date">{s.startedAt.replace('T', ' ').slice(0, 16)}</span>
                  <span class="badge status-{s.status.toLowerCase()}" aria-label={statusLabel(s.status)}>
                    <svg viewBox="0 0 24 24" width="10" height="10" aria-hidden="true">
                      {#if s.status === 'Completed'}<path d="M12 2C6.48 2 2 6.48 2 12s4.48 10 10 10 10-4.48 10-10S17.52 2 12 2zm-2 15l-5-5 1.41-1.41L10 14.17l7.59-7.59L19 8l-9 9z" fill="currentColor"/>
                      {:else if s.status === 'Failed'}<path d="M12 2C6.48 2 2 6.48 2 12s4.48 10 10 10 10-4.48 10-10S17.52 2 12 2zm1 15h-2v-2h2v2zm0-4h-2V7h2v6z" fill="currentColor"/>
                      {:else}<path d="M12 2C6.48 2 2 6.48 2 12s4.48 10 10 10 10-4.48 10-10S17.52 2 12 2zm-2 15l-5-5 1.41-1.41L10 14.17l7.59-7.59L19 8l-9 9z" fill="currentColor"/>{/if}
                    </svg>
                    {statusLabel(s.status)}
                  </span>
                </div>
                <div class="session-item-meta">
                  <span class="hint">{s.gpuName || s.gpuInstanceId}</span>
                  {#if s.bestLp != null}<span class="hint">{$t(s.reliability?.status === 'Passed' ? 'gpuTest.bestTag' : 'gpuTest.candidateTag')}: LP{s.bestLp}</span>{/if}
                </div>
                <div class="session-item-foot">
                  <span class="hint">{s.config.workload}</span>
                  <span class="hint">{fmtBytes(s.totalBytes)}</span>
                </div>
              </button>
            </div>
          {/each}
        {/if}
      </div>

      <!-- 右側：詳細結果 -->
      <div class="session-detail">
        {#if detail}
          <!-- 標題 + 狀態 -->
          <div class="detail-head">
            <h2>{$t('gpuTest.resultsTitle')}</h2>
            <div class="detail-head-badges">
              <span class="badge status-{detail.summary.status.toLowerCase()}">
                <svg viewBox="0 0 24 24" width="10" height="10" aria-hidden="true">
                  {#if detail.summary.status === 'Completed'}<path d="M12 2C6.48 2 2 6.48 2 12s4.48 10 10 10 10-4.48 10-10S17.52 2 12 2zm-2 15l-5-5 1.41-1.41L10 14.17l7.59-7.59L19 8l-9 9z" fill="currentColor"/>
                  {:else if detail.summary.status === 'Failed'}<path d="M12 2C6.48 2 2 6.48 2 12s4.48 10 10 10 10-4.48 10-10S17.52 2 12 2zm1 15h-2v-2h2v2zm0-4h-2V7h2v6z" fill="currentColor"/>
                  {:else}<path d="M12 2C6.48 2 2 6.48 2 12s4.48 10 10 10 10-4.48 10-10S17.52 2 12 2zm-2 15l-5-5 1.41-1.41L10 14.17l7.59-7.59L19 8l-9 9z" fill="currentColor"/>{/if}
                </svg>
                {statusLabel(detail.summary.status)}
              </span>
              {#if detail.summary.status === 'Failed' && detail.summary.bestLp == null && detail.results.length > 0}
                <span class="badge warn">{$t('gpuTest.partialError')}</span>
              {/if}
              <button class="small danger" onclick={() => deleteHistory(detail!.summary.id)}>{$t('gpuTest.delete')}</button>
            </div>
          </div>

          {#if detail.summary.status === 'Failed' && detail.summary.error}
            <div class="error-msg" role="alert">{errText(detail.summary.error)}</div>
          {/if}

          <!-- 指標表格 -->
          {#if detail.results.length > 0}
            <div class="table-scroll">
              <table class="metric-table">
                <thead><tr><th>{$t('gpuTest.colLp')}</th>{#each colMeta as c (c.key)}<th>{$t(`gpuTest.${c.label}`)}</th>{/each}<th>{$t('gpuTest.colSamples')}</th></tr></thead>
                <tbody>
                  {#each detail.results as r (r.lp)}
                    <tr>
                      <td class="lp-cell">{r.lp}{#if r.lp === detail.summary.bestLp}<span class="badge best">{$t(isPassed ? 'gpuTest.bestTag' : 'gpuTest.candidateTag')}</span>{/if}</td>
                      <td class={cellClass(r.avgFps, bestAvg, isUnusual(r, 'avgFps'))}>{r.avgFps?.toFixed(1) ?? '—'}</td>
                      <td class={cellClass(r.maxFps, bestMax)}>{r.maxFps?.toFixed(1) ?? '—'}</td>
                      <td class={cellClass(r.minFps, bestMin)}>{r.minFps?.toFixed(1) ?? '—'}</td>
                      <td class={cellClass(r.stdevFps, bestStdev, isUnusual(r, 'stdevFps'))}>{r.stdevFps?.toFixed(1) ?? '—'}</td>
                      <td class={cellClass(r.p1Percentile, bestP1Pct)}>{r.p1Percentile?.toFixed(1) ?? '—'}</td>
                      <td class={cellClass(r.p01Percentile, bestP01Pct)}>{r.p01Percentile?.toFixed(1) ?? '—'}</td>
                      <td class={cellClass(r.p001Percentile, bestP001Pct)}>{r.p001Percentile?.toFixed(1) ?? '—'}</td>
                      <td class={cellClass(r.p0005Percentile, bestP0005Pct)}>{r.p0005Percentile?.toFixed(1) ?? '—'}</td>
                      <td class={cellClass(r.p1Low, bestP1, isUnusual(r, 'p1Low'))}>{r.p1Low?.toFixed(1) ?? '—'}</td>
                      <td class={cellClass(r.p01Low, bestP01, isUnusual(r, 'p01Low'))}>{r.p01Low?.toFixed(1) ?? '—'}</td>
                      <td class={cellClass(r.p001Low, bestP001)}>{r.p001Low?.toFixed(1) ?? '—'}</td>
                      <td class={cellClass(r.p0005Low, bestP0005)}>{r.p0005Low?.toFixed(1) ?? '—'}</td>
                      <td class={cellClass(r.frametimeMadPct, bestMad)}>{fmtPctVal(r.frametimeMadPct)}</td>
                      <td class={cellClass(r.spikeRatePct, bestSpike)}>{fmtPctVal(r.spikeRatePct)}</td>
                      <td>{r.sampleCount}</td>
                    </tr>
                  {/each}
                </tbody>
              </table>
            </div>
          {:else}
            <div class="hint">{$t('gpuTest.emptyResults')}</div>
          {/if}

          <!-- 中繼資料 -->
          <div class="meta-strip">
            <div class="meta-item"><span class="hint">{$t('gpuTest.metaGpu')}</span><span>{detail.summary.gpuName || detail.summary.gpuInstanceId}</span></div>
            <div class="meta-item"><span class="hint">{$t('gpuTest.metaCpuFp')}</span><span class="mono" title={detail.summary.cpuFingerprint}>{detail.summary.cpuFingerprint.slice(0, 12)}…</span></div>
            <div class="meta-item"><span class="hint">{$t('gpuTest.metaApi')}</span><span>{detail.summary.config.workload}</span></div>
            {#if detail.summary.bestLp != null}<div class="meta-item"><span class="hint">{$t(isPassed ? 'gpuTest.colBest' : 'gpuTest.colCandidate')}</span><span>{detail.summary.bestLp}</span></div>{/if}
          </div>

          <!-- 可靠性摘要 -->
          {#if detail.summary.status === 'Completed' && reliability}
            <div class="reliability-block" class:rel-passed={relStatus === 'Passed'} class:rel-equivalent={relStatus === 'Equivalent'} class:rel-inconclusive={relStatus === 'Inconclusive'}>
              <div class="rel-head">
                <span class="badge rel-badge">{relStatusLabel(relStatus)}</span>
                <span class="hint">{relSummaryText(relStatus)}</span>
              </div>
              {#if relStatus !== 'Unassessed'}
                <div class="rel-rounds">
                  {#each reliability.perRoundWinners as w, i (i)}
                    <span class="rel-round">{$t('gpuTest.round', { values: { round: i + 1 } })}：{w != null ? `LP ${w}` : '—'}</span>
                  {/each}
                </div>
                <div class="rel-meta">{$t('gpuTest.reliabilityWins', { values: { wins: reliability.candidateWins, evaluated: reliability.evaluatedRounds, required: reliability.requiredWins } })}</div>
                {#if reliability.compositeAdvantagePct != null || reliability.avgFpsAdvantagePct != null || reliability.p1LowAdvantagePct != null || reliability.spikeRateDeltaPp != null}
                  <div class="rel-evidence">
                    <span class="hint">{$t('gpuTest.reliabilityEvidence')}</span>
                    {#if reliability.compositeAdvantagePct != null}<span>{$t('gpuTest.evidenceComposite')} {fmtPct(reliability.compositeAdvantagePct)}</span>{/if}
                    {#if reliability.avgFpsAdvantagePct != null}<span>{$t('gpuTest.colAvg')} {fmtPct(reliability.avgFpsAdvantagePct)}</span>{/if}
                    {#if reliability.p1LowAdvantagePct != null}<span>{$t('gpuTest.colP1')} {fmtPct(reliability.p1LowAdvantagePct)}</span>{/if}
                    {#if reliability.spikeRateDeltaPp != null}<span>{$t('gpuTest.evidenceSpike')} {fmtDeltaPp(reliability.spikeRateDeltaPp)}</span>{/if}
                  </div>
                  <div class="hint rel-note">{$t('gpuTest.reliabilityEvidenceNote')}</div>
                {/if}
                {#if reliability.avgFpsPct != null || reliability.p1LowPct != null || reliability.p01LowPct != null}
                  <div class="rel-pcts">
                    <span class="hint">{$t('gpuTest.reliabilityImprovement')}</span>
                    <span>{$t('gpuTest.colAvg')} {fmtPct(reliability.avgFpsPct)}</span>
                    <span>{$t('gpuTest.colP1')} {fmtPct(reliability.p1LowPct)}</span>
                    <span>{$t('gpuTest.colP01')} {fmtPct(reliability.p01LowPct)}</span>
                  </div>
                {/if}
              {/if}
            </div>
          {/if}

          <!-- 套用最佳 + GPU 策略 -->
          <div class="policy-section">
            <div class="policy-header">
              <h3>{$t('gpuTest.policyTitle')}</h3>
              <span class="hint">{$t('gpuTest.policyCurrent')}</span>
            </div>
            {#if policyLoading}
              <span class="hint">{$t('gpuTest.policyLoading')}</span>
            {:else if $gpuPolicy}
              <dl class="policy-list">
                <div><dt>{$t('gpuTest.policyDevicePolicy')}</dt><dd>{policyDevicePolicy ?? '—'}</dd></div>
                <div><dt>{$t('gpuTest.policyOverride')}</dt><dd class="mono">{$gpuPolicy.assignmentSetOverride?.bytes?.map((b) => b.toString(16).padStart(2, '0')).join(' ') ?? '—'}</dd></div>
                <div><dt>{$t('gpuTest.policyLp')}</dt><dd>{policyLp != null ? policyLp : $t('gpuTest.policyNone')}</dd></div>
              </dl>
            {:else}
              <span class="hint">{$t('gpuTest.policyNone')}</span>
            {/if}

            <div class="policy-actions">
              {#if detail.summary.status === 'Completed' && detail.summary.bestLp != null}
                <button class="primary" disabled={busy || !canApply || recoveryRequired} onclick={applyBest} title={canApply ? '' : (errText(applyStatus?.reason ?? null) || '')}>
                  {$t(isPassed ? 'gpuTest.applyBest' : 'gpuTest.applyCandidate')}
                </button>
                {#if !canApply && applyStatus?.reason}<span class="hint apply-reason">{errText(applyStatus.reason)}</span>{/if}
                {#if policyLp != null && policyLp !== detail.summary.bestLp}
                  <span class="hint mismatch">{$t('gpuTest.policyMismatch', { values: { current: policyLp, best: detail.summary.bestLp } })}</span>
                {/if}
              {/if}
              <button disabled={busy || recoveryRequired} onclick={restorePrevious}>{$t('gpuTest.restore')}</button>
            </div>
          </div>
        {:else}
          <div class="detail-empty">
            <svg viewBox="0 0 24 24" width="32" height="32" aria-hidden="true"><path d="M19 3H5c-1.1 0-2 .9-2 2v14c0 1.1.9 2 2 2h14c1.1 0 2-.9 2-2V5c0-1.1-.9-2-2-2zM9 17H7v-7h2v7zm4 0h-2V7h2v10zm4 0h-2v-4h2v4z" fill="currentColor" opacity="0.25"/></svg>
            <p class="hint">{$t('gpuTest.noDetail')}</p>
          </div>
        {/if}
      </div>
    </div>
  {:else}
    <!-- 手動 GPU 中斷親和性 -->
    <section class="panel">
      <h2>{$t('gpuTest.manualAffinityTitle')}</h2>
      <div class="form-grid">
        <label class="field">
          <span class="field-label">{$t('gpuTest.manualAffinityLpSelect')}</span>
          <select bind:value={manualLp} disabled={supportedLps.length === 0}>
            <option value={null} disabled selected={manualLp == null}>—</option>
            {#each supportedLps as i (i)}
              <option value={i}>LP {i}</option>
            {/each}
          </select>
        </label>
        {#if selectedGpu}
          <div class="field">
            <span class="field-label">{$t('gpuTest.policyLp')}</span>
            {#if policyLoading}
              <span class="hint">{$t('gpuTest.policyLoading')}</span>
            {:else}
              <span class="mono">{policyLp != null ? `LP ${policyLp}` : $t('gpuTest.policyNone')}</span>
            {/if}
          </div>
        {/if}
      </div>
      <div class="action-row">
        <span class="hint">{$t('gpuTest.gpuSelect')}: {$gpuDevices.find((d) => d.instanceId === selectedGpu)?.friendlyName ?? (selectedGpu || $t('gpuTest.noGpu'))}</span>
        <div class="policy-actions">
          <button
            class="primary"
            disabled={busy || isRunning || recoveryRequired || manualLp == null || !selectedGpu}
            onclick={manualApplyClicked}
          >{$t('gpuTest.manualApply')}</button>
          <button
            disabled={busy || isRunning || recoveryRequired}
            onclick={restorePrevious}
          >{$t('gpuTest.restore')}</button>
        </div>
      </div>
    </section>
  {/if}

  <!-- ═══════════════════════════════════════════════════════════════════ -->
  <!-- 確認對話框                                                           -->
  <!-- ═══════════════════════════════════════════════════════════════════ -->
  <ConfirmDialog open={confirmAction === 'start'} title={$t('gpuTest.riskTitle') as string} message={$t('gpuTest.riskBody', { values: { gpu: $gpuDevices.find((d) => d.instanceId === selectedGpu)?.friendlyName ?? selectedGpu, count: restartCount } }) as string} detail={$t('gpuTest.riskEstimate', { values: { minutes: estMinutes } }) as string} confirmLabel={$t('gpuTest.riskConfirm') as string} cancelLabel={$t('gpuTest.riskCancel') as string} {busy} onconfirm={doStart} oncancel={() => (confirmAction = null)} />
  <ConfirmDialog open={confirmAction === 'applyBest'} title={$t('gpuTest.applyBestTitle') as string} message={$t('gpuTest.applyBestConfirm', { values: { lp: detail?.summary.bestLp ?? '', gpu: $gpuDevices.find((d) => d.instanceId === detail?.summary.gpuInstanceId)?.friendlyName ?? detail?.summary.gpuInstanceId ?? '' } }) as string} confirmLabel={$t('common.confirm') as string} cancelLabel={$t('common.cancel') as string} {busy} onconfirm={confirmApplyBest} oncancel={() => (confirmAction = null)} />
  <ConfirmDialog open={confirmAction === 'manualApply'} title={$t('gpuTest.manualApplyConfirmTitle') as string} message={$t('gpuTest.manualApplyConfirmBody', { values: { lp: manualLp ?? '', gpu: $gpuDevices.find((d) => d.instanceId === selectedGpu)?.friendlyName ?? selectedGpu } }) as string} confirmLabel={$t('common.confirm') as string} cancelLabel={$t('common.cancel') as string} {busy} onconfirm={confirmManualApply} oncancel={() => (confirmAction = null)} />
  <ConfirmDialog open={confirmAction === 'restore'} title={$t('gpuTest.restoreTitle') as string} message={$t('gpuTest.restoreConfirm') as string} confirmLabel={$t('gpuTest.restore') as string} cancelLabel={$t('common.cancel') as string} {busy} onconfirm={confirmRestorePrevious} oncancel={() => (confirmAction = null)} />
  <ConfirmDialog open={confirmAction === 'deleteHistory'} title={$t('gpuTest.deleteTitle') as string} message={$t('gpuTest.deleteConfirm') as string} confirmLabel={$t('gpuTest.delete') as string} cancelLabel={$t('common.cancel') as string} danger {busy} onconfirm={confirmDeleteHistory} oncancel={() => { confirmAction = null; deleteTargetId = null; }} />
</div>

<style>
  .gpu-test { display: flex; flex-direction: column; gap: var(--space-3); }

  /* ── 區段切換 ── */
  .segment-bar {
    display: flex;
    gap: 2px;
    background: var(--surface-2);
    border-radius: var(--radius-md);
    padding: 3px;
    width: fit-content;
  }
  .segment-btn {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    background: transparent;
    border: none;
    border-radius: var(--radius-sm);
    padding: var(--space-2) var(--space-4);
    height: 32px;
    font-size: 13px;
    font-weight: var(--font-weight-medium);
    color: var(--text-secondary);
    cursor: pointer;
    transition: all var(--transition-fast);
  }
  .segment-btn:hover:not(:disabled) { color: var(--text-primary); }
  .segment-btn.active { background: var(--surface-0); color: var(--text-primary); font-weight: var(--font-weight-medium); box-shadow: var(--shadow-xs); }
  .segment-btn:disabled { opacity: 0.4; cursor: default; }

  /* ── Recovery banner ── */
  .recovery-banner {
    display: flex; align-items: center; gap: var(--space-2);
    padding: var(--space-3); background: var(--danger-muted);
    border: 1px solid var(--danger); border-radius: var(--radius-md);
    color: var(--danger); font-weight: var(--font-weight-medium); font-size: 13px;
  }

  /* ── 面板 ── */
  .panel { background: var(--surface-1); border: 1px solid var(--border-subtle); border-radius: var(--radius-md); padding: var(--space-4); }
  .panel h2 { margin: 0 0 var(--space-3); font-size: 14px; }
  .running-panel { border-color: var(--accent); }

  /* ── 表單 ── */
  .form-grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(200px, 1fr)); gap: var(--space-3) var(--space-4); }
  .field { display: flex; flex-direction: column; gap: 4px; }
  .field.check { flex-direction: row; align-items: center; gap: var(--space-2); }
  .field.full-width { grid-column: 1 / -1; }
  .field-label { color: var(--text-secondary); font-size: 12px; font-weight: var(--font-weight-medium); }

  .lp-chips { display: flex; flex-wrap: wrap; gap: 5px; }
  .lp-chips button { min-width: 32px; padding: 3px 8px; text-align: center; font-size: 12px; }
  .lp-chips button.selected { background: var(--accent); border-color: var(--accent); color: var(--accent-text); }

  .vulkan-group { margin-top: var(--space-1); }
  .vulkan-opts { display: flex; flex-wrap: wrap; gap: var(--space-3); margin-top: var(--space-2); padding: var(--space-3); background: var(--surface-2); border-radius: var(--radius-sm); }

  .section-toggle {
    display: flex; align-items: center; gap: var(--space-2);
    background: none; border: none; color: var(--text-secondary);
    font: inherit; font-size: 12px; cursor: pointer; padding: 0;
  }
  .section-toggle svg { transition: transform var(--transition-fast); }
  .section-toggle svg.rotated { transform: rotate(90deg); }

  .action-row { display: flex; align-items: center; justify-content: space-between; margin-top: var(--space-4); gap: var(--space-3); }

  .error-msg { display: flex; align-items: center; gap: var(--space-2); color: var(--danger); font-size: 12px; margin-top: var(--space-2); }

  /* ── 執行中 ── */
  .active-warning { background: var(--surface-2); border: 1px solid var(--accent); border-radius: var(--radius-sm); padding: var(--space-3); display: flex; flex-direction: column; gap: 2px; margin-bottom: var(--space-3); }
  .active-warning strong { color: var(--accent); font-size: 15px; }
  .running-meta { display: grid; grid-template-columns: repeat(auto-fill, minmax(140px, 1fr)); gap: var(--space-2) var(--space-4); margin: 0 0 var(--space-3); }
  .running-meta dt { color: var(--text-secondary); font-size: 11px; }
  .running-meta dd { margin: 0; font-size: 13px; }

  /* ── Results workspace ── */
  .results-workspace { display: flex; gap: 0; border: 1px solid var(--border-subtle); border-radius: var(--radius-md); overflow: hidden; }

  .session-list { width: 240px; flex-shrink: 0; background: var(--surface-1); border-right: 1px solid var(--border-subtle); display: flex; flex-direction: column; }
  .session-list-head { padding: var(--space-2) var(--space-3); border-bottom: 1px solid var(--border-subtle); }
  .session-item {
    display: flex; flex-direction: column; gap: 2px; width: 100%; text-align: left;
    background: transparent; border: none; border-bottom: 1px solid var(--border-subtle);
    border-radius: 0; padding: var(--space-2) var(--space-3); height: auto; cursor: pointer;
  }
  .session-item:hover { background: var(--surface-2); }
  .session-item.active { background: var(--accent-muted); border-left: 3px solid var(--accent); padding-left: calc(var(--space-3) - 3px); }
  .session-item-top { display: flex; align-items: center; justify-content: space-between; gap: var(--space-1); }
  .session-date { font-size: 12px; font-weight: var(--font-weight-medium); }
  .session-item-meta { display: flex; gap: var(--space-2); font-size: 11px; }
  .session-item-foot { display: flex; justify-content: space-between; gap: var(--space-2); }
  .empty-hint { padding: var(--space-4); text-align: center; }

  /* Status badges */
  .badge { display: inline-flex; align-items: center; gap: 3px; font-size: 10px; font-weight: var(--font-weight-medium); padding: 0 5px; border-radius: var(--radius-xs); line-height: 16px; }
  .badge.status-completed { background: var(--success-muted); color: var(--success); }
  .badge.status-failed, .badge.status-cancelled { background: var(--danger-muted); color: var(--danger); }
  .badge.best { background: var(--accent); color: var(--accent-text); }
  .badge.warn { background: var(--warning-muted); color: var(--warning); }

  .session-detail { flex: 1; min-width: 0; background: var(--surface-0); padding: var(--space-4); }
  .detail-head { display: flex; align-items: center; justify-content: space-between; gap: var(--space-3); margin-bottom: var(--space-3); flex-wrap: wrap; }
  .detail-head h2 { margin: 0; font-size: 14px; flex: 1; }
  .detail-head-badges { display: flex; align-items: center; gap: var(--space-2); }
  .detail-empty { display: flex; flex-direction: column; align-items: center; justify-content: center; gap: var(--space-3); height: 100%; color: var(--text-secondary); }

  /* 指標表格 */
  .table-scroll { overflow-x: auto; margin-bottom: var(--space-3); }
  .metric-table { width: 100%; border-collapse: collapse; font-variant-numeric: tabular-nums; }
  .metric-table th, .metric-table td { padding: 5px 8px; border-bottom: 1px solid var(--border-subtle); text-align: right; white-space: nowrap; }
  .metric-table th:first-child, .metric-table td:first-child { text-align: left; }
  .metric-table th { color: var(--text-secondary); font-size: 11px; font-weight: var(--font-weight-medium); }
  td.best { background: var(--accent-muted); color: var(--accent); font-weight: var(--font-weight-medium); }
  td.second { background: color-mix(in srgb, var(--accent-muted) 50%, transparent); }
  td.unusual-value { color: var(--warning); }
  .lp-cell { font-weight: var(--font-weight-medium); }

  /* Meta */
  .meta-strip { display: flex; flex-wrap: wrap; gap: var(--space-2) var(--space-5); margin-bottom: var(--space-4); }
  .meta-item { display: flex; flex-direction: column; gap: 1px; }
  .mono { font-family: 'IBM Plex Sans TC', monospace; font-size: 11px; }

  /* Policy */
  .policy-section { background: var(--surface-1); border: 1px solid var(--border-subtle); border-radius: var(--radius-sm); padding: var(--space-3); }
  .policy-header { display: flex; align-items: baseline; gap: var(--space-3); margin-bottom: var(--space-2); }
  .policy-header h3 { margin: 0; font-size: 13px; font-weight: var(--font-weight-semibold); }
  .policy-list { display: flex; flex-direction: column; gap: 4px; margin: 0 0 var(--space-2); }
  .policy-list div { display: flex; gap: var(--space-3); }
  .policy-list dt { color: var(--text-secondary); min-width: 110px; font-size: 12px; }
  .policy-list dd { margin: 0; font-size: 12px; }
  .policy-actions { display: flex; flex-wrap: wrap; align-items: center; gap: var(--space-2); }
  .apply-reason, .mismatch { max-width: 360px; }
  .mismatch { color: var(--warning); }

  /* Reliability */
  .reliability-block { display: flex; flex-direction: column; gap: var(--space-2); border: 1px solid var(--border-subtle); border-radius: var(--radius-sm); padding: var(--space-3); margin-bottom: var(--space-4); }
  .reliability-block.rel-passed { border-color: var(--success); }
  .reliability-block.rel-equivalent { border-color: var(--accent); }
  .reliability-block.rel-inconclusive { border-color: var(--warning); }
  .rel-head { display: flex; align-items: center; gap: var(--space-2); flex-wrap: wrap; }
  .rel-badge { background: var(--surface-2); color: var(--text-primary); }
  .rel-passed .rel-badge { background: var(--success-muted); color: var(--success); }
  .rel-equivalent .rel-badge { background: var(--accent-muted); color: var(--accent); }
  .rel-inconclusive .rel-badge { background: var(--warning-muted); color: var(--warning); }
  .rel-rounds { display: flex; flex-wrap: wrap; gap: var(--space-1) var(--space-4); font-size: 12px; }
  .rel-round { font-variant-numeric: tabular-nums; }
  .rel-meta { font-size: 12px; color: var(--text-secondary); }
  .rel-evidence { display: flex; flex-wrap: wrap; gap: var(--space-1) var(--space-4); font-size: 12px; font-variant-numeric: tabular-nums; }
  .rel-note { font-size: 11px; }
  .rel-pcts { display: flex; flex-wrap: wrap; gap: var(--space-1) var(--space-4); font-size: 12px; font-variant-numeric: tabular-nums; }

  /* Progress */
  .progress-track { height: 6px; background: var(--surface-2); border-radius: var(--radius-full); overflow: hidden; margin-bottom: var(--space-2); }
  .progress-track > div { height: 100%; background: var(--accent); transition: width 0.3s; border-radius: var(--radius-full); }

  @media (max-width: 999px) {
    .results-workspace { flex-direction: column; }
    .session-list { width: 100%; border-right: none; border-bottom: 1px solid var(--border-subtle); }
  }
</style>
