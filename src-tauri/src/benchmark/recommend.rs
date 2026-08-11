//! 基準測試推薦 → Rule 核心集合（Task 5）。
//! 語意：排除 core 0 與「含 best LP 的整個實體核心」的所有 LP，
//! 其餘支援的 group-0 LP 全部保留。severe LP 僅作為測試結果與 UI 標註，
//! 不用來排除遊戲可用核心。排他以實體核心為單位，不會只排除單一 SMT LP。
//! 純函式，fixture 測試涵蓋 SMT/非 SMT/重疊/同核心多 severe/空結果。

use crate::topology::Topology;

/// 固定排除 core 0，並排除 best LP 所屬實體核心的所有 LP；
/// 回傳其餘支援 LP（依 index 排序）。
/// `severe_lps` 為了 IPC 相容性保留，不參與推薦集合計算。
/// 回傳空集合 = 沒有可用的核心（UI 需停用 import）。
pub fn recommended_cores(topo: &Topology, best_lp: u32, _severe_lps: &[u32]) -> Vec<u32> {
    let excluded_core = topo
        .logical_processors
        .iter()
        .find(|lp| lp.index == best_lp)
        .map(|lp| lp.core_id);
    topo.logical_processors
        .iter()
        .filter(|lp| lp.core_id != 0 && Some(lp.core_id) != excluded_core)
        .map(|l| l.index)
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
        assert!(!r.contains(&0) && !r.contains(&1)); // 核心 0 固定排除
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
    fn always_excludes_physical_core_zero() {
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

    #[test]
    fn unknown_severe_lp_is_ignored() {
        let t = topo_8c();
        let r = recommended_cores(&t, 0, &[999]); // 不存在的 LP 忽略
        assert_eq!(r, vec![1, 2, 3, 4, 5, 6, 7]);
    }
}
