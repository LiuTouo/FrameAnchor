//! 基準測試分析（Task 2）：PresentMon CSV 解析、frametime→FPS、
//! AutoGpuAffinity 相容的統計（Max/Avg/Min/STDEV + 1/0.1/0.01/0.005
//! time-weighted lows）、dense rank、最佳 LP、嚴重 LP。
//!
//! 全為純函式，fixture 測試不需真實 GPU/驅動。

use super::LpResult;

/// PresentMon CSV 中 frametime 欄位名（毫秒）
pub const COL_MS_BETWEEN_PRESENTS: &str = "msBetweenPresents";

/// 解析 PresentMon 1.x/2.x CSV，回傳 `MsBetweenPresents` 的 frametime 序列。
/// 欄名不分大小寫；2.x 的 `NA`/空值列會跳過。空 header、缺欄或沒有任何
/// 有效有限正數 → Err（該 CSV 視為無效）。
pub fn parse_presentmon_csv(text: &str) -> Result<Vec<f64>, String> {
    let mut header_idx: Option<usize> = None;
    let mut frames: Vec<f64> = Vec::new();
    let mut saw_data = false;

    for (lineno, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        let fields = split_csv_line(line);
        if header_idx.is_none() {
            if let Some(i) = fields
                .iter()
                .position(|f| f.trim().eq_ignore_ascii_case(COL_MS_BETWEEN_PRESENTS))
            {
                header_idx = Some(i);
            }
            continue; // header 行本身不是資料
        }
        let idx = header_idx.unwrap();
        if idx >= fields.len() {
            continue; // 該列缺欄 → 跳過
        }
        let value = fields[idx].trim();
        if value.is_empty() || value.eq_ignore_ascii_case("NA") {
            continue;
        }
        let v: f64 = value.parse().map_err(|_| {
            format!(
                "CSV 第 {} 行 msBetweenPresents 非數值: {:?}",
                lineno + 1,
                fields[idx]
            )
        })?;
        if v.is_finite() && v > 0.0 {
            frames.push(v);
            saw_data = true;
        }
    }

    match header_idx {
        None => Err("CSV 缺 msBetweenPresents 欄位".to_string()),
        Some(_) if !saw_data => Err("CSV 沒有有效 frametime 資料".to_string()),
        Some(_) => Ok(frames),
    }
}

/// 對含引號欄位的 CSV 列做正確的欄位切割（PresentMon 的 Application 帶引號）
fn split_csv_line(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                if in_quotes && chars.peek() == Some(&'"') {
                    cur.push('"');
                    chars.next();
                } else {
                    in_quotes = !in_quotes;
                }
            }
            ',' if !in_quotes => out.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    out.push(cur);
    out
}

/// frametime(ms) → 即時 FPS（1000/ms）。輸入皆 > 0。
pub fn frame_times_to_fps(frames: &[f64]) -> Vec<f64> {
    frames.iter().map(|&ms| 1000.0 / ms).collect()
}

/// Bessel 校正（n-1）的母體標準差
fn stdev_bessel(values: &[f64]) -> f64 {
    let n = values.len();
    if n < 2 {
        return 0.0;
    }
    let mean = values.iter().sum::<f64>() / n as f64;
    let var = values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / (n as f64 - 1.0);
    var.sqrt()
}

/// time-weighted low：把 frametimes 排序後以「時間比例」取分位，
/// 回傳對應的 FPS。`low_quantile` 例：1% low → 0.01。
/// 定義：找最小的 frametime，使累積時間佔總時間 ≥ 1 - low_quantile，
/// FPS = 1000 / 該 frametime（該時間比例內至少有此 FPS）。
pub fn time_weighted_low_fps(frames: &[f64], low_quantile: f64) -> f64 {
    if frames.is_empty() {
        return 0.0;
    }
    let mut sorted = frames.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let total: f64 = sorted.iter().sum();
    if total <= 0.0 {
        return 0.0;
    }
    let target = 1.0 - low_quantile;
    let mut cum = 0.0;
    for &ft in &sorted {
        cum += ft;
        if cum / total >= target {
            return 1000.0 / ft;
        }
    }
    1000.0 / sorted[sorted.len() - 1]
}

/// 從合併後的 frametime 序列計算單一 LP 的完整指標。
/// 空序列 → Err（該 LP 無效）。
pub fn compute_lp_result(lp: u32, frames: &[f64]) -> Result<LpResult, String> {
    if frames.is_empty() {
        return Err(format!("LP {lp} 無 frametime 資料"));
    }
    let fps = frame_times_to_fps(frames);
    let n = fps.len() as u32;
    let avg_ft = frames.iter().sum::<f64>() / frames.len() as f64;
    // AutoGpuAffinity 相容的 Avg FPS：1000 / mean(frametime_ms)，
    // 不是 mean(1000/frametime)（對非均勻 frametime 兩者不同）
    let avg = 1000.0 / avg_ft;
    // Max/Min 與 STDEV 仍是針對 instantaneous FPS
    let max = fps.iter().cloned().fold(f64::MIN, f64::max);
    let min = fps.iter().cloned().fold(f64::MAX, f64::min);
    Ok(LpResult {
        lp,
        avg_fps: Some(avg),
        max_fps: Some(max),
        min_fps: Some(min),
        stdev_fps: Some(stdev_bessel(&fps)),
        p1_low: Some(time_weighted_low_fps(frames, 0.01)),
        p01_low: Some(time_weighted_low_fps(frames, 0.001)),
        p001_low: Some(time_weighted_low_fps(frames, 0.0001)),
        p0005_low: Some(time_weighted_low_fps(frames, 0.00005)),
        avg_frame_time_ms: Some(avg_ft),
        sample_count: n,
        completed: true,
        error: None,
    })
}

/// 把各 round 的 frametime 合併成單一序列（round 順序不重要，統計用）
pub fn merge_rounds(per_round: &[Vec<f64>]) -> Vec<f64> {
    per_round.iter().flatten().copied().collect()
}

// ── 排序與選擇 ──────────────────────────────────────────────────────────

/// 單一欄位的 dense rank（1 起跳、同值同 rank、無空位）。
/// `descending=true`：值越大 rank 越小（越好）。
/// 同時保留第 1、2 個不同值（顯示表用，避免外觀上的 tie）。
pub struct ColumnRank {
    pub ranks: Vec<usize>,
    // Task 3 顯示表用：保留第 1、2 個不同值，避免外觀 tie
    #[allow(dead_code)]
    pub first: Option<f64>,
    #[allow(dead_code)]
    pub second: Option<f64>,
}

pub fn rank_column(values: &[f64], descending: bool) -> ColumnRank {
    let mut order: Vec<usize> = (0..values.len()).collect();
    order.sort_by(|&a, &b| {
        let ord = values[a]
            .partial_cmp(&values[b])
            .unwrap_or(std::cmp::Ordering::Equal);
        if descending {
            ord.reverse()
        } else {
            ord
        }
    });
    let mut ranks = vec![0usize; values.len()];
    let mut distinct: Vec<f64> = Vec::new();
    let mut r = 0usize;
    for &i in &order {
        if distinct.last().map(|&d| d != values[i]).unwrap_or(true) {
            r += 1;
            distinct.push(values[i]);
        }
        ranks[i] = r;
    }
    ColumnRank {
        ranks,
        first: distinct.first().copied(),
        second: distinct.get(1).copied(),
    }
}

fn median(values: &[f64]) -> f64 {
    let mut v = values.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = v.len();
    if n == 0 {
        return 0.0;
    }
    if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    }
}

/// 完成且四項排序指標皆有的結果（缺任一 → 無法參與排名）
fn complete_rows<'a>(results: &'a [LpResult]) -> Vec<&'a LpResult> {
    results
        .iter()
        .filter(|r| {
            r.completed
                && r.avg_fps.is_some()
                && r.p1_low.is_some()
                && r.p01_low.is_some()
                && r.stdev_fps.is_some()
        })
        .collect()
}

/// 最佳 LP：四欄 dense-rank 總和（Avg desc、1% Low desc、0.1% Low desc、
/// STDEV asc），平手依 0.1% Low、1% Low、Avg（皆越高越好）、LP 越小越好。
pub fn best_lp(results: &[LpResult]) -> Option<u32> {
    let rows = complete_rows(results);
    if rows.is_empty() {
        return None;
    }
    let avg_r = rank_column(
        &rows.iter().map(|r| r.avg_fps.unwrap()).collect::<Vec<_>>(),
        true,
    );
    let p1_r = rank_column(
        &rows.iter().map(|r| r.p1_low.unwrap()).collect::<Vec<_>>(),
        true,
    );
    let p01_r = rank_column(
        &rows.iter().map(|r| r.p01_low.unwrap()).collect::<Vec<_>>(),
        true,
    );
    let stdev_r = rank_column(
        &rows
            .iter()
            .map(|r| r.stdev_fps.unwrap())
            .collect::<Vec<_>>(),
        false,
    );

    let mut best: usize = 0;
    let mut best_key: (usize, f64, f64, f64, u32) =
        (usize::MAX, f64::MIN, f64::MIN, f64::MIN, u32::MAX);
    for (i, row) in rows.iter().enumerate() {
        let sum = avg_r.ranks[i] + p1_r.ranks[i] + p01_r.ranks[i] + stdev_r.ranks[i];
        let key = (
            sum,
            row.p01_low.unwrap(),
            row.p1_low.unwrap(),
            row.avg_fps.unwrap(),
            row.lp,
        );
        if better_key(&key, &best_key) {
            best_key = key;
            best = i;
        }
    }
    Some(rows[best].lp)
}

fn better_key(a: &(usize, f64, f64, f64, u32), b: &(usize, f64, f64, f64, u32)) -> bool {
    if a.0 != b.0 {
        return a.0 < b.0;
    }
    if a.1 != b.1 {
        return a.1 > b.1;
    }
    if a.2 != b.2 {
        return a.2 > b.2;
    }
    if a.3 != b.3 {
        return a.3 > b.3;
    }
    a.4 < b.4
}

/// 嚴重 LP：Avg、1% Low、0.1% Low 任一低於該指標中位數的 85%，
/// 或 STDEV 高於中位數的 150%。中位數 STDEV 為 0 時停用 STDEV 條件。
pub fn severe_lps(results: &[LpResult]) -> Vec<u32> {
    let rows = complete_rows(results);
    if rows.is_empty() {
        return Vec::new();
    }
    let med_avg = median(&rows.iter().map(|r| r.avg_fps.unwrap()).collect::<Vec<_>>());
    let med_p1 = median(&rows.iter().map(|r| r.p1_low.unwrap()).collect::<Vec<_>>());
    let med_p01 = median(&rows.iter().map(|r| r.p01_low.unwrap()).collect::<Vec<_>>());
    let med_stdev = median(
        &rows
            .iter()
            .map(|r| r.stdev_fps.unwrap())
            .collect::<Vec<_>>(),
    );
    let stdev_active = med_stdev > 0.0;

    rows.iter()
        .filter(|r| {
            let avg_sev = r.avg_fps.unwrap() < 0.85 * med_avg;
            let p1_sev = r.p1_low.unwrap() < 0.85 * med_p1;
            let p01_sev = r.p01_low.unwrap() < 0.85 * med_p01;
            let stdev_sev = stdev_active && r.stdev_fps.unwrap() > 1.5 * med_stdev;
            avg_sev || p1_sev || p01_sev || stdev_sev
        })
        .map(|r| r.lp)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 產生 N 個 frametime，圍繞 base 上下 jitter
    fn frames_n(base: f64, n: usize, jitter: f64) -> Vec<f64> {
        (0..n)
            .map(|i| {
                let j = if i % 2 == 0 { jitter } else { -jitter };
                (base + j).max(0.5)
            })
            .collect()
    }

    #[test]
    fn csv_parses_ms_between_presents_column() {
        let csv = "\
Application,ProcessID,SwapChainAddress,SyncInterval,msBetweenPresents,msInPresentAPI
\"game.exe (1234)\",1234,0x1234,1,16.7,0.5
\"game.exe (1234)\",1234,0x1234,1,8.3,0.4
";
        let frames = parse_presentmon_csv(csv).unwrap();
        assert_eq!(frames, vec![16.7, 8.3]);
    }

    #[test]
    fn csv_v2_header_is_case_insensitive_and_skips_na() {
        let csv = "Application,ProcessID,MsBetweenPresents\ngame.exe,42,NA\ngame.exe,42,0.125\n";
        assert_eq!(parse_presentmon_csv(csv).unwrap(), vec![0.125]);
    }

    #[test]
    fn csv_skips_non_finite_frame_times() {
        let csv = "Application,MsBetweenPresents\ngame.exe,NaN\ngame.exe,inf\ngame.exe,1.5\n";
        assert_eq!(parse_presentmon_csv(csv).unwrap(), vec![1.5]);
    }

    #[test]
    fn csv_with_quoted_comma_field_keeps_alignment() {
        let csv = "\
Application,ProcessID,msBetweenPresents
\"proc, with comma (7)\",7,10.0
\"proc2\",8,20.0
";
        let frames = parse_presentmon_csv(csv).unwrap();
        assert_eq!(frames, vec![10.0, 20.0]);
    }

    #[test]
    fn csv_missing_column_errors() {
        assert!(parse_presentmon_csv("Application,ProcessID\n1,2\n").is_err());
    }

    #[test]
    fn csv_empty_data_errors() {
        let csv = "Application,msBetweenPresents\n";
        assert!(parse_presentmon_csv(csv).is_err());
        // 全 0 / 負 frametime → 視為無資料
        let csv = "Application,msBetweenPresents\n0,0\n-1\n";
        assert!(parse_presentmon_csv(csv).is_err());
    }

    #[test]
    fn csv_invalid_numeric_row_errors() {
        let csv = "Application,msBetweenPresents\nabc,xyz\n";
        assert!(parse_presentmon_csv(csv).is_err());
    }

    #[test]
    fn fps_conversion_and_basic_stats() {
        let frames = frames_n(10.0, 4, 2.0); // 12,8,12,8
        let fps = frame_times_to_fps(&frames);
        let m = compute_lp_result(3, &frames).unwrap();
        assert_eq!(m.lp, 3);
        assert_eq!(m.sample_count, 4);
        // avg = 1000 / mean(frametime) = 1000 / 10 = 100（非 mean(instant fps) ≈ 104.17）
        assert_eq!(m.avg_fps.unwrap(), 100.0);
        assert_eq!(m.max_fps.unwrap(), 125.0);
        assert!((m.min_fps.unwrap() - 83.33333).abs() < 0.01);
        let _ = fps;
    }

    /// 非均勻 frametime：明確區分 1000/mean(frametime) 與 mean(1000/frametime)
    #[test]
    fn avg_fps_is_1000_over_mean_frame_time() {
        // frames [10, 20]ms：mean(instant FPS) = (100+50)/2 = 75；
        // AutoGpuAffinity 定義 avg = 1000 / 15 ≈ 66.67
        let m = compute_lp_result(0, &[10.0, 20.0]).unwrap();
        let expected = 1000.0 / 15.0;
        assert!(
            (m.avg_fps.unwrap() - expected).abs() < 1e-9,
            "avg={}",
            m.avg_fps.unwrap()
        );
        assert!(
            (m.avg_fps.unwrap() - 75.0).abs() > 1e-6,
            "不得是 mean(instant fps)"
        );
        assert_eq!(m.avg_frame_time_ms.unwrap(), 15.0);
        // max/min 仍是 instantaneous FPS
        assert_eq!(m.max_fps.unwrap(), 100.0);
        assert_eq!(m.min_fps.unwrap(), 50.0);
    }

    #[test]
    fn stdev_uses_bessel_correction() {
        // 兩樣本 [100, 110]：mean=105, 樣本變異=((5)^2+(-5)^2)/(2-1)=50, stdev≈7.071
        let frames = vec![10.0, 1000.0 / 110.0]; // fps=100,110
        let m = compute_lp_result(0, &frames).unwrap();
        let expected = (50.0_f64).sqrt();
        assert!((m.stdev_fps.unwrap() - expected).abs() < 1e-9);
        // 單一樣本 stdev = 0（Bessel n-1 = 0）
        let m1 = compute_lp_result(1, &[16.7]).unwrap();
        assert_eq!(m1.stdev_fps.unwrap(), 0.0);
    }

    #[test]
    fn time_weighted_low_uses_frametime_fraction() {
        // 9 個 5ms + 1 個 100ms：總時間 145ms。
        // 1% low：target=0.99，累積到 100ms(140/145=0.965) 還不到，需全部 → 1000/100=10
        let mut frames = vec![100.0];
        frames.extend(std::iter::repeat(5.0).take(9));
        let low = time_weighted_low_fps(&frames, 0.01);
        assert!((low - 10.0).abs() < 1e-9, "low={low}");
        // 100% 的所有樣本都在 5ms 以下 → 1% low 為 5ms → 200 FPS
        let low_all = time_weighted_low_fps(&[5.0; 10], 0.01);
        assert!((low_all - 200.0).abs() < 1e-9);
    }

    #[test]
    fn compute_lp_result_empty_errors() {
        assert!(compute_lp_result(0, &[]).is_err());
    }

    #[test]
    fn merge_rounds_concatenates() {
        let merged = merge_rounds(&[vec![1.0, 2.0], vec![3.0], vec![]]);
        assert_eq!(merged, vec![1.0, 2.0, 3.0]);
    }

    #[test]
    fn dense_rank_descending_with_ties() {
        let vals = [100.0, 200.0, 200.0, 50.0];
        let c = rank_column(&vals, true);
        // 降序：200,200 → rank1；100 → rank2；50 → rank3
        assert_eq!(c.ranks, vec![2, 1, 1, 3]);
        assert_eq!(c.first, Some(200.0));
        assert_eq!(c.second, Some(100.0));
    }

    #[test]
    fn dense_rank_ascending() {
        let vals = [10.0, 5.0, 5.0, 20.0];
        let c = rank_column(&vals, false);
        assert_eq!(c.ranks, vec![2, 1, 1, 3]);
    }

    fn lp(avg: f64, p1: f64, p01: f64, stdev: f64) -> LpResult {
        LpResult {
            lp: 0,
            avg_fps: Some(avg),
            p1_low: Some(p1),
            p01_low: Some(p01),
            stdev_fps: Some(stdev),
            completed: true,
            ..Default::default()
        }
    }

    #[test]
    fn best_lp_picks_sum_of_dense_ranks() {
        // LP 由 avg 欄位帶出
        let mut a = lp(100.0, 90.0, 80.0, 5.0);
        a.lp = 1;
        let mut b = lp(90.0, 85.0, 75.0, 2.0);
        b.lp = 2;
        let mut c = lp(95.0, 92.0, 78.0, 8.0);
        c.lp = 3;
        let r = best_lp(&[a, b, c]);
        assert_eq!(r, Some(1)); // LP1 avg/p1/p01 最好，總和最小
    }

    #[test]
    fn best_lp_tie_break_by_p01() {
        // 四欄 dense rank 總和相同（6=6），由 0.1% Low 較高者勝
        let mut a = lp(100.0, 80.0, 95.0, 10.0);
        a.lp = 4;
        let mut b = lp(80.0, 100.0, 80.0, 5.0);
        b.lp = 7;
        assert_eq!(best_lp(&[a, b]), Some(4));
    }

    #[test]
    fn best_lp_tie_break_lower_lp_when_all_equal() {
        let mut a = lp(100.0, 90.0, 80.0, 5.0);
        a.lp = 3;
        let mut b = lp(100.0, 90.0, 80.0, 5.0);
        b.lp = 6;
        assert_eq!(best_lp(&[a, b]), Some(3));
    }

    #[test]
    fn best_lp_skips_incomplete_rows() {
        let mut a = lp(100.0, 90.0, 80.0, 5.0);
        a.lp = 1;
        let mut b = LpResult::default();
        b.lp = 2; // completed=false
        let r = best_lp(&[a, b]);
        assert_eq!(r, Some(1));
    }

    #[test]
    fn severe_flags_below_85_percent_of_median() {
        // 中位數：avg=100,p1=90,p01=80,stdev=5
        let mut a = lp(100.0, 90.0, 80.0, 5.0);
        a.lp = 1;
        let mut bad = lp(80.0, 90.0, 80.0, 5.0); // avg 80 < 85
        bad.lp = 2;
        let mut stdev_bad = lp(100.0, 90.0, 80.0, 9.0); // stdev 9 > 1.5*5=7.5
        stdev_bad.lp = 3;
        let sev = severe_lps(&[a, bad, stdev_bad]);
        assert_eq!(sev, vec![2, 3]);
    }

    #[test]
    fn severe_disables_stdev_condition_when_median_zero() {
        // 全部 stdev = 0 → 中位數 0 → STDEV 條件停用
        let mut a = lp(100.0, 90.0, 80.0, 0.0);
        a.lp = 1;
        let mut b = lp(100.0, 90.0, 80.0, 0.0);
        b.lp = 2;
        let mut c = lp(60.0, 90.0, 80.0, 0.0); // avg 60 < 85 → 仍嚴重
        c.lp = 3;
        let sev = severe_lps(&[a, b, c]);
        assert_eq!(sev, vec![3]);
    }
}
