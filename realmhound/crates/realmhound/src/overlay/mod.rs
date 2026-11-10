//! Self-contained, modular RotMG In-Game Overlay package.
//!
//! Designed for zero-conflict upstream merges:
//! - Persists its own configuration to `<data_local_dir>/RealmHound/overlay_config.json`.
//! - Owns its runtime state in a module-private `OnceLock<Mutex<OverlayManager>>`.
//! - Exposes only 4 thin hook functions called from `app.rs`.

pub mod config;
pub mod renderer;
pub mod settings_ui;
pub mod window_tracker;

use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use eframe::egui;
use realmhound_core::combat::LiveBossDpsSnapshot;
use realmhound_core::settings::{QuestMarkProgressMode, TaskbarSettings};
use realmhound_core::vault::AccountData;

use self::config::{
    MissionFilterSource, OverlayConfig, OverlayDisplayMode, OverlayQuestSide, PillOrientation,
    QuestFilterSource,
};
use self::renderer::{
    project_overlay_data, render_overlay_viewport, OverlayProjectedData, OverlayRenderContext,
    OverlayRuntimeState,
};
use self::settings_ui::OverlaySettingsModalState;
use self::window_tracker::{should_show_overlay, GameWindowState};
use crate::panels::quest::QuestPanel;
use crate::processing::MissionView;
use crate::rendering::sprite_renderer::SpriteRenderer;
use crate::shadcn_ui::Shadcn;
use realmhound_core::GameEvent;

/// Subset of [`OverlayConfig`] that affects the projected mission and quest list.
/// Checked so changes to notes text, active tab, positioning, opacity, etc. do
/// not invalidate the mission/quest projection or clone heavy collections.
#[derive(Clone, PartialEq, Eq)]
struct TaskConfigKey {
    show_missions: bool,
    mission_source: MissionFilterSource,
    show_claimable_missions: bool,
    only_eligible_for_current_char: bool,
    show_mission_names: bool,
    show_mission_rewards: bool,
    show_all_choice_options: bool,
    max_missions: usize,
    show_quests: bool,
    quest_source: QuestFilterSource,
    quest_side: OverlayQuestSide,
    highlight_completable_quests: bool,
    only_quests_with_progress: bool,
    show_quest_names: bool,
    show_quest_rewards: bool,
    max_quests: usize,
    display_mode: OverlayDisplayMode,
    pill_orientation: PillOrientation,
}

impl TaskConfigKey {
    fn from_config(cfg: &OverlayConfig) -> Self {
        Self {
            show_missions: cfg.show_missions,
            mission_source: cfg.mission_source,
            show_claimable_missions: cfg.show_claimable_missions,
            only_eligible_for_current_char: cfg.only_eligible_for_current_char,
            show_mission_names: cfg.show_mission_names,
            show_mission_rewards: cfg.show_mission_rewards,
            show_all_choice_options: cfg.show_all_choice_options,
            max_missions: cfg.max_missions,
            show_quests: cfg.show_quests,
            quest_source: cfg.quest_source,
            quest_side: cfg.quest_side,
            highlight_completable_quests: cfg.highlight_completable_quests,
            only_quests_with_progress: cfg.only_quests_with_progress,
            show_quest_names: cfg.show_quest_names,
            show_quest_rewards: cfg.show_quest_rewards,
            max_quests: cfg.max_quests,
            display_mode: cfg.display_mode,
            pill_orientation: cfg.pill_orientation,
        }
    }
}

struct ProjectionCacheKey {
    mission_gen: u64,
    account_epoch: u64,
    quests_revision: u64,
    live_char_id: Option<i32>,
    is_seasonal_char: bool,
    quests_stale: bool,
    hidden_missions: HashSet<i64>,
    taskbar: TaskbarSettings,
    mark_mode: QuestMarkProgressMode,
    task_cfg: TaskConfigKey,
}

struct OverlayManager {
    config: OverlayConfig,
    win_state: GameWindowState,
    runtime: OverlayRuntimeState,
    modal: OverlaySettingsModalState,
    projected_cache: OverlayProjectedData,
    cache_key: Option<ProjectionCacheKey>,
    currently_shown: bool,
}

impl OverlayManager {
    fn new() -> Self {
        Self {
            config: OverlayConfig::load(),
            win_state: GameWindowState::default(),
            runtime: OverlayRuntimeState::default(),
            modal: OverlaySettingsModalState::new(),
            projected_cache: OverlayProjectedData::default(),
            cache_key: None,
            currently_shown: false,
        }
    }
}

fn manager_lock() -> &'static Mutex<OverlayManager> {
    static INSTANCE: OnceLock<Mutex<OverlayManager>> = OnceLock::new();
    INSTANCE.get_or_init(|| Mutex::new(OverlayManager::new()))
}

/// Forward a live [`GameEvent`] broadcast so the overlay knows the current map
/// (Safe Area / Realm / Dungeon) in real time.
pub fn notify_game_event(event: &GameEvent) {
    if let Ok(mut mgr) = manager_lock().lock() {
        mgr.win_state.on_game_event(event);
    }
}

/// True when the Overlay Settings modal is currently open (used by `app.rs` to
/// disable background panel clicks while a modal is active).
pub fn is_settings_open() -> bool {
    manager_lock()
        .lock()
        .map(|mgr| mgr.modal.open)
        .unwrap_or(false)
}

/// True when the overlay is enabled, in preview mode, or the settings modal is open.
/// Used by the processing worker to decide whether live DPS changes are worth
/// requesting an immediate UI repaint.
pub fn is_enabled() -> bool {
    manager_lock()
        .lock()
        .map(|mgr| {
            mgr.config.enabled
                || mgr.modal.open
                || mgr.runtime.preview_override
                || mgr.runtime.unlocked_override
        })
        .unwrap_or(false)
}

/// Flush any pending config or notes changes to disk immediately (called on app exit).
pub fn flush_pending_saves() {
    if let Ok(mut mgr) = manager_lock().lock() {
        if mgr.runtime.notes_dirty_at.take().is_some() {
            mgr.config.save();
        }
    }
}

/// Render the `🎯 Overlay` button in the top navigation bar.
pub fn render_top_bar_button(
    ui: &mut egui::Ui,
    shadcn: &Shadcn,
    icon_only: bool,
    text_only: bool,
    tab_tooltips: bool,
) {
    let Ok(mut mgr) = manager_lock().lock() else {
        return;
    };
    let OverlayManager {
        ref mut config,
        ref mut modal,
        ref mut runtime,
        currently_shown,
        ..
    } = *mgr;

    let changed = settings_ui::render_top_bar_button(
        ui,
        shadcn,
        icon_only,
        text_only,
        tab_tooltips,
        config,
        modal,
        runtime,
        currently_shown,
    );
    if changed {
        config.save();
    }
}

/// Poll RotMG window/process state, project active missions & daily quests when
/// needed, render the Overlay Settings modal (if open), and render the floating
/// translucent overlay viewport (if active).
#[allow(clippy::too_many_arguments)]
pub fn update_and_render(
    ctx: &egui::Context,
    shadcn: &Shadcn,
    sprite_renderer: &mut SpriteRenderer,
    mission_view: &MissionView,
    mission_gen: u64,
    hidden_missions: &HashSet<i64>,
    quest_panel: &mut QuestPanel,
    account_data: &AccountData,
    account_epoch: u64,
    live_char_id: Option<i32>,
    is_seasonal_char: bool,
    client_launch_unix: i64,
    session_connected: bool,
    map_pauses_boost_timers: bool,
    live_dps: Option<&LiveBossDpsSnapshot>,
) {
    let Ok(mut mgr_guard) = manager_lock().lock() else {
        return;
    };
    let mgr = &mut *mgr_guard;

    let any_active =
        mgr.config.enabled || mgr.modal.open || mgr.runtime.preview_override || mgr.runtime.unlocked_override;
    if !any_active {
        if mgr.currently_shown {
            window_tracker::notify_overlay_hidden();
            mgr.runtime.on_hidden();
            if mgr.runtime.notes_dirty_at.take().is_some() {
                mgr.config.save();
            }
        }
        mgr.currently_shown = false;
        return;
    }

    // 1. Poll RotMG process / window focus / area kind (throttled).
    mgr.win_state
        .poll_throttled(session_connected, map_pauses_boost_timers);

    // Keep repainting at a responsive cadence while the overlay is active so
    // top-bar hover detection, window tracking, and focus changes feel instant.
    if mgr.config.enabled || mgr.runtime.preview_override {
        ctx.request_repaint_after(Duration::from_millis(80));
    }

    // 2. Refresh projected mission & quest data only when inputs or settings change.
    let quests_revision = quest_panel.quests_revision();
    let quests_stale = quest_panel.daily_quests_stale(
        (client_launch_unix > 0).then_some(client_launch_unix),
        chrono::Utc::now(),
    );
    let (quests, hidden_quests, item_counts, mark_mode, taskbar_settings, _has_quest_data) =
        quest_panel.overlay_snapshot(account_data, account_epoch);

    let task_cfg = TaskConfigKey::from_config(&mgr.config);
    let needs_reproject = mgr.cache_key.as_ref().map_or(true, |k| {
        k.mission_gen != mission_gen
            || k.account_epoch != account_epoch
            || k.quests_revision != quests_revision
            || k.live_char_id != live_char_id
            || k.is_seasonal_char != is_seasonal_char
            || k.quests_stale != quests_stale
            || k.mark_mode != mark_mode
            || &k.hidden_missions != hidden_missions
            || &k.taskbar != taskbar_settings
            || k.task_cfg != task_cfg
    });

    if needs_reproject {
        let render_ctx = OverlayRenderContext {
            mission_view,
            hidden_missions,
            quests,
            hidden_quests,
            item_counts,
            quest_mark_mode: mark_mode,
            taskbar_settings,
            quests_stale,
            account_data,
            live_char_id,
            is_seasonal_char,
        };
        let mut projected = project_overlay_data(&mgr.config, &render_ctx);
        // Preserve current live DPS snapshot across mission/quest reproject
        projected.live_dps = mgr.projected_cache.live_dps.take();
        mgr.projected_cache = projected;
        mgr.cache_key = Some(ProjectionCacheKey {
            mission_gen,
            account_epoch,
            quests_revision,
            live_char_id,
            is_seasonal_char,
            quests_stale,
            hidden_missions: hidden_missions.clone(),
            taskbar: taskbar_settings.clone(),
            mark_mode,
            task_cfg,
        });
    }

    // Always update live DPS snapshot for the current frame
    mgr.projected_cache.live_dps = live_dps.cloned();

    let has_items = !mgr.projected_cache.is_empty(&mgr.config);
    let show_viewport = should_show_overlay(
        &mgr.config,
        &mgr.win_state,
        mgr.runtime.preview_override,
        mgr.runtime.unlocked_override,
        has_items,
    );
    if mgr.currently_shown && !show_viewport {
        window_tracker::notify_overlay_hidden();
        mgr.runtime.on_hidden();
        if mgr.runtime.notes_dirty_at.take().is_some() {
            mgr.config.save();
        }
    }
    mgr.currently_shown = show_viewport;

    let mut cfg_changed = false;

    // 3. Render Settings Modal (if open)
    if mgr.modal.open {
        if settings_ui::render_settings_modal(
            ctx,
            shadcn,
            sprite_renderer,
            &mut mgr.config,
            &mgr.win_state,
            &mgr.projected_cache,
            &mut mgr.runtime,
            &mut mgr.modal,
            mgr.currently_shown,
        ) {
            cfg_changed = true;
            // Force re-projection next check if config changed
            mgr.cache_key = None;
        }
    }

    // 4. Render Floating Child Viewport (if active)
    if show_viewport {
        if render_overlay_viewport(
            ctx,
            sprite_renderer,
            &mut mgr.config,
            &mgr.win_state,
            &mgr.projected_cache,
            &mut mgr.runtime,
        ) {
            cfg_changed = true;
        }
    }

    // 5. Debounced auto-save for notes text
    if let Some(dirty_at) = mgr.runtime.notes_dirty_at {
        if dirty_at.elapsed() >= Duration::from_millis(600) {
            mgr.runtime.notes_dirty_at = None;
            cfg_changed = true;
        }
    }

    if cfg_changed {
        mgr.config.save();
    }
}
