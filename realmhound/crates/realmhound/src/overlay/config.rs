//! Self-contained configuration and persistence for the RotMG in-game overlay.
//!
//! Stored separately in `<data_local_dir>/RealmHound/overlay_config.json` so
//! upstream changes to `realmhound-core::settings::Settings` never conflict.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tracing::warn;

/// When the overlay should automatically appear.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum VisibilityTrigger {
    /// Show only when RotMG is the active/focused foreground window (or running
    /// on platforms where window focus is not exposed by the compositor).
    #[default]
    WhenRotmgFocused,
    /// Show whenever a RotMG Exalt client process or active session is running.
    WhenRotmgRunning,
    /// Always show the overlay while enabled, even if RotMG is closed.
    AlwaysVisible,
}

impl VisibilityTrigger {
    pub const ALL: [Self; 3] = [
        Self::WhenRotmgFocused,
        Self::WhenRotmgRunning,
        Self::AlwaysVisible,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::WhenRotmgFocused => "When RotMG is Focused",
            Self::WhenRotmgRunning => "While RotMG is Running",
            Self::AlwaysVisible => "Always Show",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::WhenRotmgFocused => {
                "Attaches to RotMG when the game window is active and hides when you Alt-Tab out."
            }
            Self::WhenRotmgRunning => {
                "Stays visible whenever RotMGExalt is running, even while another window is focused."
            }
            Self::AlwaysVisible => {
                "Keeps the overlay visible at all times while enabled (useful for testing or second monitors)."
            }
        }
    }
}

/// Visual layout style of the overlay.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum OverlayDisplayMode {
    /// Translucent glass panel showing mission cards with progress bars and
    /// daily quest rows with mark sprites and counts.
    #[default]
    CompactCards,
    /// Ultra-compact icon-only pill strip (reuses smart mission+quest combining).
    UltraCompactPills,
}

impl OverlayDisplayMode {
    pub const ALL: [Self; 2] = [Self::CompactCards, Self::UltraCompactPills];

    pub fn label(self) -> &'static str {
        match self {
            Self::CompactCards => "Compact Cards & Progress Bars",
            Self::UltraCompactPills => "Ultra-Compact Icon Pills",
        }
    }
}

/// Flow direction when `OverlayDisplayMode::UltraCompactPills` is active.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PillOrientation {
    #[default]
    Horizontal,
    Vertical,
}

impl PillOrientation {
    pub fn label(self) -> &'static str {
        match self {
            Self::Horizontal => "Horizontal Strip",
            Self::Vertical => "Vertical Stack",
        }
    }
}

/// Screen/game-window anchor position for the overlay.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum OverlayAnchor {
    #[default]
    TopLeft,
    BottomLeft,
    TopRight,
    BottomRight,
    TopCenter,
    /// Free-dragged screen coordinates (`custom_x`, `custom_y`).
    Custom,
}

impl OverlayAnchor {
    pub const ALL: [Self; 6] = [
        Self::TopLeft,
        Self::BottomLeft,
        Self::TopRight,
        Self::BottomRight,
        Self::TopCenter,
        Self::Custom,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::TopLeft => "Top-Left (Discord Style)",
            Self::BottomLeft => "Bottom-Left",
            Self::TopRight => "Top-Right",
            Self::BottomRight => "Bottom-Right",
            Self::TopCenter => "Top-Center",
            Self::Custom => "Custom (Dragged)",
        }
    }
}

/// Which missions to include in the overlay.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum MissionFilterSource {
    /// Show missions pinned with the compass in the Missions tab; if none are
    /// pinned, automatically fall back to active in-progress missions.
    #[default]
    TrackedOrActive,
    /// Strictly only missions pinned with the compass icon in the Missions tab.
    TrackedOnly,
    /// All active in-progress (and optionally claimable) missions.
    AllInProgress,
}

impl MissionFilterSource {
    pub const ALL: [Self; 3] = [
        Self::TrackedOrActive,
        Self::TrackedOnly,
        Self::AllInProgress,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::TrackedOrActive => "Tracked (Compass) or Active Fallback",
            Self::TrackedOnly => "Tracked (Compass) Only",
            Self::AllInProgress => "All In-Progress Missions",
        }
    }
}

/// Which Tinkerer quests to include in the overlay.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum QuestFilterSource {
    /// Show Daily Quest Chests pinned with the compass; if none are pinned,
    /// show all Daily Quest Chests.
    #[default]
    TrackedOrAllChests,
    /// Strictly only Daily Quest Chests pinned with the compass in the Quests tab.
    TrackedChestsOnly,
    /// All Daily Quest Chests (not hidden in the Quests tab).
    AllDailyChests,
    /// All active daily quests (Daily Quest Chests + Daily Miscellaneous + Event).
    AllDailyQuests,
}

impl QuestFilterSource {
    pub const ALL: [Self; 4] = [
        Self::TrackedOrAllChests,
        Self::TrackedChestsOnly,
        Self::AllDailyChests,
        Self::AllDailyQuests,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::TrackedOrAllChests => "Tracked Chests (or All Chests Fallback)",
            Self::TrackedChestsOnly => "Tracked Chests (Compass) Only",
            Self::AllDailyChests => "All Daily Quest Chests",
            Self::AllDailyQuests => "All Daily & Event Quests",
        }
    }
}

/// Which storage side's mark counts to display on quest rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum OverlayQuestSide {
    /// Follow the Quests tab mark progress setting / current character season.
    #[default]
    FollowApp,
    /// Show Seasonal mark counts only.
    Seasonal,
    /// Show Regular mark counts only.
    Regular,
    /// Show both `S:x/n` and `R:x/n` side by side.
    Both,
}

impl OverlayQuestSide {
    pub const ALL: [Self; 4] = [Self::FollowApp, Self::Seasonal, Self::Regular, Self::Both];

    pub fn label(self) -> &'static str {
        match self {
            Self::FollowApp => "Auto (Match Character / App)",
            Self::Seasonal => "Seasonal Only",
            Self::Regular => "Regular Only",
            Self::Both => "Both (Seasonal & Regular)",
        }
    }
}

/// Active tab in the in-game overlay.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum OverlayTab {
    /// Compact live boss DPS meter (ranking, total damage, DPM, top damage player).
    #[default]
    LiveDps,
    /// Missions & Daily Quests tracker.
    MissionsQuests,
    /// Persistent scratchpad notes tab.
    Notes,
}

impl OverlayTab {
    pub const ALL: [Self; 3] = [Self::LiveDps, Self::MissionsQuests, Self::Notes];

    pub fn short_label(self) -> &'static str {
        match self {
            Self::LiveDps => "DPS",
            Self::MissionsQuests => "Tasks",
            Self::Notes => "Notes",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::LiveDps => "Live DPS",
            Self::MissionsQuests => "Missions & Quests",
            Self::Notes => "Notes",
        }
    }
}

/// Persisted configuration for the RotMG Game Overlay.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct OverlayConfig {
    /// Master enable switch for the in-game overlay.
    pub enabled: bool,
    /// Currently selected overlay tab (`LiveDps` by default).
    pub active_tab: OverlayTab,
    /// Persistent scratchpad notes retained across bootups.
    pub notes_text: String,
    /// Whether the overlay is currently minimized (`-` button) to a compact header pill.
    pub minimized: bool,
    /// Automatic visibility trigger mode.
    pub trigger: VisibilityTrigger,
    /// Card vs ultra-compact pill display mode.
    pub display_mode: OverlayDisplayMode,
    /// Orientation for `UltraCompactPills` mode.
    pub pill_orientation: PillOrientation,

    // --- Area-based visibility filters ---
    /// Show overlay while in Nexus, Vault, Daily Quest Room, Pet Yard, Guild Hall.
    pub show_in_safe_areas: bool,
    /// Show overlay while in the Realm overworld.
    pub show_in_realm: bool,
    /// Show overlay while inside dungeons / boss encounters.
    pub show_in_dungeons: bool,
    /// Hide the overlay window completely when there are 0 missions and 0 quests to show.
    pub hide_when_empty: bool,

    // --- Positioning & Interaction ---
    /// Preset corner/edge anchor relative to the RotMG game canvas, or custom coordinates.
    pub anchor: OverlayAnchor,
    /// Horizontal offset (px) from the anchored corner/edge inside the game window.
    pub offset_x: f32,
    /// Vertical offset (px) from the anchored corner/edge inside the game window.
    pub offset_y: f32,
    /// Saved screen X when `anchor == OverlayAnchor::Custom`.
    pub custom_x: f32,
    /// Saved screen Y when `anchor == OverlayAnchor::Custom`.
    pub custom_y: f32,
    /// Pass mouse clicks through the body of the overlay to RotMG underneath.
    /// Note: The top header bar (`-` and `x` buttons) remains clickable even when
    /// body click-through is enabled. When false (default), you can also scroll
    /// the mission/quest list with the mouse wheel without stealing focus from RotMG.
    pub click_through: bool,

    // --- Visual & Translucent Styling ---
    /// Panel background opacity percentage (10..=98).
    pub bg_opacity_pct: u8,
    /// Foreground content (icons, bars, text) opacity percentage (35..=100).
    pub content_opacity_pct: u8,
    /// UI scale percentage (65..=150).
    pub scale_pct: u8,
    /// Target panel width in logical pixels for Compact Cards mode (200..=520).
    pub card_width: f32,
    /// Maximum height of the scrollable missions + quests area before scrolling kicks in (140..=900).
    pub max_height: f32,
    /// Show subtle glass border around the overlay panel.
    pub show_border: bool,
    /// Show compact header bar with current map/area badge and `-` / `x` buttons.
    pub show_header: bool,

    // --- Missions Section ---
    /// Show the Missions section in the overlay.
    pub show_missions: bool,
    /// Which missions to include.
    pub mission_source: MissionFilterSource,
    /// Include missions that are ready to claim (`MissionState::Claimable`).
    pub show_claimable_missions: bool,
    /// Only show missions that the current live character is eligible for
    /// (matches Seasonal/Crucible/worn-gear requirements).
    pub only_eligible_for_current_char: bool,
    /// Show the mission title text on each card.
    pub show_mission_names: bool,
    /// Show BXP and item reward icons on each mission card.
    pub show_mission_rewards: bool,
    /// Expand multi-choice objectives into all choice bars instead of collapsing to the best one.
    pub show_all_choice_options: bool,
    /// Maximum number of mission cards to include in the scrollable list (1..=60).
    pub max_missions: usize,

    // --- Daily Quests Section ---
    /// Show the Daily Quests section in the overlay.
    pub show_quests: bool,
    /// Which quests to include.
    pub quest_source: QuestFilterSource,
    /// Which storage side(s) to count marks on.
    pub quest_side: OverlayQuestSide,
    /// Highlight quests that have all required marks collected (`READY`).
    pub highlight_completable_quests: bool,
    /// Hide quests where you currently own 0 of the required marks.
    pub only_quests_with_progress: bool,
    /// Show the quest title next to the reward/mark icons.
    pub show_quest_names: bool,
    /// Show the quest reward icon (chest/item) at the start of the row.
    pub show_quest_rewards: bool,
    /// Maximum number of quest rows to include in the scrollable list (1..=30).
    pub max_quests: usize,
}

impl Default for OverlayConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            active_tab: OverlayTab::LiveDps,
            notes_text: String::new(),
            minimized: false,
            trigger: VisibilityTrigger::WhenRotmgFocused,
            display_mode: OverlayDisplayMode::CompactCards,
            pill_orientation: PillOrientation::Horizontal,

            show_in_safe_areas: true,
            show_in_realm: true,
            show_in_dungeons: true,
            hide_when_empty: false,

            anchor: OverlayAnchor::TopLeft,
            offset_x: 10.0,
            offset_y: 10.0,
            custom_x: 16.0,
            custom_y: 40.0,
            click_through: true,

            bg_opacity_pct: 55,
            content_opacity_pct: 92,
            scale_pct: 90,
            card_width: 265.0,
            max_height: 360.0,
            show_border: true,
            show_header: true,

            show_missions: true,
            mission_source: MissionFilterSource::TrackedOrActive,
            show_claimable_missions: true,
            only_eligible_for_current_char: false,
            show_mission_names: true,
            show_mission_rewards: true,
            show_all_choice_options: false,
            max_missions: 50,

            show_quests: true,
            quest_source: QuestFilterSource::TrackedOrAllChests,
            quest_side: OverlayQuestSide::FollowApp,
            highlight_completable_quests: true,
            only_quests_with_progress: false,
            show_quest_names: true,
            show_quest_rewards: true,
            max_quests: 20,
        }
    }
}

impl OverlayConfig {
    /// Path to `<data_local_dir>/RealmHound/overlay_config.json`.
    pub fn config_path() -> Option<PathBuf> {
        dirs::data_local_dir().map(|d| d.join("RealmHound").join("overlay_config.json"))
    }

    /// Load configuration from disk, falling back to [`Self::default`] if missing or invalid.
    pub fn load() -> Self {
        let Some(path) = Self::config_path() else {
            return Self::default();
        };
        let Ok(raw) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        match serde_json::from_str::<Self>(&raw) {
            Ok(mut cfg) => {
                // Migrate legacy 5-mission cap from initial build so all missions show
                if cfg.max_missions == 5 && cfg.max_quests == 4 {
                    cfg.max_missions = 50;
                    cfg.max_quests = 20;
                }
                cfg.clamp_values();
                cfg
            }
            Err(e) => {
                warn!("Failed to parse overlay_config.json ({e}), using defaults");
                Self::default()
            }
        }
    }

    /// Save configuration to disk.
    pub fn save(&self) {
        let Some(path) = Self::config_path() else {
            return;
        };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match serde_json::to_string_pretty(self) {
            Ok(json) => {
                if let Err(e) = std::fs::write(&path, json) {
                    warn!("Failed to write overlay_config.json: {e}");
                }
            }
            Err(e) => warn!("Failed to serialize overlay_config.json: {e}"),
        }
    }

    /// Ensure numeric fields remain within safe UI bounds.
    pub fn clamp_values(&mut self) {
        self.bg_opacity_pct = self.bg_opacity_pct.clamp(10, 98);
        self.content_opacity_pct = self.content_opacity_pct.clamp(35, 100);
        self.scale_pct = self.scale_pct.clamp(65, 150);
        self.card_width = self.card_width.clamp(200.0, 520.0);
        self.max_height = self.max_height.clamp(140.0, 900.0);
        self.offset_x = self.offset_x.clamp(0.0, 1000.0);
        self.offset_y = self.offset_y.clamp(0.0, 1000.0);
        self.max_missions = self.max_missions.clamp(1, 60);
        self.max_quests = self.max_quests.clamp(1, 30);
    }

    /// Normalized background alpha (`0.10..=0.98`).
    pub fn bg_alpha(&self) -> f32 {
        (self.bg_opacity_pct.clamp(10, 98) as f32) / 100.0
    }

    /// Normalized foreground/content opacity multiplier (`0.35..=1.0`).
    pub fn content_opacity(&self) -> f32 {
        (self.content_opacity_pct.clamp(35, 100) as f32) / 100.0
    }

    /// Normalized UI scale factor (`0.65..=1.50`).
    pub fn scale(&self) -> f32 {
        (self.scale_pct.clamp(65, 150) as f32) / 100.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_roundtrips_json() {
        let mut cfg = OverlayConfig::default();
        assert_eq!(cfg.active_tab, OverlayTab::LiveDps);
        assert_eq!(cfg.notes_text, "");
        cfg.notes_text = "Test note line 1\nLine 2".to_string();
        cfg.active_tab = OverlayTab::Notes;
        let json = serde_json::to_string(&cfg).expect("serialize default config");
        let parsed: OverlayConfig = serde_json::from_str(&json).expect("deserialize config");
        assert_eq!(cfg, parsed);
        assert_eq!(parsed.notes_text, "Test note line 1\nLine 2");
        assert_eq!(parsed.active_tab, OverlayTab::Notes);
    }

    #[test]
    fn partial_json_populates_defaults_and_clamps() {
        let raw = r#"{
            "enabled": true,
            "trigger": "when_rotmg_running",
            "bg_opacity_pct": 250,
            "scale_pct": 10,
            "max_missions": 0
        }"#;
        let mut parsed: OverlayConfig = serde_json::from_str(raw).expect("parse partial json");
        parsed.clamp_values();
        assert!(parsed.enabled);
        assert_eq!(parsed.active_tab, OverlayTab::LiveDps);
        assert_eq!(parsed.notes_text, "");
        assert_eq!(parsed.trigger, VisibilityTrigger::WhenRotmgRunning);
        assert_eq!(parsed.bg_opacity_pct, 98);
        assert_eq!(parsed.scale_pct, 65);
        assert_eq!(parsed.max_missions, 1);
        assert!(parsed.show_quests);
    }
}
