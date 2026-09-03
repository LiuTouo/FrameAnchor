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
    FpsCapPolicy,
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
  // 取消專用階段/百分比（後端 state 承載；取消中才有意義）
  const cancelStage = $derived($benchmarkState?.cancelStage ?? null);
  const cancelProgressPct = $derived($benchmarkState?.cancelProgress ?? 0);
  // compact progress 視窗模式（後端 windowLayout=CompactProgress）
  const compact = $derived($benchmarkState?.windowLayout === 'CompactProgress');
  const winIntegrity = $derived($benchmarkState?.windowIntegrity ?? null);

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
  let vulkanOptionsOpen = $state(true);
  let width = $state(1280);
  let height = $state(720);
  let manualLp = $state<number | null>(null);
  let fpsCap = $state(0);
  let fpsCapPolicy = $state<FpsCapPolicy>('Adaptive');
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
  // 基準測試候選 LP：P-core primary（非 SMT sibling），排除實體 Core 0，
  // 與後端 effective_lps 空清單預設一致；供「全部」按鈕與初始選擇使用。
  // 手動 GPU 親和性下拉選單仍使用上方完整的 supportedLps（含 Core 0）。
  const benchmarkLps = $derived.by(() => {
    if (!$topology) return [];
    const limit = Math.min($topology.totalLp, 64);
    const primaries = (pOnly: boolean) =>
      $topology.logicalProcessors
        .filter(
          (p) =>
            p.index < limit &&
            p.coreId !== 0 &&
            !p.isSmtSibling &&
            (pOnly ? ($topology.physicalCores[p.coreId]?.isPCore ?? false) : true),
        )
        .map((p) => p.index)
        .sort((a, b) => a - b);
    const p = primaries(true);
    return p.length > 0 ? p : primaries(false);
  });
  const lpList = $derived(lps);
  // 自適應排程：primary 短篩（10s）→ Top5 racing（20s）→ Top3 正式 capture
  // → Top2 前向/反向確認。restartCount 使用最大 capture 預算。
  const racingCount = $derived(Math.min(lpList.length, 5));
  const refinementCount = $derived(Math.min(lpList.length, 3));
  const restartCount = $derived(lpList.length + racingCount + refinementCount + 20);
  const estMinutes = $derived(
    Math.max(1, Math.round((
      lpList.length * (Math.min(sampleSecs, 10) + Math.min(warmUpSecs, 3) + 19)
      + racingCount * (Math.min(sampleSecs, 20) + Math.min(warmUpSecs, 3) + 19)
      + (refinementCount + 20) * (sampleSecs + warmUpSecs + 19)
    ) / 60)),
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
      // 預設候選與後端 effective_lps 一致：P-core primary（非 SMT sibling），排除實體 Core 0
      lps = [...benchmarkLps];
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

  onMount(() => {
    void init();
    // Registry 可能被外部工具改寫；背景回讀避免面板長時間顯示舊策略。
    const policyTimer = window.setInterval(() => {
      if (policyGpu && !isRunning && !busy) void refreshPolicyFor(policyGpu, false);
    }, 5000);
    return () => window.clearInterval(policyTimer);
  });

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

  async function refreshPolicyFor(instanceId: string, showLoading = true) {
    if (!instanceId) return;
    if (showLoading) policyLoading = true;
    try { gpuPolicy.set(await ipc.getGpuAffinityPolicy(instanceId)); }
    catch { gpuPolicy.set(null); }
    finally { if (showLoading) policyLoading = false; }
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
      await ipc.startGpuBenchmark({ candidateLps: lps, gpuInstanceId: selectedGpu, workload, warmUpSecs, sampleSecs, repetitions: 5, syncWorkloadAffinity: false, fullscreen: false, width, height, fpsCap, fpsCapPolicy, tripleBuffer, vulkanArgs: buildVulkanArgs(), workloadExePath: null, presentmonPath: null, gamePath: null, windowTitle: null });
      errMsg = null;
    } catch (e) { errMsg = String(e); }
    finally { busy = false; }
  }

  async function doCancel() {
    if (cancelPending) return; // 已請求取消，避免重複送出
    busy = true;
    try {
      await ipc.cancelBenchmark();
      cancelSent = true;
      // 立即讀回後端 state（requested/0%），不等 runner 下一個 capture boundary event
      benchmarkState.set(await ipc.getBenchmarkState());
    } catch (e) { errMsg = String(e); }
    finally { busy = false; }
  }

  function buildVulkanArgs(): string[] {
    // 強制視窗模式（fullscreen 已移除）
    return [`--fullscreen=0`, `--width=${width}`, `--height=${height}`, `--fps_cap=${fpsCap}`, `--triple_buffering=${tripleBuffer ? 1 : 0}`];
  }

  // ── GPU 策略 ──
  async function applyBest() { if (applyTargetLp != null) confirmAction = 'applyBest'; }
  async function confirmApplyBest() {
    if (applyTargetLp == null) return;
    confirmAction = null; busy = true;
    try { await ipc.applyBestGpuAffinity(detail!.summary.id); errMsg = null; await refreshPolicyFor(detail!.summary.gpuInstanceId || selectedGpu); }
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

  function cancelStageLabel(stage: string | null | undefined): string {
    switch (stage) {
      case 'requested': return $t('gpuTest.cancelRequested') as string;
      case 'stopping': return $t('gpuTest.cancelStopping') as string;
      case 'restoring': return $t('gpuTest.cancelRestoring') as string;
      case 'finalizing': return $t('gpuTest.cancelFinalizing') as string;
      default: return $t('gpuTest.cancelling') as string;
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
  const currentRound = $derived(progress?.phaseRound ?? null);
  const etaMin = $derived(progress?.etaSecs ? Math.max(1, Math.round(progress.etaSecs / 60)) : null);
  const canApply = $derived(applyStatus?.canApply ?? false);
  // 視窗完整性顯示（compact 模式）
  const integrityLabel = $derived.by(() => {
    const wi = winIntegrity;
    if (!wi) return '';
    if (wi.error) return $t('gpuTest.windowIntegrityError') as string;
    if (wi.retries > 0) return $t('gpuTest.windowIntegrityRetry', { values: { n: wi.retries } }) as string;
    return $t('gpuTest.windowIntegrityOk') as string;
  });

  // 新 schema 證據欄位（舊 session 缺欄 → undefined/null）。
  const verifiedBest = $derived(detail?.summary.verifiedBestLp ?? null);
  const screeningCandidate = $derived(detail?.summary.screeningCandidateLp ?? null);
  const screeningRunnerUp = $derived(detail?.summary.screeningRunnerUpLp ?? null);
  const confirmationWinner = $derived(detail?.summary.confirmationWinnerLp ?? null);
  const captureQuality = $derived(detail?.summary.captureQuality ?? null);
  const envStability = $derived(detail?.summary.environmentStability ?? null);
  // 套用目標：新 schema 用 verifiedBestLp；legacy（無 verifiedBestLp）沿用 bestLp（後端把關）。
  const applyTargetLp = $derived(verifiedBest ?? detail?.summary.bestLp ?? null);
  // 前端額外閘：新 schema 只在 verifiedBestLp 存在且 Passed 時啟用；legacy 交由後端。
  const isVerifiedApplyable = $derived(
    detail?.summary.verifiedBestLp != null
      ? detail.summary.reliability?.status === 'Passed'
      : true,
  );
  // 反向驗證結果標籤（reverseRan=false → 未執行；passed → 通過；其餘 → 未定）。
  const reverseResultLabel = $derived.by(() => {
    const rel = reliability;
    if (!rel?.reverseRan) return $t('gpuTest.reverseNotRun') as string;
    if (rel.reverseVerdict === 'passed') return $t('gpuTest.reversePassed') as string;
    return $t('gpuTest.reverseInconclusive') as string;
  });

  // 進行中 phase 由後端明確提供；stage 僅描述單次 capture 的低階步驟。
  const phaseLabel = $derived.by(() => {
    const p = $benchmarkProgress;
    if (!p) return '';
    const stage = p.stage ?? '';
    if (stage === 'calibrating') return $t('gpuTest.phaseCalibrating') as string;
    switch (p.phase) {
      case 'Screening': return $t('gpuTest.phaseScreening') as string;
      case 'Refinement': return $t('gpuTest.phaseRefinement') as string;
      case 'Confirmation': return $t('gpuTest.phaseConfirming') as string;
      case 'ReverseConfirmation': return $t('gpuTest.phaseReverseConfirming') as string;
      case 'EquivalentValidation': return $t('gpuTest.phaseEquivalentValidation') as string;
    }
    switch (stage) {
      case 'starting': return $t('gpuTest.phaseStarting') as string;
      case 'applying': return $t('gpuTest.phaseApplying') as string;
      case 'launching': return $t('gpuTest.phaseLaunching') as string;
      case 'collecting': return $t('gpuTest.phaseCollecting') as string;
      case 'collected': return $t('gpuTest.phaseCollected') as string;
      case 'finalizing': return $t('gpuTest.phaseFinalizing') as string;
    }
    return stage || ($t('gpuTest.phaseUnknown') as string);
  });

  // ── Equivalent-mode（algorithmVersion=2 且 Reliability=Equivalent）──
  const isEquivalentMode = $derived(
    !!detail &&
      detail.summary.reliability?.status === 'Equivalent' &&
      (detail.summary.reliability.algorithmVersion ?? 0) === 2,
  );
  const equivalentFinalists = $derived(detail?.summary.equivalentFinalistLps ?? []);
  const equivalentValidation = $derived(detail?.equivalentSafetyValidation ?? null);
  const equivalentEvidence = $derived(reliability);
  let selectedEquivalentLp = $state<number | null>(null);
  const equivalentCurrentInPair = $derived(
    policyLp != null && equivalentFinalists.length === 2 && equivalentFinalists.includes(policyLp),
  );
  const validationStatus = $derived(equivalentValidation?.status ?? 'None');
  // ImmediatePass（目前核心已在等效組、rounds=0）→ 顯示「無額外 capture」，非一般測試通過。
  const equivalentAlreadyInPair = $derived(
    validationStatus === 'Passed' && equivalentValidation?.reason === 'already_in_equivalent_pair',
  );
  // 可套用需：驗證 Passed + selected 一致 + 後端判定可套用（含 live reference snapshot 未變）。
  const equivalentApplyable = $derived(
    validationStatus === 'Passed' &&
      equivalentValidation?.selectedLp != null &&
      equivalentValidation.selectedLp === selectedEquivalentLp &&
      applyStatus?.canApply === true,
  );
  // 套用後（或切換 session）自動選回第一個 finalist。
  $effect(() => {
    if (isEquivalentMode && equivalentFinalists.length === 2) {
      if (selectedEquivalentLp == null || !equivalentFinalists.includes(selectedEquivalentLp)) {
        selectedEquivalentLp = equivalentFinalists[0];
      }
    }
  });
  // 驗證 Pending 期間輪詢 session 直到 Passed/Failed/Cancelled。
  $effect(() => {
    if (validationStatus !== 'Pending' || !detail) return;
    const id = detail.summary.id;
    const timer = setInterval(() => {
      void pollValidation(id);
    }, 2000);
    return () => clearInterval(timer);
  });

  async function pollValidation(id: string) {
    try {
      detail = await ipc.getBenchmarkSession(id);
      applyStatus = await ipc.getBenchmarkApplyStatus(id);
    } catch {
      /* 忽略輪詢錯誤，下次再試 */
    }
  }

  async function validateEquivalent() {
    if (selectedEquivalentLp == null || !detail) return;
    busy = true;
    try {
      await ipc.validateEquivalentCandidate(detail.summary.id, selectedEquivalentLp);
      errMsg = null;
      await pollValidation(detail.summary.id);
    } catch (e) {
      errMsg = String(e);
    } finally {
      busy = false;
    }
  }

  async function applyEquivalent() {
    if (selectedEquivalentLp == null || !detail) return;
    busy = true;
    try {
      await ipc.applyEquivalentGpuAffinity(detail.summary.id, selectedEquivalentLp);
      errMsg = null;
      await loadDetail(detail.summary.id); // 套用後重新載入狀態
    } catch (e) {
      errMsg = String(e);
    } finally {
      busy = false;
    }
  }

  async function cancelEquivalentValidation() {
    if (!detail) return;
    busy = true;
    try {
      await ipc.cancelBenchmark();
      errMsg = null;
      await pollValidation(detail.summary.id); // 取消後狀態轉 Cancelled/Failed
    } catch (e) {
      errMsg = String(e);
    } finally {
      busy = false;
    }
  }

  function equivalentStatusLabel(s: string): string {
    switch (s) {
      case 'Passed': return $t('gpuTest.equivalentValidationPassed') as string;
      case 'Failed': return $t('gpuTest.equivalentValidationFailed') as string;
      case 'Pending': return $t('gpuTest.equivalentValidationRunning') as string;
      case 'Cancelled': return $t('gpuTest.equivalentValidationCancelled') as string;
      default: return $t('gpuTest.equivalentValidationNotRun') as string;
    }
  }
</script>

<!-- ═══════════════════════════════════════════════════════════════════════ -->
<!-- 區段切換控制                                                           -->
<!-- ═══════════════════════════════════════════════════════════════════════ -->
<div class="gpu-test" class:compact>
  {#if !compact}
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
  {/if}

  <!-- ═══════════════════════════════════════════════════════════════════ -->
  <!-- 測試區段                                                             -->
  <!-- ═══════════════════════════════════════════════════════════════════ -->
  {#if segment === 'test'}
    {#if isRunning}
      <!-- 執行中：精簡進度 UI -->
      <section class="panel running-panel" class:compact aria-live="polite">
        <h2>{cancelPending ? $t('gpuTest.cancelling') : $t('gpuTest.runningTitle')}</h2>
        {#if !compact}
        <div class="active-warning" role="alert">
          <strong>{$t('gpuTest.activeWarningZh')}</strong>
          <span class="hint">{$t('gpuTest.activeWarningEn')}</span>
        </div>
        {/if}
        {#if cancelPending}
          <!-- 取消中：切換為取消專用進度與階段，不顯示原 benchmark 進度冒充取消進度 -->
          <dl class="running-meta">
            <div><dt>{$t('gpuTest.cancelProgress')}</dt><dd>{cancelProgressPct}%</dd></div>
            <div><dt>{$t('gpuTest.colStatus')}</dt><dd>{cancelStageLabel(cancelStage)}</dd></div>
          </dl>
          <div class="progress-track" role="progressbar" aria-valuemin="0" aria-valuemax="100" aria-valuenow={cancelProgressPct} aria-label={$t('gpuTest.cancelling')}>
            <div style="width: {cancelProgressPct}%"></div>
          </div>
        {:else}
          <dl class="running-meta">
            {#if !compact}<div><dt>{$t('gpuTest.gpuSelect')}</dt><dd>{$gpuDevices.find((d) => d.instanceId === selectedGpu)?.friendlyName ?? selectedGpu}</dd></div>{/if}
            {#if currentRound != null}<div><dt>{$t('gpuTest.repetitions')}</dt><dd>{$t('gpuTest.round', { values: { round: currentRound } })}</dd></div>{/if}
            <div><dt>{$t('gpuTest.currentLpLabel')}</dt><dd>{$benchmarkState?.currentLp ?? '—'}</dd></div>
            <div><dt>{$t('gpuTest.progressLabel')}</dt><dd>{$benchmarkState?.progressPct ?? 0}%</dd></div>
            {#if etaMin != null}<div><dt>{$t('gpuTest.etaLabel')}</dt><dd>{$t('gpuTest.riskEstimate', { values: { minutes: etaMin } })}</dd></div>{/if}
            <div><dt>{$t('gpuTest.colStatus')}</dt><dd>{phaseLabel || stageLabel($benchmarkState?.stage)}</dd></div>
            <div><dt>{$t('gpuTest.windowIntegrity')}</dt><dd>{integrityLabel || '—'}</dd></div>
          </dl>
          <div class="progress-track" role="progressbar" aria-valuemin="0" aria-valuemax="100" aria-valuenow={$benchmarkState?.progressPct ?? 0} aria-label={$t('gpuTest.runningTitle')}>
            <div style="width: {($benchmarkState?.progressPct ?? 0)}%"></div>
          </div>
        {/if}
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
                  <label class="field"><span class="field-label">{$t('gpuTest.width')}</span><input type="number" bind:value={width} min="1" /></label>
                  <label class="field"><span class="field-label">{$t('gpuTest.height')}</span><input type="number" bind:value={height} min="1" /></label>
                  <label class="field">
                    <span class="field-label">{$t('gpuTest.fpsCapPolicyLabel')}</span>
                    <select bind:value={fpsCapPolicy}>
                      <option value="Adaptive">{$t('gpuTest.fpsCapAdaptive')}</option>
                      <option value="Fixed">{$t('gpuTest.fpsCapFixed')}</option>
                    </select>
                  </label>
                  <label class="field">
                    <span class="field-label">{$t('gpuTest.fpsCap')}</span>
                    <input type="number" bind:value={fpsCap} min="0" disabled={fpsCapPolicy === 'Adaptive'} />
                  </label>
                  {#if fpsCapPolicy === 'Adaptive'}<span class="hint full-width">{$t('gpuTest.fpsCapAdaptiveHint')}</span>{/if}
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
                      <td class="lp-cell">{r.lp}
                        {#if isEquivalentMode}
                          {#if equivalentFinalists.includes(r.lp)}<span class="badge">{$t('gpuTest.equivalentTag')}</span>{/if}
                        {:else}
                          {#if r.lp === applyTargetLp}<span class="badge best">{$t(verifiedBest != null ? 'gpuTest.verifiedTag' : (isPassed ? 'gpuTest.bestTag' : 'gpuTest.candidateTag'))}</span>{/if}
                          {#if screeningCandidate != null && r.lp === screeningCandidate && r.lp !== applyTargetLp}<span class="badge">{$t('gpuTest.candidateTag')}</span>{/if}
                        {/if}
                      </td>
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
            {#if applyTargetLp != null}<div class="meta-item"><span class="hint">{$t(verifiedBest != null ? 'gpuTest.verifiedBest' : (isPassed ? 'gpuTest.colBest' : 'gpuTest.colCandidate'))}</span><span>{applyTargetLp}</span></div>{/if}
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
                <div class="rel-meta">{$t('gpuTest.reliabilityConfirmation', { values: { screening: reliability.screeningRounds, confirmation: reliability.confirmationRounds } })}</div>
                {#if reliability.compositeAdvantagePct != null || reliability.ciLowerPct != null || reliability.avgFpsAdvantagePct != null || reliability.p1LowAdvantagePct != null || reliability.spikeRateDeltaPp != null}
                  <div class="rel-evidence">
                    <span class="hint">{$t('gpuTest.reliabilityEvidence')}</span>
                    {#if reliability.compositeAdvantagePct != null}<span>{$t('gpuTest.evidenceComposite')} {fmtPct(reliability.compositeAdvantagePct)}</span>{/if}
                    {#if reliability.ciLowerPct != null && reliability.ciUpperPct != null}<span>{$t('gpuTest.reliabilityCi', { values: { lower: fmtPct(reliability.ciLowerPct), upper: fmtPct(reliability.ciUpperPct) } })}</span>{/if}
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

          <!-- Equivalent-mode：實質等效契約（algorithmVersion=2 且 Equivalent） -->
          {#if isEquivalentMode}
            <div class="equivalent-block">
              <div class="equivalent-notice" role="status">
                <svg viewBox="0 0 24 24" width="16" height="16" aria-hidden="true"><path d="M12 2C6.48 2 2 6.48 2 12s4.48 10 10 10 10-4.48 10-10S17.52 2 12 2zm1 15h-2v-2h2v2zm0-4h-2V7h2v6z" fill="currentColor"/></svg>
                <span>{$t('gpuTest.equivalentNoVerifiedBest')}</span>
              </div>

              <h3>{$t('gpuTest.equivalentFinalists')}</h3>
              <div class="equivalent-select" role="radiogroup" aria-label={$t('gpuTest.equivalentFinalists')}>
                {#each equivalentFinalists as lp (lp)}
                  <button
                    class:selected={selectedEquivalentLp === lp}
                    class:current={policyLp === lp}
                    role="radio"
                    aria-checked={selectedEquivalentLp === lp}
                    onclick={() => (selectedEquivalentLp = lp)}
                  >
                    LP {lp}{policyLp === lp ? ` · ${$t('gpuTest.equivalentCurrentCore')}` : ''}
                  </button>
                {/each}
              </div>

              <h3>{$t('gpuTest.equivalentEvidence')}</h3>
              <dl class="equivalent-evidence">
                <div><dt>{$t('gpuTest.colAvg')}</dt><dd>{fmtPct(equivalentEvidence?.equivalentAvgImprovementPct)}</dd></div>
                <div><dt>{$t('gpuTest.colP1')}</dt><dd>{fmtPct(equivalentEvidence?.equivalentP1ImprovementPct)}</dd></div>
                <div><dt>{$t('gpuTest.colP01')}</dt><dd>{fmtPct(equivalentEvidence?.equivalentP01ImprovementPct)}</dd></div>
                <div><dt>{$t('gpuTest.evidenceMad')}</dt><dd>{fmtDeltaPp(equivalentEvidence?.equivalentMadDeltaPp)}</dd></div>
                <div><dt>{$t('gpuTest.evidenceSpike')}</dt><dd>{fmtDeltaPp(equivalentEvidence?.equivalentSpikeDeltaPp)}</dd></div>
              </dl>
              <span class="hint">{$t('gpuTest.equivalentThresholds')}</span>

              {#if equivalentCurrentInPair}
                <div class="hint equivalent-hint">{$t('gpuTest.equivalentCurrentInPairHint')}</div>
              {:else}
                <div class="hint equivalent-hint">{$t('gpuTest.equivalentCaptureHint')}</div>
              {/if}

              {#if validationStatus !== 'None'}
                <div class="equivalent-validation" class:passed={validationStatus === 'Passed'} class:failed={validationStatus === 'Failed' || validationStatus === 'Cancelled'}>
                  <span class="badge">{equivalentStatusLabel(validationStatus)}</span>
                  {#if equivalentValidation?.reason && (validationStatus === 'Failed' || validationStatus === 'Cancelled')}
                    <span class="hint">{errText(equivalentValidation.reason)}</span>
                  {/if}
                  {#if equivalentAlreadyInPair}
                    <span class="hint">{$t('gpuTest.equivalentAlreadyInPair')}</span>
                  {:else if validationStatus === 'Passed' && equivalentValidation}
                    <dl class="equivalent-evidence">
                      <div><dt>{$t('gpuTest.colAvg')}</dt><dd>{fmtPct(equivalentValidation.avgImprovementPct)}</dd></div>
                      <div><dt>{$t('gpuTest.colP1')}</dt><dd>{fmtPct(equivalentValidation.p1ImprovementPct)}</dd></div>
                      <div><dt>{$t('gpuTest.colP01')}</dt><dd>{fmtPct(equivalentValidation.p01ImprovementPct)}</dd></div>
                      <div><dt>{$t('gpuTest.evidenceMad')}</dt><dd>{fmtDeltaPp(equivalentValidation.madDeltaPp)}</dd></div>
                      <div><dt>{$t('gpuTest.evidenceSpike')}</dt><dd>{fmtDeltaPp(equivalentValidation.spikeDeltaPp)}</dd></div>
                    </dl>
                  {/if}
                </div>
              {/if}

              <div class="policy-actions">
                <button
                  class="primary"
                  disabled={busy || recoveryRequired || validationStatus === 'Pending' || selectedEquivalentLp == null}
                  onclick={validateEquivalent}
                >
                  {validationStatus === 'Pending' ? $t('gpuTest.equivalentValidationRunning') : $t('gpuTest.equivalentValidate')}
                </button>
                {#if validationStatus === 'Pending'}
                  <button class="danger" disabled={busy} onclick={cancelEquivalentValidation}>{$t('gpuTest.cancel')}</button>
                {/if}
                {#if equivalentApplyable}
                  <button class="primary" disabled={busy || recoveryRequired} onclick={applyEquivalent}>{$t('gpuTest.equivalentApply')}</button>
                {/if}
              </div>
            </div>
          {/if}

          <!-- 新 schema 證據：篩選候選/亞軍、前向勝者、反向驗證、已驗證核心 -->
          {#if detail.summary.status === 'Completed' && !isEquivalentMode}
            <div class="evidence-block">
              <h3>{$t('gpuTest.evidenceTitle')}</h3>
              <dl class="evidence-grid">
                <div class="evidence-item"><dt>{$t('gpuTest.screeningCandidate')}</dt><dd>{screeningCandidate != null ? `LP ${screeningCandidate}` : '—'}</dd></div>
                <div class="evidence-item"><dt>{$t('gpuTest.screeningRunnerUp')}</dt><dd>{screeningRunnerUp != null ? `LP ${screeningRunnerUp}` : '—'}</dd></div>
                <div class="evidence-item"><dt>{$t('gpuTest.confirmationWinner')}</dt><dd>{confirmationWinner != null ? `LP ${confirmationWinner}` : '—'}</dd></div>
                <div class="evidence-item"><dt>{$t('gpuTest.reverseResult')}</dt><dd>{reverseResultLabel}</dd></div>
                <div class="evidence-item verified"><dt>{$t('gpuTest.verifiedBest')}</dt><dd>{verifiedBest != null ? `LP ${verifiedBest}` : '—'}</dd></div>
              </dl>
            </div>
          {/if}

          {#if captureQuality}
            <div class="evidence-block">
              <h3>{$t('gpuTest.captureQualityTitle')}</h3>
              <dl class="evidence-grid">
                <div><dt>{$t('gpuTest.captureTotal')}</dt><dd>{captureQuality.totalCaptures}</dd></div>
                <div><dt>{$t('gpuTest.captureValid')}</dt><dd>{captureQuality.validCaptures}</dd></div>
                <div><dt>{$t('gpuTest.captureInvalid')}</dt><dd>{captureQuality.invalidCaptures}</dd></div>
                <div><dt>{$t('gpuTest.captureWindowInvalid')}</dt><dd>{captureQuality.windowInvalidCaptures ?? 0}</dd></div>
                <div><dt>{$t('gpuTest.captureWindowRetry')}</dt><dd>{captureQuality.windowRetryCaptures ?? 0}</dd></div>
                <div><dt>{$t('gpuTest.captureOverflowed')}</dt><dd>{captureQuality.overflowedPresentEvents}</dd></div>
                <div><dt>{$t('gpuTest.captureEtwLost')}</dt><dd>{captureQuality.etwEventsLost}</dd></div>
                <div><dt>{$t('gpuTest.captureEffectiveFpsCap')}</dt><dd>{captureQuality.effectiveFpsCap > 0 ? captureQuality.effectiveFpsCap : $t('gpuTest.captureUnlimited')}</dd></div>
                <div><dt>{$t('gpuTest.captureBufferSize')}</dt><dd>{captureQuality.circularBufferSize}</dd></div>
                <div><dt>{$t('gpuTest.captureIntegrityPassed')}</dt><dd>{captureQuality.integrityPassed ? $t('gpuTest.boolYes') : $t('gpuTest.boolNo')}</dd></div>
              </dl>
            </div>
          {/if}

          {#if envStability}
            <div class="evidence-block">
              <h3>{$t('gpuTest.envStabilityTitle')}</h3>
              <dl class="evidence-grid">
                <div><dt>{$t('gpuTest.envStabilityPassed')}</dt><dd>{envStability.passed ? $t('gpuTest.envStable') : $t('gpuTest.envUnstable')}</dd></div>
                <div><dt>{$t('gpuTest.envDriftReruns')}</dt><dd>{envStability.driftReruns}</dd></div>
                {#if !envStability.passed && envStability.error}<div><dt>{$t('gpuTest.envError')}</dt><dd>{errText(envStability.error)}</dd></div>{/if}
              </dl>
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
              <p class="hint">{$t('gpuTest.policyReadbackHint')}</p>
            {:else}
              <span class="hint">{$t('gpuTest.policyNone')}</span>
            {/if}

            <div class="policy-actions">
              {#if detail.summary.status === 'Completed' && applyTargetLp != null}
                <button class="primary" disabled={busy || !canApply || !isVerifiedApplyable || recoveryRequired} onclick={applyBest} title={!canApply ? (errText(applyStatus?.reason ?? null) || '') : ''}>
                  {$t(verifiedBest != null ? 'gpuTest.applyVerified' : (isPassed ? 'gpuTest.applyBest' : 'gpuTest.applyCandidate'))}
                </button>
                {#if !canApply && applyStatus?.reason}<span class="hint apply-reason">{errText(applyStatus.reason)}</span>{/if}
                {#if policyLp != null && policyLp !== applyTargetLp}
                  <span class="hint mismatch">{$t('gpuTest.policyMismatch', { values: { current: policyLp, best: applyTargetLp } })}</span>
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
  .gpu-test { display: flex; flex-direction: column; gap: var(--space-4); }

  /* ── 區段切換 ── */
  .segment-bar {
    display: flex;
    gap: 2px;
    background: var(--surface-2);
    border: 1px solid var(--border-subtle);
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
    padding: 0 var(--space-4);
    height: 34px;
    font-size: 13px;
    font-weight: var(--font-weight-medium);
    color: var(--text-secondary);
    cursor: pointer;
    transition: background var(--transition-fast), color var(--transition-fast);
  }
  .segment-btn:hover:not(:disabled) { color: var(--text-primary); }
  .segment-btn.active { background: var(--surface-1); color: var(--text-primary); font-weight: var(--font-weight-semibold); box-shadow: var(--shadow-xs); }
  .segment-btn:disabled { opacity: 0.4; cursor: default; }

  /* ── Recovery banner ── */
  .recovery-banner {
    display: flex; align-items: center; gap: var(--space-2);
    padding: var(--space-3) var(--space-4); background: var(--danger-muted);
    border: 1px solid var(--danger); border-radius: var(--radius-md);
    color: var(--danger); font-weight: var(--font-weight-medium); font-size: 13px;
  }

  /* ── 面板 ── */
  .panel { background: var(--surface-1); border: 1px solid var(--border-subtle); border-radius: var(--radius-lg); padding: var(--space-5); }
  .panel h2 { margin: 0 0 var(--space-4); font-size: 15px; font-weight: var(--font-weight-semibold); }
  .running-panel { border-color: var(--accent); }

  /* ── 表單 ── */
  .form-grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(210px, 1fr)); gap: var(--space-4); }
  .field { display: flex; flex-direction: column; gap: 6px; }
  .field.check { flex-direction: row; align-items: center; gap: var(--space-2); }
  .field.full-width { grid-column: 1 / -1; }
  .field-label { color: var(--text-secondary); font-size: 12px; font-weight: var(--font-weight-medium); }

  .lp-chips { display: flex; flex-wrap: wrap; gap: 6px; }
  .lp-chips button { min-width: 36px; height: var(--control-sm); padding: 0 10px; text-align: center; font-size: 12px; }
  .lp-chips button.selected { background: var(--accent); border-color: var(--accent); color: var(--accent-text); }

  .vulkan-group { margin-top: var(--space-1); }
  .vulkan-opts { display: flex; flex-wrap: wrap; gap: var(--space-3) var(--space-4); margin-top: var(--space-2); padding: var(--space-4); background: var(--surface-2); border-radius: var(--radius-md); }

  .section-toggle {
    display: flex; align-items: center; gap: var(--space-2);
    background: none; border: none; color: var(--text-secondary);
    font: inherit; font-size: 12px; font-weight: var(--font-weight-medium); cursor: pointer; padding: 0;
  }
  .section-toggle:hover { color: var(--text-primary); }
  .section-toggle svg { transition: transform var(--transition-fast); }
  .section-toggle svg.rotated { transform: rotate(90deg); }

  .action-row { display: flex; align-items: center; justify-content: space-between; margin-top: var(--space-4); gap: var(--space-3); flex-wrap: wrap; }

  .error-msg { display: flex; align-items: center; gap: var(--space-2); color: var(--danger); font-size: 12px; margin-top: var(--space-3); }

  /* ── 執行中 ── */
  .active-warning { background: var(--surface-2); border: 1px solid var(--accent); border-radius: var(--radius-md); padding: var(--space-4); display: flex; flex-direction: column; gap: 2px; margin-bottom: var(--space-4); }
  .active-warning strong { color: var(--accent); font-size: 15px; }
  .running-meta { display: grid; grid-template-columns: repeat(auto-fill, minmax(150px, 1fr)); gap: var(--space-3) var(--space-4); margin: 0 0 var(--space-4); }
  .running-meta dt { color: var(--text-secondary); font-size: 11px; }
  .running-meta dd { margin: 0; font-size: 13.5px; font-weight: var(--font-weight-medium); }

  /* ── Results workspace ── */
  .results-workspace { display: flex; gap: 0; border: 1px solid var(--border-subtle); border-radius: var(--radius-lg); overflow: hidden; background: var(--surface-0); }

  .session-list { width: 252px; flex-shrink: 0; background: var(--surface-1); border-right: 1px solid var(--border-subtle); display: flex; flex-direction: column; }
  .session-list-head { padding: var(--space-3); border-bottom: 1px solid var(--border-subtle); }
  .session-item {
    display: flex; flex-direction: column; gap: 3px; width: 100%; text-align: left;
    background: transparent; border: none; border-bottom: 1px solid var(--border-subtle);
    border-radius: 0; padding: var(--space-3); height: auto; cursor: pointer;
  }
  .session-item:hover { background: var(--surface-2); }
  .session-item.active { background: var(--accent-muted); border-left: 3px solid var(--accent); padding-left: calc(var(--space-3) - 3px); }
  .session-item-top { display: flex; align-items: center; justify-content: space-between; gap: var(--space-1); }
  .session-date { font-size: 12px; font-weight: var(--font-weight-medium); }
  .session-item-meta { display: flex; gap: var(--space-2); font-size: 11px; flex-wrap: wrap; }
  .session-item-foot { display: flex; justify-content: space-between; gap: var(--space-2); }
  .empty-hint { padding: var(--space-5); text-align: center; }

  /* Status badges */
  .badge { display: inline-flex; align-items: center; gap: 4px; font-size: 11px; font-weight: var(--font-weight-medium); padding: 1px 8px; border-radius: var(--radius-full); line-height: 18px; }
  .badge.status-completed { background: var(--success-muted); color: var(--success); }
  .badge.status-failed, .badge.status-cancelled { background: var(--danger-muted); color: var(--danger); }
  .badge.best { background: var(--accent); color: var(--accent-text); }
  .badge.warn { background: var(--warning-muted); color: var(--warning); }

  .session-detail { flex: 1; min-width: 0; background: var(--surface-0); padding: var(--space-5); }
  .detail-head { display: flex; align-items: center; justify-content: space-between; gap: var(--space-3); margin-bottom: var(--space-4); flex-wrap: wrap; }
  .detail-head h2 { margin: 0; font-size: 15px; flex: 1; }
  .detail-head-badges { display: flex; align-items: center; gap: var(--space-2); }
  .detail-empty { display: flex; flex-direction: column; align-items: center; justify-content: center; gap: var(--space-3); height: 100%; color: var(--text-secondary); }

  /* 指標表格 */
  .table-scroll { overflow-x: auto; margin-bottom: var(--space-4); }
  .metric-table { width: 100%; border-collapse: collapse; font-variant-numeric: tabular-nums; }
  .metric-table th, .metric-table td { padding: 9px 12px; border-bottom: 1px solid var(--border-subtle); text-align: right; white-space: nowrap; }
  .metric-table th:first-child, .metric-table td:first-child { text-align: left; }
  .metric-table th { color: var(--text-secondary); font-size: 11px; font-weight: var(--font-weight-semibold); letter-spacing: 0.03em; text-transform: uppercase; }
  .metric-table tbody tr:hover { background: var(--surface-1); }
  td.best { background: var(--accent-muted); color: var(--accent); font-weight: var(--font-weight-medium); }
  td.second { background: color-mix(in srgb, var(--accent-muted) 50%, transparent); }
  td.unusual-value { color: var(--warning); }
  .lp-cell { font-weight: var(--font-weight-medium); }

  /* Meta */
  .meta-strip { display: flex; flex-wrap: wrap; gap: var(--space-3) var(--space-6); margin-bottom: var(--space-4); }
  .meta-item { display: flex; flex-direction: column; gap: 2px; }
  .mono { font-family: 'IBM Plex Sans TC', monospace; font-size: 11.5px; }

  /* Policy */
  .policy-section { background: var(--surface-1); border: 1px solid var(--border-subtle); border-radius: var(--radius-md); padding: var(--space-4); }
  .policy-header { display: flex; align-items: baseline; gap: var(--space-3); margin-bottom: var(--space-3); }
  .policy-header h3 { margin: 0; font-size: 13px; font-weight: var(--font-weight-semibold); }
  .policy-list { display: flex; flex-direction: column; gap: 6px; margin: 0 0 var(--space-3); }
  .policy-list div { display: flex; gap: var(--space-3); }
  .policy-list dt { color: var(--text-secondary); min-width: 120px; font-size: 12px; }
  .policy-list dd { margin: 0; font-size: 12px; }
  .policy-actions { display: flex; flex-wrap: wrap; align-items: center; gap: var(--space-2); }
  .apply-reason, .mismatch { max-width: 360px; }
  .mismatch { color: var(--warning); }

  /* Reliability */
  .reliability-block { display: flex; flex-direction: column; gap: var(--space-3); border: 1px solid var(--border-subtle); border-radius: var(--radius-md); padding: var(--space-4); margin-bottom: var(--space-4); }
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

  /* Evidence */
  .evidence-block { border: 1px solid var(--border-subtle); border-radius: var(--radius-md); padding: var(--space-4); margin-bottom: var(--space-4); }
  .evidence-block h3 { margin: 0 0 var(--space-3); font-size: 13px; font-weight: var(--font-weight-semibold); }
  .evidence-grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(150px, 1fr)); gap: var(--space-2) var(--space-4); margin: 0; }
  .evidence-grid div { display: flex; flex-direction: column; gap: 2px; }
  .evidence-grid dt { color: var(--text-secondary); font-size: 11px; }
  .evidence-grid dd { margin: 0; font-size: 13px; font-weight: var(--font-weight-medium); }
  .evidence-item.verified dd { color: var(--success); }

  /* Equivalent-mode */
  .equivalent-block { display: flex; flex-direction: column; gap: var(--space-3); border: 1px solid var(--accent); border-radius: var(--radius-md); padding: var(--space-4); margin-bottom: var(--space-4); }
  .equivalent-block h3 { margin: var(--space-2) 0 0; font-size: 13px; font-weight: var(--font-weight-semibold); }
  .equivalent-notice { display: flex; align-items: center; gap: var(--space-2); color: var(--accent); font-size: 13px; font-weight: var(--font-weight-medium); }
  .equivalent-select { display: flex; gap: var(--space-2); flex-wrap: wrap; }
  .equivalent-select button { min-width: 96px; }
  .equivalent-select button.selected { background: var(--accent); border-color: var(--accent); color: var(--accent-text); }
  .equivalent-select button.current { border-color: var(--accent); }
  .equivalent-evidence { display: flex; flex-wrap: wrap; gap: var(--space-1) var(--space-4); margin: 0; font-size: 12px; font-variant-numeric: tabular-nums; }
  .equivalent-evidence div { display: flex; gap: var(--space-2); }
  .equivalent-evidence dt { color: var(--text-secondary); }
  .equivalent-evidence dd { margin: 0; font-weight: var(--font-weight-medium); }
  .equivalent-hint { font-size: 12px; }
  .equivalent-validation { display: flex; flex-direction: column; gap: var(--space-2); padding: var(--space-3); border-radius: var(--radius-md); background: var(--surface-2); }
  .equivalent-validation.passed { border: 1px solid var(--success); }
  .equivalent-validation.failed { border: 1px solid var(--warning); }

  /* Progress */
  .progress-track { height: 8px; background: var(--surface-2); border-radius: var(--radius-full); overflow: hidden; margin-bottom: var(--space-3); }
  .progress-track > div { height: 100%; background: var(--accent); transition: width 0.3s; border-radius: var(--radius-full); }

  @media (max-width: 999px) {
    .results-workspace { flex-direction: column; }
    .session-list { width: 100%; border-right: none; border-bottom: 1px solid var(--border-subtle); }
  }

  /* ── compact progress 模式：內容壓縮、無滾動、cancel 固定可見 ── */
  .gpu-test.compact { height: 100%; }
  .gpu-test.compact .running-panel {
    flex: 1 1 auto;
    min-height: 0;
    display: flex;
    flex-direction: column;
    overflow: hidden;
    padding: var(--space-3);
  }
  .gpu-test.compact .running-panel h2 {
    flex: 0 0 auto;
    margin: 0 0 var(--space-2);
    font-size: 14px;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .gpu-test.compact .running-meta {
    flex: 0 0 auto;
    grid-template-columns: 1fr 1fr;
    gap: var(--space-2) var(--space-3);
    margin: 0 0 var(--space-2);
  }
  .gpu-test.compact .running-meta dt { font-size: 10.5px; }
  .gpu-test.compact .running-meta dd {
    font-size: 12.5px;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .gpu-test.compact .progress-track {
    flex: 0 0 auto;
    margin-bottom: var(--space-2);
  }
  .gpu-test.compact .action-row {
    flex: 0 0 auto;
    margin-top: auto;
    padding-top: var(--space-2);
  }
</style>
