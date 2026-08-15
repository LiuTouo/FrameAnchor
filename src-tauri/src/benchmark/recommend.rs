//! 基準測試推薦 → Rule 核心集合（Task 5）。
//! 語意：永遠排除「含 best LP 的整個實體核心」的所有 LP；core 0 僅在排除後
//! 仍保留至少 [`MIN_GAME_PHYSICAL_CORES`] 顆實體核心時才一併排除，否則保留給遊戲
//! （除非 core 0 本身就是 best core）。其餘支援的 group-0 LP 全部保留。
//! severe LP 僅作為測試結果與 UI 標註，不用來排除遊戲可用核心。
//! 排他以實體核心為單位，不會只排除單一 SMT LP。
//! 純函式，fixture 測試涵蓋 SMT/非 SMT/重疊/同核心多 severe/空結果/核心不足。

use crate::topology::Topology;

/// 遊戲核心排除後仍須保留的最少實體核心數。低於此值則不排除 core 0。
pub const MIN_GAME_PHYSICAL_CORES: usize = 6;

/// 排除 best LP 所屬實體核心的所有 LP，並在保留核心仍足夠時額外排除 core 0；
/// 回傳其餘支援 LP（依 index 排序，維持整顆核心的 SMT 語意）。
/// `severe_lps` 為了 IPC 相容性保留，不參與推薦集合計算。
/// 回傳空集合 = 沒有可用的核心（UI 需停用 import）。
pub fn recommended_cores(topo: &Topology, best_lp: u32, _severe_lps: &[u32]) -> Vec<u32> {
    // best LP 所屬實體核心（找不到則視為無 best core）
    let best_core = topo
        .logical_processors
        .iter()
        .find(|lp| lp.index == best_lp)
        .map(|lp| lp.core_id);

    // 支援的 group-0 LP（index < 64）所代表的 distinct 實體核心。
    // index >= 64 的 LP 既不算入核心數，也不回傳。
    let mut supported_cores: Vec<u32> = topo
        .logical_processors
        .iter()
        .filter(|lp| lp.index < 64)
        .map(|lp| lp.core_id)
        .collect();
    supported_cores.sort_unstable();
    supported_cores.dedup();

    // 同時排除 best core 與 core 0 後仍剩餘的 distinct 實體核心數。
    let remaining_after_both = supported_cores
        .iter()
        .filter(|&&c| Some(c) != best_core && c != 0)
        .count();

    // 剩餘核心達門檻才排除 core 0；否則保留 core 0 給遊戲（core 0 是 best core 時
    // 已由上面的 best_core 排除涵蓋，無需額外處理）。
    let exclude_core0 = remaining_after_both >= MIN_GAME_PHYSICAL_CORES;

    topo.logical_processors
        .iter()
        .filter(|lp| lp.index < 64)
        .filter(|lp| Some(lp.core_id) != best_core)
        .filter(|lp| !(exclude_core0 && lp.core_id == 0))
        .map(|lp| lp.index)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::topology::build_topology;

    /// 非 SMT 8C（每核心 1 LP）
    fn topo_8c() -> Topology {
        build_topology((0..8u32).map(|c| (vec![c], 0, false)).collect())
    }

    /// SMT 8C16T（每核心 2 LP：2c, 2c+1）
    fn topo_8c16t() -> Topology {
        build_topology(
            (0..8u32)
                .map(|c| (vec![c * 2, c * 2 + 1], 0, true))
                .collect(),
        )
    }

    #[test]
    fn non_smt_excludes_only_best_core() {
        let t = topo_8c();
        let r = recommended_cores(&t, 0, &[]);
        assert_eq!(r, vec![1, 2, 3, 4, 5, 6, 7]); // LP0 被排除
    }

    #[test]
    fn smt_excludes_entire_physical_core() {
        let t = topo_8c16t();
        let r = recommended_cores(&t, 0, &[]);
        // 核心 0（LP0, LP1）整顆排除，其餘 2..15 保留
        assert_eq!(r.len(), 14);
        assert!(!r.contains(&0) && !r.contains(&1));
        assert!(r.contains(&2) && r.contains(&15));
    }

    #[test]
    fn severe_does_not_remove_its_physical_core() {
        let t = topo_8c16t();
        let r = recommended_cores(&t, 4, &[10]);
        assert!(!r.contains(&4) && !r.contains(&5));
        assert!(!r.contains(&0) && !r.contains(&1));
        assert!(r.contains(&10) && r.contains(&11));
        assert_eq!(r.len(), 12);
    }

    #[test]
    fn severe_on_best_core_does_not_change_result() {
        let t = topo_8c16t();
        let r = recommended_cores(&t, 2, &[3]); // 都屬核心 1（LP2,3）
        assert!(!r.contains(&2) && !r.contains(&3));
        // 8 核心、best core !=0：排除後仍剩 6 核心 → core 0 一併排除
        assert!(!r.contains(&0) && !r.contains(&1));
        assert_eq!(r.len(), 12);
    }

    #[test]
    fn multiple_severe_lps_are_ignored() {
        let t = topo_8c16t();
        let r = recommended_cores(&t, 0, &[1, 2, 3]);
        assert_eq!(r.len(), 14);
        assert!(!r.contains(&0) && !r.contains(&1));
        assert!(r.contains(&2) && r.contains(&3));
    }

    #[test]
    fn excludes_core_zero_when_six_cores_remain() {
        // 8 核心、best core 5（LP10,11）：排除 best core 後仍剩 7 核心，
        // 再排除 core 0 剩 6 核心（>= 門檻）→ core 0 一併排除。
        let t = topo_8c16t();
        let r = recommended_cores(&t, 10, &[]);
        assert!(!r.contains(&0) && !r.contains(&1)); // core 0
        assert!(!r.contains(&10) && !r.contains(&11)); // best LP 所屬 core 5
        assert_eq!(r.len(), 12);
    }

    #[test]
    fn single_physical_core_yields_empty() {
        let t = build_topology(vec![(vec![0, 1], 0, true)]); // 只有 1 顆核心
        let r = recommended_cores(&t, 0, &[]);
        assert!(r.is_empty(), "只有一顆核心時沒有可保留的 LP");
    }

    /// 7 實體核心、best core !=0：排除後剩 5 核心 < 門檻 → 保留 core 0。
    #[test]
    fn retains_core_zero_on_seven_cores() {
        let t = build_topology((0..7u32).map(|c| (vec![c], 0, false)).collect());
        let r = recommended_cores(&t, 1, &[]); // best = core 1
        assert_eq!(r, vec![0, 2, 3, 4, 5, 6]); // core 0 保留、core 1 排除
    }

    /// 6 實體核心、best core !=0：排除後剩 4 核心 < 門檻 → 保留 core 0。
    #[test]
    fn retains_core_zero_on_six_cores() {
        let t = build_topology((0..6u32).map(|c| (vec![c], 0, false)).collect());
        let r = recommended_cores(&t, 1, &[]);
        assert_eq!(r, vec![0, 2, 3, 4, 5]);
    }

    /// 4 實體核心、best core !=0：排除後剩 2 核心 < 門檻 → 保留 core 0。
    #[test]
    fn retains_core_zero_on_four_cores() {
        let t = build_topology((0..4u32).map(|c| (vec![c], 0, false)).collect());
        let r = recommended_cores(&t, 1, &[]);
        assert_eq!(r, vec![0, 2, 3]);
    }

    /// legacy：best LP 落在 core 0 → core 0 只排除一次（best core），其餘全保留。
    #[test]
    fn best_on_core_zero_excludes_core_zero_once() {
        let t = topo_8c16t();
        let r = recommended_cores(&t, 0, &[]); // best = core 0
        assert!(!r.contains(&0) && !r.contains(&1)); // core 0 整顆排除
        assert_eq!(r.len(), 14);
        assert!(r.contains(&2) && r.contains(&15));
    }

    /// index >= 64 的 LP 既不算入核心數、也不回傳（group 0 上限）。
    #[test]
    fn lp_at_or_above_64_is_not_counted_nor_returned() {
        let t = build_topology(vec![
            (vec![0, 1], 0, true),   // core 0（SMT）
            (vec![2], 0, false),     // core 1
            (vec![64, 65], 0, true), // core 2（超出 group 0）
        ]);
        let r = recommended_cores(&t, 2, &[]); // best = core 1（LP2）
        assert!(!r.contains(&64));
        assert!(!r.contains(&65));
        // supported cores 只有 core 0、core 1；排除 best core 後剩 1 核心 < 門檻 → 保留 core 0
        assert_eq!(r, vec![0, 1]);
    }

    #[test]
    fn unknown_severe_lp_is_ignored() {
        let t = topo_8c();
        let r = recommended_cores(&t, 0, &[999]); // 不存在的 LP 忽略
        assert_eq!(r, vec![1, 2, 3, 4, 5, 6, 7]);
    }
}
