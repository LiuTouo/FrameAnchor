//! 資料模型（PLAN §5.2）。JSON 序列化：struct 欄位 camelCase、enum variant PascalCase。

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    pub version: u32,
    #[serde(default)]
    pub settings: Settings,
    #[serde(default)]
    pub rules: Vec<Rule>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: 1,
            settings: Settings::default(),
            rules: Vec::new(),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    #[serde(default = "default_language")]
    pub language: String,
    #[serde(default)]
    pub start_with_windows: bool,
    #[serde(default = "default_true")]
    pub start_minimized: bool,
    #[serde(default = "default_true")]
    pub close_to_tray: bool,
    #[serde(default = "default_poll_interval")]
    pub poll_interval_ms: u64,
    #[serde(default)]
    pub show_advanced_priorities: bool,
}

fn default_language() -> String {
    "zh-TW".to_string()
}
fn default_true() -> bool {
    true
}
fn default_poll_interval() -> u64 {
    1000
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            language: default_language(),
            start_with_windows: false,
            start_minimized: true,
            close_to_tray: true,
            poll_interval_ms: default_poll_interval(),
            show_advanced_priorities: false,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Rule {
    pub id: String,
    pub name: String,
    pub exe_path: String,
    #[serde(default)]
    pub match_by: MatchBy,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub affinity: AffinitySpec,
    #[serde(default)]
    pub priority: CpuPriority,
    #[serde(default)]
    pub advanced: AdvancedSpec,
}

impl Rule {
    pub fn new(exe_path: String, name: String) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name,
            exe_path,
            match_by: MatchBy::FullPath,
            enabled: true,
            affinity: AffinitySpec::default(),
            priority: CpuPriority::High,
            advanced: AdvancedSpec::default(),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub enum MatchBy {
    #[default]
    FullPath,
    FileName,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct AffinitySpec {
    #[serde(default)]
    pub mode: AffinityMode,
    #[serde(default)]
    pub cores: Vec<u32>,
}

impl Default for AffinitySpec {
    fn default() -> Self {
        Self {
            mode: AffinityMode::All,
            cores: Vec::new(),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub enum AffinityMode {
    #[default]
    All,
    NoSmtSibling,
    PCoresOnly,
    Custom,
    /// 軟綁定：僅設定執行緒 ideal processor（偏好核心），不硬排除其他核心
    Prefer,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Default)]
pub enum CpuPriority {
    Idle,
    BelowNormal,
    #[default]
    Normal,
    AboveNormal,
    High,
}

impl CpuPriority {
    /// 顯示用字串（面板「實際狀態」欄）
    pub fn as_str(&self) -> &'static str {
        match self {
            CpuPriority::Idle => "Idle",
            CpuPriority::BelowNormal => "BelowNormal",
            CpuPriority::Normal => "Normal",
            CpuPriority::AboveNormal => "AboveNormal",
            CpuPriority::High => "High",
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct AdvancedSpec {
    #[serde(default)]
    pub io_priority: Option<IoPriority>,
    #[serde(default)]
    pub memory_priority: Option<MemPriority>,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub enum IoPriority {
    VeryLow,
    Low,
    Normal,
    High,
}

impl IoPriority {
    pub fn to_raw(&self) -> u32 {
        match self {
            IoPriority::VeryLow => 0,
            IoPriority::Low => 1,
            IoPriority::Normal => 2,
            IoPriority::High => 3,
        }
    }

    pub fn from_raw(v: u32) -> Self {
        match v {
            0 => IoPriority::VeryLow,
            1 => IoPriority::Low,
            3 => IoPriority::High,
            _ => IoPriority::Normal,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub enum MemPriority {
    VeryLow,
    Low,
    Medium,
    BelowNormal,
    Normal,
}

impl MemPriority {
    pub fn to_raw(&self) -> u32 {
        match self {
            MemPriority::VeryLow => 1,
            MemPriority::Low => 2,
            MemPriority::Medium => 3,
            MemPriority::BelowNormal => 4,
            MemPriority::Normal => 5,
        }
    }

    pub fn from_raw(v: u32) -> Self {
        match v {
            1 => MemPriority::VeryLow,
            2 => MemPriority::Low,
            3 => MemPriority::Medium,
            4 => MemPriority::BelowNormal,
            _ => MemPriority::Normal,
        }
    }
}
