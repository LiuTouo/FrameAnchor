//! 每核心使用率（PLAN §7.5）：NtQuerySystemInformation(SystemProcessorPerformanceInformation=8)
//! 每秒取樣，與上次取樣算差值。面板未開啟時暫停（省電設計）。

use std::sync::Arc;

use tauri::{AppHandle, Emitter};
use windows::Win32::System::SystemInformation::{GetSystemInfo, SYSTEM_INFO};

use crate::AppState;

const SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION: i32 = 8;

#[link(name = "ntdll")]
extern "system" {
    fn NtQuerySystemInformation(class: i32, info: *mut u8, len: u32, ret_len: *mut u32) -> i32;
}

/// SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION（per-LP）
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Sppi {
    idle_time: i64,
    kernel_time: i64, // 注意：已包含 idle_time
    user_time: i64,
    dpc_time: i64,
    interrupt_time: i64,
    interrupt_count: u32,
}

fn num_logical_processors() -> usize {
    let mut info = SYSTEM_INFO::default();
    unsafe { GetSystemInfo(&mut info) };
    info.dwNumberOfProcessors.max(1) as usize
}

/// 取樣器：保存上次計數，sample() 回傳每 LP 使用率 0..1
pub struct Sampler {
    prev: Vec<Sppi>,
    count: usize,
}

impl Sampler {
    pub fn new() -> Self {
        let count = num_logical_processors();
        Self {
            prev: vec![Sppi::default(); count],
            count,
        }
    }

    fn read_counters(&self) -> Vec<Sppi> {
        let mut cur = vec![Sppi::default(); self.count];
        let mut ret_len: u32 = 0;
        unsafe {
            NtQuerySystemInformation(
                SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION,
                cur.as_mut_ptr() as *mut u8,
                (self.count * std::mem::size_of::<Sppi>()) as u32,
                &mut ret_len,
            );
        }
        cur
    }

    pub fn sample(&mut self) -> Vec<f32> {
        let cur = self.read_counters();
        let utils = cur
            .iter()
            .zip(self.prev.iter())
            .map(|(c, p)| {
                let idle = c.idle_time - p.idle_time;
                let busy = (c.kernel_time - p.kernel_time) + (c.user_time - p.user_time);
                if busy <= 0 {
                    0.0
                } else {
                    (1.0 - idle as f64 / busy as f64).clamp(0.0, 1.0) as f32
                }
            })
            .collect();
        self.prev = cur;
        utils
    }
}

/// usage task：streaming 開啟時每秒 emit `usage-update`，關閉時等待（PLAN §7.5）
pub fn spawn(app: AppHandle, state: Arc<AppState>) {
    tauri::async_runtime::spawn(async move {
        let mut rx = state.usage_tx.subscribe();
        let mut sampler = Sampler::new();
        // 丟掉第一次取樣（與全零 prev 的差值無意義）
        sampler.sample();
        loop {
            if *rx.borrow() {
                let utils = sampler.sample();
                let _ = app.emit("usage-update", utils);
                tokio::select! {
                    _ = tokio::time::sleep(std::time::Duration::from_secs(1)) => {},
                    _ = rx.changed() => {},
                }
            } else {
                // 暫停：等 streaming 開啟
                if rx.changed().await.is_err() {
                    break;
                }
            }
        }
    });
}
