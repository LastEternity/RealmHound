//! Transparent multi-viewport overlay renderer and mission/quest data projector.
//!
//! Features:
//! - **Discord-style top-left game canvas attachment**: Automatically tracks the
//!   RotMG client area in windowed, maximized, and borderless-fullscreen modes.
//! - **Always-active `—` (Minimize) and `✕` (Close) buttons**: Collapse the
//!   overlay to a sleek single-line corner badge (`+` to expand) or close it at
//!   any time without stealing focus from RotMG.
//! - **Compact, scrollable Mission & Daily Quest cards**: Displays all active
//!   missions and daily quests in a compact layout inside a smooth vertical
//!   `ScrollArea`.

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, FontId, Layout, Margin, Pos2, Rect, RichText,
    Sense, Stroke, StrokeKind, Vec2,
};
use realmhound_core::api::MissionState;
use realmhound_core::assets::get_asset_manager;
use realmhound_core::combat::{LiveBossDpsSnapshot, LiveDpsParticipant};
use realmhound_core::protocol::data::QuestData;
use realmhound_core::settings::{QuestMarkProgressMode, TaskbarSettings};
use realmhound_core::vault::AccountData;

use super::config::{
    MissionFilterSource, OverlayAnchor, OverlayConfig, OverlayDisplayMode, OverlayQuestSide,
    OverlayTab, PillOrientation, QuestFilterSource,
};
use super::window_tracker::{
    compute_overlay_position, primary_mouse_button_down, query_screen_cursor_pos,
    set_overlay_allow_activate, sync_platform_overlay_window, take_overlay_wheel_scroll_px,
    GameWindowState,
};
use crate::panels::missions::{
    completion_fraction, current_char_from, mission_eligible, mission_fallback_icon, obj_icon,
    on_cooldown, paint_obj_icon, CurrentChar, ObjIcon,
};
use crate::panels::quest::{is_daily_chest_quest, quest_section_info};
use crate::panels::taskbar::{
    build_quest_task, build_taskbar_items, ChainGroup, CombinedTask, MissionTask, QuestTask,
    TaskbarItem, TaskbarTask,
};
use crate::processing::{
    MissionCategory, MissionEntryView, MissionView, ObjectiveKind, RewardView,
};
use crate::rendering::sprite_renderer::SpriteRenderer;

/// One objective progress bar inside an [`OverlayMissionCard`].
#[derive(Clone)]
pub struct OverlayObjectiveBar {
    pub icon: ObjIcon,
    pub label: String,
    pub have: i32,
    pub need: i32,
    pub is_choice: bool,
}

/// One reward item chip inside an [`OverlayMissionCard`].
#[derive(Clone)]
pub struct OverlayRewardChip {
    pub object_id: i32,
    pub amount: i64,
}

/// Pre-projected mission card ready to draw on the overlay.
#[derive(Clone)]
pub struct OverlayMissionCard {
    pub uid: i64,
    pub name: String,
    pub desc: String,
    pub category: MissionCategory,
    pub state: MissionState,
    pub eligible: bool,
    pub fraction: f32,
    pub objectives: Vec<OverlayObjectiveBar>,
    pub bxp: i64,
    pub item_rewards: Vec<OverlayRewardChip>,
}

/// One distinct required mark and its owned counts inside an [`OverlayQuestRow`].
#[derive(Clone)]
pub struct OverlayQuestMark {
    pub item_id: i32,
    pub need: u32,
    pub seasonal_have: u32,
    pub regular_have: u32,
}

/// Pre-projected quest row ready to draw on the overlay.
#[derive(Clone)]
pub struct OverlayQuestRow {
    pub name: String,
    pub reward_item_id: Option<i32>,
    pub marks: Vec<OverlayQuestMark>,
    pub seasonal_completable: bool,
    pub regular_completable: bool,
    pub total_have_primary: u32,
    pub total_need: u32,
}

/// Complete projected snapshot for the overlay in a given frame.
#[derive(Clone, Default)]
pub struct OverlayProjectedData {
    pub missions: Vec<OverlayMissionCard>,
    pub quests: Vec<OverlayQuestRow>,
    pub pills: Vec<TaskbarItem>,
    pub primary_seasonal: bool,
    pub show_both_quest_sides: bool,
    pub quests_stale: bool,
    pub defs_loaded: bool,
    /// Latest live boss DPS snapshot (refreshed independently of the
    /// mission/quest projection whenever the worker publishes a new one).
    pub live_dps: Option<LiveBossDpsSnapshot>,
}

impl OverlayProjectedData {
    /// Whether the overlay has nothing to show for the *active tab* (drives
    /// the `hide_when_empty` setting).
    pub fn is_empty(&self, cfg: &OverlayConfig) -> bool {
        match cfg.active_tab {
            OverlayTab::LiveDps => self.live_dps.is_none(),
            OverlayTab::Notes => false,
            OverlayTab::MissionsQuests => self.tasks_empty(cfg),
        }
    }

    /// Whether the Missions & Quests tab has nothing to show.
    pub fn tasks_empty(&self, cfg: &OverlayConfig) -> bool {
        match cfg.display_mode {
            OverlayDisplayMode::CompactCards => {
                let m_empty = !cfg.show_missions || self.missions.is_empty();
                let q_empty = !cfg.show_quests || self.quests.is_empty();
                m_empty && q_empty
            }
            OverlayDisplayMode::UltraCompactPills => self.pills.is_empty(),
        }
    }
}

/// Inputs needed from `RealmHoundApp` to project and render the overlay.
pub struct OverlayRenderContext<'a> {
    pub mission_view: &'a MissionView,
    pub hidden_missions: &'a HashSet<i64>,
    pub quests: &'a [QuestData],
    pub hidden_quests: &'a HashSet<String>,
    pub item_counts: Option<&'a HashMap<i32, (u32, u32)>>,
    pub quest_mark_mode: QuestMarkProgressMode,
    pub taskbar_settings: &'a TaskbarSettings,
    pub quests_stale: bool,
    pub account_data: &'a AccountData,
    pub live_char_id: Option<i32>,
    pub is_seasonal_char: bool,
}

/// Project missions, quests, and pills according to `cfg`.
pub fn project_overlay_data(
    cfg: &OverlayConfig,
    input: &OverlayRenderContext<'_>,
) -> OverlayProjectedData {
    let current_char: Option<CurrentChar> =
        current_char_from(input.account_data, input.live_char_id);

    let (primary_seasonal, show_both_quest_sides) = match cfg.quest_side {
        OverlayQuestSide::FollowApp => match input.quest_mark_mode {
            QuestMarkProgressMode::Seasonal => (true, false),
            QuestMarkProgressMode::Regular => (false, false),
            QuestMarkProgressMode::Both => (input.is_seasonal_char, true),
        },
        OverlayQuestSide::Seasonal => (true, false),
        OverlayQuestSide::Regular => (false, false),
        OverlayQuestSide::Both => (input.is_seasonal_char, true),
    };

    let missions = if cfg.show_missions {
        project_missions(cfg, input, current_char)
    } else {
        Vec::new()
    };

    let empty_counts = HashMap::new();
    let counts = input.item_counts.unwrap_or(&empty_counts);
    let quests = if cfg.show_quests {
        project_quests(cfg, input, counts, primary_seasonal, show_both_quest_sides)
    } else {
        Vec::new()
    };

    let pills = project_pills(cfg, input, counts, current_char, primary_seasonal);

    OverlayProjectedData {
        missions,
        quests,
        pills,
        primary_seasonal,
        show_both_quest_sides,
        quests_stale: input.quests_stale,
        defs_loaded: input.mission_view.defs_loaded,
        // Filled in by the overlay manager, which tracks it separately.
        live_dps: None,
    }
}

fn project_missions(
    cfg: &OverlayConfig,
    input: &OverlayRenderContext<'_>,
    current_char: Option<CurrentChar>,
) -> Vec<OverlayMissionCard> {
    let entries = &input.mission_view.entries;
    let any_tracked = entries.iter().any(|e| {
        let uid = e.uid();
        input.taskbar_settings.mission_tracked(uid) && !input.hidden_missions.contains(&uid)
    });

    let use_tracked_filter = match cfg.mission_source {
        MissionFilterSource::TrackedOnly => true,
        MissionFilterSource::TrackedOrActive => any_tracked,
        MissionFilterSource::AllInProgress => false,
    };

    let mut cards: Vec<OverlayMissionCard> = Vec::new();
    for e in entries {
        let uid = e.uid();
        if input.hidden_missions.contains(&uid) {
            continue;
        }
        if use_tracked_filter && !input.taskbar_settings.mission_tracked(uid) {
            continue;
        }
        if on_cooldown(e) {
            continue;
        }
        match e.state {
            MissionState::InProgress => {}
            MissionState::Claimable => {
                if !cfg.show_claimable_missions {
                    continue;
                }
            }
            MissionState::Locked | MissionState::Claimed | MissionState::Unknown(_) => continue,
        }

        let eligible = mission_eligible(current_char, e.participants, &e.worn_restriction);
        if cfg.only_eligible_for_current_char && current_char.is_some() && !eligible {
            continue;
        }

        let fallback = mission_fallback_icon(e);
        let obj_indices = select_objective_indices(e, cfg.show_all_choice_options);
        let objectives: Vec<OverlayObjectiveBar> = obj_indices
            .into_iter()
            .filter_map(|idx| {
                let o = e.objectives.get(idx)?;
                if o.need <= 0 {
                    return None;
                }
                Some(OverlayObjectiveBar {
                    icon: obj_icon(o, &fallback),
                    label: short_objective_label(o, &e.desc),
                    have: o.have.clamp(0, o.need),
                    need: o.need,
                    is_choice: e.one_of && e.objectives.len() > 1,
                })
            })
            .collect();

        if objectives.is_empty() && e.state != MissionState::Claimable {
            continue;
        }

        let mut bxp = 0i64;
        let mut item_rewards = Vec::new();
        for r in &e.rewards_detail {
            match r {
                RewardView::Bxp(amount) => bxp += *amount,
                RewardView::Item {
                    object_id, amount, ..
                } if *object_id > 0 => {
                    item_rewards.push(OverlayRewardChip {
                        object_id: *object_id,
                        amount: *amount,
                    });
                }
                _ => {}
            }
        }

        cards.push(OverlayMissionCard {
            uid,
            name: e.name.clone(),
            desc: e.desc.clone(),
            category: e.category,
            state: e.state,
            eligible,
            fraction: completion_fraction(e),
            objectives,
            bxp,
            item_rewards,
        });
    }

    cards.sort_by(|a, b| {
        let a_claim = a.state == MissionState::Claimable;
        let b_claim = b.state == MissionState::Claimable;
        b_claim
            .cmp(&a_claim)
            .then_with(|| b.eligible.cmp(&a.eligible))
            .then_with(|| b.fraction.total_cmp(&a.fraction))
            .then_with(|| a.category.order().cmp(&b.category.order()))
            .then_with(|| a.uid.cmp(&b.uid))
    });

    cards.truncate(cfg.max_missions.clamp(1, 60));
    cards
}

fn select_objective_indices(e: &MissionEntryView, show_all_choice: bool) -> Vec<usize> {
    if e.objectives.is_empty() {
        return Vec::new();
    }
    if !e.one_of || e.objectives.len() <= 1 || show_all_choice {
        return (0..e.objectives.len()).collect();
    }
    let best = e
        .objectives
        .iter()
        .enumerate()
        .filter(|(_, o)| o.need > 0)
        .max_by(|(_, a), (_, b)| {
            let fa = a.have as f32 / a.need as f32;
            let fb = b.have as f32 / b.need as f32;
            fa.total_cmp(&fb)
        })
        .map(|(i, _)| i)
        .unwrap_or(0);
    vec![best]
}

fn short_objective_label(o: &crate::processing::ObjectiveView, mission_desc: &str) -> String {
    if let Some(d) = &o.dungeon_name {
        if !d.is_empty() {
            return d.clone();
        }
    }
    if matches!(o.kind, ObjectiveKind::Difficulty) && !o.target.trim().is_empty() {
        return format!("Diff {}", o.target.trim());
    }
    if !o.label.trim().is_empty() {
        return o.label.trim().to_string();
    }
    mission_desc.trim().to_string()
}

fn project_quests(
    cfg: &OverlayConfig,
    input: &OverlayRenderContext<'_>,
    counts: &HashMap<i32, (u32, u32)>,
    primary_seasonal: bool,
    show_both_sides: bool,
) -> Vec<OverlayQuestRow> {
    let any_tracked_chest = input.quests.iter().any(|q| {
        is_daily_chest_quest(q)
            && !input.hidden_quests.contains(&q.id)
            && input.taskbar_settings.quest_tracked(&q.id)
    });

    let mut rows: Vec<OverlayQuestRow> = Vec::new();
    for q in input.quests {
        if input.hidden_quests.contains(&q.id) {
            continue;
        }
        let is_chest = is_daily_chest_quest(q);
        let (_section_title, is_daily) = quest_section_info(q);

        let keep = match cfg.quest_source {
            QuestFilterSource::TrackedChestsOnly => {
                is_chest && input.taskbar_settings.quest_tracked(&q.id)
            }
            QuestFilterSource::TrackedOrAllChests => {
                if any_tracked_chest {
                    is_chest && input.taskbar_settings.quest_tracked(&q.id)
                } else {
                    is_chest
                }
            }
            QuestFilterSource::AllDailyChests => is_chest,
            QuestFilterSource::AllDailyQuests => is_daily || is_chest,
        };
        if !keep {
            continue;
        }

        let mut mark_order: Vec<(i32, u32)> = Vec::new();
        for &req_id in &q.requirements {
            if let Some(entry) = mark_order.iter_mut().find(|(id, _)| *id == req_id) {
                entry.1 += 1;
            } else {
                mark_order.push((req_id, 1));
            }
        }
        if mark_order.is_empty() {
            continue;
        }

        let mut marks = Vec::with_capacity(mark_order.len());
        let mut seasonal_completable = true;
        let mut regular_completable = true;
        let mut total_have_s = 0u32;
        let mut total_have_r = 0u32;
        let mut total_need = 0u32;

        for (item_id, need) in mark_order {
            let (s_owned, r_owned) = counts.get(&item_id).copied().unwrap_or((0, 0));
            if s_owned < need {
                seasonal_completable = false;
            }
            if r_owned < need {
                regular_completable = false;
            }
            total_have_s += s_owned.min(need);
            total_have_r += r_owned.min(need);
            total_need += need;
            marks.push(OverlayQuestMark {
                item_id,
                need,
                seasonal_have: s_owned,
                regular_have: r_owned,
            });
        }

        let total_have_primary = if show_both_sides {
            total_have_s.max(total_have_r)
        } else if primary_seasonal {
            total_have_s
        } else {
            total_have_r
        };

        if cfg.only_quests_with_progress && total_have_primary == 0 {
            continue;
        }

        let reward_item_id = q.rewards.first().copied();

        rows.push(OverlayQuestRow {
            name: q.name.clone(),
            reward_item_id,
            marks,
            seasonal_completable,
            regular_completable,
            total_have_primary,
            total_need: total_need.max(1),
        });
    }

    rows.sort_by(|a, b| {
        let a_ready = if show_both_sides {
            a.seasonal_completable || a.regular_completable
        } else if primary_seasonal {
            a.seasonal_completable
        } else {
            a.regular_completable
        };
        let b_ready = if show_both_sides {
            b.seasonal_completable || b.regular_completable
        } else if primary_seasonal {
            b.seasonal_completable
        } else {
            b.regular_completable
        };
        let fa = a.total_have_primary as f32 / a.total_need as f32;
        let fb = b.total_have_primary as f32 / b.total_need as f32;
        b_ready.cmp(&a_ready).then_with(|| fb.total_cmp(&fa))
    });

    rows.truncate(cfg.max_quests.clamp(1, 30));
    rows
}

fn project_pills(
    cfg: &OverlayConfig,
    input: &OverlayRenderContext<'_>,
    counts: &HashMap<i32, (u32, u32)>,
    current_char: Option<CurrentChar>,
    primary_seasonal: bool,
) -> Vec<TaskbarItem> {
    let any_tracked_mission = input.mission_view.entries.iter().any(|e| {
        let uid = e.uid();
        input.taskbar_settings.mission_tracked(uid) && !input.hidden_missions.contains(&uid)
    });
    let use_tracked_missions = match cfg.mission_source {
        MissionFilterSource::TrackedOnly => true,
        MissionFilterSource::TrackedOrActive => any_tracked_mission,
        MissionFilterSource::AllInProgress => false,
    };

    let any_tracked_chest = input.quests.iter().any(|q| {
        is_daily_chest_quest(q)
            && !input.hidden_quests.contains(&q.id)
            && input.taskbar_settings.quest_tracked(&q.id)
    });

    let quest_tasks: Vec<QuestTask> = if cfg.show_quests {
        input
            .quests
            .iter()
            .filter(|q| {
                if input.hidden_quests.contains(&q.id) {
                    return false;
                }
                let is_chest = is_daily_chest_quest(q);
                match cfg.quest_source {
                    QuestFilterSource::TrackedChestsOnly => {
                        is_chest && input.taskbar_settings.quest_tracked(&q.id)
                    }
                    QuestFilterSource::TrackedOrAllChests => {
                        if any_tracked_chest {
                            is_chest && input.taskbar_settings.quest_tracked(&q.id)
                        } else {
                            is_chest
                        }
                    }
                    QuestFilterSource::AllDailyChests => is_chest,
                    QuestFilterSource::AllDailyQuests => is_chest || quest_section_info(q).1,
                }
            })
            .filter_map(|q| {
                build_quest_task(
                    q,
                    |id| counts.get(&id).copied().unwrap_or((0, 0)),
                    primary_seasonal,
                    input.quest_mark_mode,
                )
            })
            .collect()
    } else {
        Vec::new()
    };

    build_taskbar_items(
        input.mission_view,
        |uid| {
            if !cfg.show_missions || input.hidden_missions.contains(&uid) {
                return false;
            }
            if use_tracked_missions {
                input.taskbar_settings.mission_tracked(uid)
            } else {
                true
            }
        },
        quest_tasks,
        current_char,
        cfg.show_all_choice_options,
    )
}

/// Format BXP numbers compactly (`6000000` -> `"6M"`, `4500000` -> `"4.5M"`, `25000` -> `"25K"`).
fn format_bxp_compact(bxp: i64) -> String {
    if bxp >= 1_000_000 {
        let m = bxp as f64 / 1_000_000.0;
        if (m - m.round()).abs() < 0.05 {
            format!("{}M BXP", m.round() as i64)
        } else {
            format!("{m:.1}M BXP")
        }
    } else if bxp >= 1_000 {
        let k = bxp as f64 / 1_000.0;
        if (k - k.round()).abs() < 0.05 {
            format!("{}K BXP", k.round() as i64)
        } else {
            format!("{k:.1}K BXP")
        }
    } else {
        format!("{bxp} BXP")
    }
}

/// An in-progress header drag driven by the global screen cursor.
///
/// Used on platforms that expose a global cursor (Windows) so dragging keeps
/// working while RotMG is the focused, fullscreen foreground window and the
/// overlay itself is never activated.
#[derive(Clone, Copy)]
struct ManualDrag {
    /// Global cursor position when the drag started (physical screen pixels).
    start_cursor: Pos2,
    /// Overlay outer position when the drag started (egui points).
    start_pos: Pos2,
    /// True once the cursor has moved past the deadzone so a plain click does
    /// not switch `cfg.anchor` to `Custom`.
    moved: bool,
}

/// Persistent runtime state for the overlay viewport (last measured content size, etc.).
pub struct OverlayRuntimeState {
    pub last_content_size: Vec2,
    /// Last position explicitly sent to the overlay window (egui points).
    pub last_applied_pos: Option<Pos2>,
    /// Position the overlay viewport was spawned at. Kept constant while the
    /// viewport is alive so egui's builder diffing never re-sends a stale
    /// `OuterPosition` that fights an in-progress drag.
    spawn_pos: Option<Pos2>,
    pub unlocked_override: bool,
    pub preview_override: bool,
    pub passthrough_active: bool,
    pub scroll_delta_y: f32,
    /// Whether the small in-overlay quick settings bar (⚙) is expanded.
    pub quick_settings_open: bool,
    /// Height (points, from the top of the overlay) of the always-interactive
    /// header zone: drag banner + header + quick settings bar.
    interactive_top_pts: f32,
    /// Overlay viewport pixels-per-point from the previous frame.
    last_ppp: f32,
    /// True if a widget inside the overlay (e.g. the opacity slider) was being
    /// dragged last frame, so click-through must stay disabled.
    widget_dragging: bool,
    drag: Option<ManualDrag>,
    /// Fallback OS-driven drag (`ViewportCommand::StartDrag`) is in progress.
    os_drag_active: bool,
    /// When the notes text was last edited and not yet persisted. The overlay
    /// manager saves once typing has been idle for a moment, so typing never
    /// writes to disk on every keystroke.
    pub notes_dirty_at: Option<Instant>,
    /// Whether the notes text box had keyboard focus last frame.
    pub notes_focused: bool,
}

impl Default for OverlayRuntimeState {
    fn default() -> Self {
        Self {
            last_content_size: egui::vec2(265.0, 220.0),
            last_applied_pos: None,
            spawn_pos: None,
            unlocked_override: false,
            preview_override: false,
            passthrough_active: false,
            scroll_delta_y: 0.0,
            quick_settings_open: false,
            interactive_top_pts: 30.0,
            last_ppp: 1.0,
            widget_dragging: false,
            drag: None,
            os_drag_active: false,
            notes_dirty_at: None,
            notes_focused: false,
        }
    }
}

impl OverlayRuntimeState {
    /// Reset per-viewport state once the overlay window has been closed so it
    /// is re-spawned at the correct position next time it appears.
    pub fn on_hidden(&mut self) {
        self.spawn_pos = None;
        self.last_applied_pos = None;
        self.drag = None;
        self.os_drag_active = false;
        self.widget_dragging = false;
        self.scroll_delta_y = 0.0;
        self.notes_focused = false;
        set_overlay_allow_activate(false);
    }

    fn is_dragging(&self) -> bool {
        self.drag.is_some() || self.os_drag_active
    }
}

/// Start dragging the overlay window from a header/banner drag handle.
fn begin_overlay_drag(ctx: &egui::Context, cfg: &mut OverlayConfig, rt: &mut OverlayRuntimeState) {
    if rt.is_dragging() {
        return;
    }
    let current = ctx
        .input(|i| i.viewport().outer_rect.map(|r| r.min))
        .or(rt.last_applied_pos)
        .unwrap_or(egui::pos2(cfg.custom_x, cfg.custom_y));

    if let Some(cursor) = query_screen_cursor_pos() {
        rt.drag = Some(ManualDrag {
            start_cursor: cursor,
            start_pos: current,
            moved: false,
        });
    } else {
        cfg.anchor = OverlayAnchor::Custom;
        cfg.custom_x = current.x;
        cfg.custom_y = current.y;
        rt.last_applied_pos = Some(current);
        rt.os_drag_active = true;
        ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
    }
}

/// Advance an in-progress drag. Returns `true` when a drag finished this frame
/// (so the final position should be persisted).
fn update_overlay_drag(
    vctx: &egui::Context,
    cfg: &mut OverlayConfig,
    rt: &mut OverlayRuntimeState,
) -> bool {
    if let Some(mut drag) = rt.drag {
        let still_down = primary_mouse_button_down()
            .unwrap_or_else(|| vctx.input(|i| i.pointer.primary_down()));
        match (still_down, query_screen_cursor_pos()) {
            (true, Some(cursor)) => {
                let delta_px = cursor - drag.start_cursor;
                if !drag.moved && delta_px.length_sq() > 4.0 {
                    drag.moved = true;
                    cfg.anchor = OverlayAnchor::Custom;
                }
                if drag.moved {
                    let ppp = vctx.pixels_per_point().max(0.25);
                    let new_pos = drag.start_pos + delta_px / ppp;
                    cfg.custom_x = new_pos.x.round();
                    cfg.custom_y = new_pos.y.round();
                    vctx.set_cursor_icon(egui::CursorIcon::Grabbing);
                }
                rt.drag = Some(drag);
                false
            }
            _ => {
                let was_moved = drag.moved;
                rt.drag = None;
                was_moved
            }
        }
    } else if rt.os_drag_active {
        // The OS is moving the window: mirror its position instead of fighting it.
        if let Some(outer) = vctx.input(|i| i.viewport().outer_rect) {
            cfg.custom_x = outer.min.x;
            cfg.custom_y = outer.min.y;
            rt.last_applied_pos = Some(outer.min);
        }
        if !vctx.input(|i| i.pointer.any_down()) {
            rt.os_drag_active = false;
            return true;
        }
        false
    } else {
        false
    }
}

/// Render the floating child viewport when visible. Returns `true` if `cfg` was
/// mutated (e.g. minimized, closed, or dragged).
pub fn render_overlay_viewport(
    root_ctx: &egui::Context,
    sprite_renderer: &mut SpriteRenderer,
    cfg: &mut OverlayConfig,
    win_state: &GameWindowState,
    projected: &OverlayProjectedData,
    rt: &mut OverlayRuntimeState,
) -> bool {
    // The Notes tab needs real clicks and keyboard focus to type into its text
    // box, so its body is never click-through and the window may be activated.
    let notes_active = cfg.active_tab == OverlayTab::Notes && !cfg.minimized;
    set_overlay_allow_activate(notes_active);
    let body_click_through =
        cfg.click_through && !rt.unlocked_override && !cfg.minimized && !notes_active;
    let is_dragging = rt.is_dragging() || rt.widget_dragging;
    let interactive_top_px = rt.interactive_top_pts * rt.last_ppp;
    let (effective_passthrough, cursor_inside) = sync_platform_overlay_window(
        body_click_through,
        interactive_top_px,
        cfg.bg_opacity_pct,
        cfg.minimized,
        is_dragging,
    );
    rt.passthrough_active = effective_passthrough;

    // Mouse-wheel deltas swallowed by the platform hook while hovering the overlay.
    rt.scroll_delta_y += take_overlay_wheel_scroll_px();

    let fallback_bounds = root_ctx
        .input(|i| i.viewport().monitor_size)
        .map(|sz| Rect::from_min_size(Pos2::ZERO, sz))
        .unwrap_or_else(|| Rect::from_min_size(Pos2::ZERO, egui::vec2(1920.0, 1080.0)));
    // Native window probes report physical pixels; egui positions are in points.
    let root_ppp = root_ctx.pixels_per_point().max(0.25);
    let target_bounds = win_state
        .game_window_rect
        .map(|r| Rect::from_min_max((r.min.to_vec2() / root_ppp).to_pos2(), (r.max.to_vec2() / root_ppp).to_pos2()))
        .unwrap_or(fallback_bounds);

    let desired_size = egui::vec2(
        rt.last_content_size.x.clamp(140.0, 680.0),
        rt.last_content_size.y.clamp(24.0, 920.0),
    );
    let spawn_pos = *rt
        .spawn_pos
        .get_or_insert_with(|| compute_overlay_position(cfg, target_bounds, desired_size));

    let viewport_id = egui::ViewportId::from_hash_of("realmhound_rotmg_overlay");
    let builder = egui::ViewportBuilder::default()
        .with_title("RealmHound Overlay")
        .with_app_id("realmhound_overlay")
        .with_decorations(false)
        .with_transparent(true)
        .with_always_on_top()
        .with_active(false)
        .with_taskbar(false)
        .with_has_shadow(false)
        .with_resizable(false)
        .with_inner_size(desired_size)
        .with_position(spawn_pos);

    let mut cfg_changed = false;

    root_ctx.show_viewport_immediate(viewport_id, builder, |vctx, _class| {
        // Only touch the shared style when it actually differs: replacing the
        // style `Arc` every frame forces needless work (and galley re-layout).
        let mut visuals = egui::Visuals::dark();
        visuals.panel_fill = Color32::TRANSPARENT;
        visuals.window_fill = Color32::TRANSPARENT;
        visuals.extreme_bg_color = Color32::from_rgba_unmultiplied(8, 10, 16, 170);
        // Slim scrollbars so scrolling takes minimal horizontal space
        visuals.widgets.noninteractive.bg_fill = Color32::from_rgba_unmultiplied(40, 50, 65, 110);
        let style_ok = {
            let style = vctx.style();
            style.visuals == visuals
                && style.spacing.scroll.bar_width == 5.0
                && style.spacing.scroll.bar_inner_margin == 2.0
                && style.spacing.scroll.bar_outer_margin == 1.0
                && !style.interaction.selectable_labels
        };
        if !style_ok {
            vctx.style_mut(|style| {
                style.visuals = visuals;
                style.spacing.scroll.bar_width = 5.0;
                style.spacing.scroll.bar_inner_margin = 2.0;
                style.spacing.scroll.bar_outer_margin = 1.0;
                // Selectable labels would swallow drags meant for the header handle.
                style.interaction.selectable_labels = false;
            });
        }

        // Drain any wheel delta delivered to the viewport so hovering the
        // header, quick-settings bar, or body scrolls the list cleanly.
        let vp_wheel_y = vctx.input_mut(|i| std::mem::take(&mut i.smooth_scroll_delta.y));
        if vp_wheel_y.abs() > 0.1 {
            rt.scroll_delta_y += vp_wheel_y;
        }

        if update_overlay_drag(vctx, cfg, rt) {
            cfg_changed = true;
        }

        // Keep the window where it belongs: attached to the game window for
        // preset anchors, or at the free-dragged custom coordinates.
        if !rt.os_drag_active {
            let target_pos = compute_overlay_position(cfg, target_bounds, desired_size);
            if rt
                .last_applied_pos
                .is_none_or(|last| (last - target_pos).length_sq() > 0.25)
            {
                vctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(target_pos));
                rt.last_applied_pos = Some(target_pos);
            }
        }

        let measured = egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(Color32::TRANSPARENT))
            .show(vctx, |ui| {
                render_overlay_panel_contents(
                    ui,
                    sprite_renderer,
                    cfg,
                    win_state,
                    projected,
                    rt,
                    &mut cfg_changed,
                    false,
                )
            })
            .inner;

        rt.last_ppp = vctx.pixels_per_point();
        rt.widget_dragging = vctx.dragged_id().is_some();

        if (measured.x - rt.last_content_size.x).abs() > 1.5
            || (measured.y - rt.last_content_size.y).abs() > 1.5
        {
            rt.last_content_size = measured;
            vctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(measured));
        }
    });

    if rt.is_dragging() || rt.widget_dragging {
        root_ctx.request_repaint();
    } else if cursor_inside {
        // Snappy hover feedback while the cursor is over the overlay.
        root_ctx.request_repaint_after(std::time::Duration::from_millis(30));
    }

    cfg_changed
}

/// Render the translucent overlay contents into `ui` and return the bounding size.
/// Used both by the floating child viewport (`is_inline_preview == false`) and
/// by the inline preview in Settings (`is_inline_preview == true`).
#[allow(clippy::too_many_arguments)]
pub fn render_overlay_panel_contents(
    ui: &mut egui::Ui,
    sprite_renderer: &mut SpriteRenderer,
    cfg: &mut OverlayConfig,
    win_state: &GameWindowState,
    projected: &OverlayProjectedData,
    rt: &mut OverlayRuntimeState,
    cfg_changed: &mut bool,
    is_inline_preview: bool,
) -> Vec2 {
    let scale = cfg.scale();
    let bg_alpha = (cfg.bg_alpha() * 255.0).round() as u8;
    let content_opacity = cfg.content_opacity();
    let is_unlocked = rt.unlocked_override && !is_inline_preview;

    let panel_bg = Color32::from_rgba_unmultiplied(10, 13, 20, bg_alpha);
    let card_bg_alpha = ((bg_alpha as u16 * 55) / 100).min(200) as u8;
    let card_bg = Color32::from_rgba_unmultiplied(18, 23, 33, card_bg_alpha);
    let border_alpha = if cfg.show_border {
        ((bg_alpha as u16 + 35).min(210)) as u8
    } else {
        0
    };
    let border_color = if is_unlocked {
        Color32::from_rgba_unmultiplied(245, 190, 65, 220)
    } else {
        Color32::from_rgba_unmultiplied(85, 105, 135, border_alpha)
    };

    let target_w = if cfg.minimized && !is_inline_preview {
        (225.0 * scale).clamp(170.0, 360.0)
    } else {
        match cfg.active_tab {
            // The DPS meter is deliberately narrow so it covers little of the game.
            OverlayTab::LiveDps => (215.0 * scale).clamp(180.0, 340.0),
            OverlayTab::Notes => (cfg.card_width * scale).clamp(190.0, 640.0),
            OverlayTab::MissionsQuests => match cfg.display_mode {
                OverlayDisplayMode::CompactCards => (cfg.card_width * scale).clamp(190.0, 640.0),
                OverlayDisplayMode::UltraCompactPills => match cfg.pill_orientation {
                    PillOrientation::Horizontal => (cfg.card_width * scale).clamp(200.0, 640.0),
                    PillOrientation::Vertical => (210.0 * scale).clamp(150.0, 400.0),
                },
            },
        }
    };

    let frame = egui::Frame::NONE
        .fill(panel_bg)
        .stroke(Stroke::new(
            if is_unlocked {
                1.5_f32
            } else if cfg.show_border {
                1.0_f32
            } else {
                0.0_f32
            },
            border_color,
        ))
        .corner_radius(CornerRadius::same(6))
        .inner_margin(Margin::symmetric((6.0 * scale) as i8, (5.0 * scale) as i8));

    let resp = frame.show(ui, |ui| {
        ui.multiply_opacity(content_opacity);
        if target_w > 0.0 {
            ui.set_width(target_w);
        }

        // Interactive Drag / Lock Banner when explicitly unlocked
        if is_unlocked {
            render_unlocked_drag_banner(ui, cfg, rt, cfg_changed, scale);
            ui.add_space(3.0 * scale);
        }

        // Top Header Bar: always rendered on the live overlay so dragging,
        // `⚙` (quick settings), `-` (minimize), and `✕` (close) are always accessible.
        if !is_inline_preview || cfg.show_header {
            render_overlay_header(
                ui,
                cfg,
                win_state,
                projected,
                rt,
                cfg_changed,
                is_inline_preview,
                scale,
            );
        }

        // Small quick settings bar (opacity + small/large icon mode).
        if rt.quick_settings_open && !cfg.minimized && !is_inline_preview {
            ui.add_space(3.0 * scale);
            render_quick_settings_bar(ui, cfg, cfg_changed, scale);
        }

        // Tab strip (DPS / Tasks / Notes) lives in the always-interactive top
        // zone so switching tabs works even with body click-through enabled.
        if !cfg.minimized || is_inline_preview {
            ui.add_space(3.0 * scale);
            render_tab_strip(ui, cfg, cfg_changed, scale);
        }

        if !is_inline_preview {
            // Everything above this line stays clickable even with click-through on.
            // `cursor().top()` is measured from the top of the overlay viewport.
            rt.interactive_top_pts = ui.cursor().top().max(0.0) + 3.0 * scale;
        }

        // If minimized (`-`), stop here so the overlay is just the sleek top bar!
        if cfg.minimized && !is_inline_preview {
            return;
        }

        ui.add_space(2.0 * scale);
        let rect = ui.available_rect_before_wrap();
        ui.painter().hline(
            rect.x_range(),
            rect.top(),
            Stroke::new(1.0_f32, Color32::from_rgba_unmultiplied(90, 110, 140, 55)),
        );

        ui.add_space(3.0 * scale);

        match cfg.active_tab {
            OverlayTab::LiveDps => {
                // Fixed, compact layout: no scroll area needed.
                rt.scroll_delta_y = 0.0;
                render_live_dps_body(
                    ui,
                    sprite_renderer,
                    projected.live_dps.as_ref(),
                    card_bg,
                    scale,
                );
                return;
            }
            OverlayTab::Notes => {
                let pending_scroll = if is_inline_preview {
                    0.0
                } else {
                    std::mem::take(&mut rt.scroll_delta_y)
                };
                render_notes_body(ui, cfg, rt, pending_scroll, is_inline_preview, scale);
                return;
            }
            OverlayTab::MissionsQuests => {}
        }

        // Scrollable Body so all missions & daily quests can be viewed cleanly
        let max_scroll_h = (cfg.max_height * scale).clamp(120.0, 900.0);
        let pending_scroll = if is_inline_preview {
            0.0
        } else {
            std::mem::take(&mut rt.scroll_delta_y)
        };
        egui::ScrollArea::vertical()
            .id_salt(if is_inline_preview {
                "overlay_preview_scroll"
            } else {
                "overlay_live_scroll"
            })
            .max_height(max_scroll_h)
            .auto_shrink([false, true])
            .animated(false)
            .show(ui, |ui| {
                if pending_scroll.abs() > 0.1 {
                    ui.scroll_with_delta(egui::vec2(0.0, pending_scroll));
                }
                match cfg.display_mode {
                    OverlayDisplayMode::CompactCards => {
                        render_compact_cards_body(
                            ui,
                            sprite_renderer,
                            cfg,
                            projected,
                            card_bg,
                            scale,
                        );
                    }
                    OverlayDisplayMode::UltraCompactPills => {
                        render_ultra_compact_pills_body(
                            ui,
                            sprite_renderer,
                            cfg,
                            projected,
                            card_bg,
                            scale,
                        );
                    }
                }
            });
    });

    resp.response.rect.size() + egui::vec2(2.0, 2.0)
}

fn render_unlocked_drag_banner(
    ui: &mut egui::Ui,
    cfg: &mut OverlayConfig,
    rt: &mut OverlayRuntimeState,
    cfg_changed: &mut bool,
    scale: f32,
) {
    let banner_bg = Color32::from_rgba_unmultiplied(65, 50, 16, 215);
    let banner_border = Color32::from_rgb(245, 190, 65);
    egui::Frame::NONE
        .fill(banner_bg)
        .stroke(Stroke::new(1.0_f32, banner_border))
        .corner_radius(CornerRadius::same(4))
        .inner_margin(Margin::symmetric(5, 3))
        .show(ui, |ui| {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let lock_btn = ui.add(
                    egui::Button::new(
                        RichText::new("🔒 Done")
                            .size(10.0 * scale)
                            .strong()
                            .color(Color32::WHITE),
                    )
                    .fill(Color32::from_rgb(45, 125, 85))
                    .corner_radius(CornerRadius::same(3)),
                );
                if lock_btn.clicked() {
                    rt.unlocked_override = false;
                    *cfg_changed = true;
                }

                // The rest of the banner is the drag handle.
                let handle_rect = ui.available_rect_before_wrap();
                let drag_resp = ui.interact(
                    handle_rect,
                    ui.id().with("overlay_unlocked_drag_handle"),
                    Sense::click_and_drag(),
                );
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    ui.add(
                        egui::Label::new(
                            RichText::new("✥ Drag to Reposition")
                                .size(10.5 * scale)
                                .strong()
                                .color(Color32::from_rgb(255, 220, 120)),
                        )
                        .truncate()
                        .selectable(false),
                    );
                });
                if drag_resp.hovered() && !rt.is_dragging() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
                }
                if drag_resp.is_pointer_button_down_on()
                    || drag_resp.drag_started()
                    || drag_resp.dragged()
                {
                    begin_overlay_drag(ui.ctx(), cfg, rt);
                }
            });
        });
}

/// Small square header icon button with pixel-perfect alignment and centering.
fn header_icon_button(
    ui: &mut egui::Ui,
    id_str: &str,
    glyph: &str,
    fill: Color32,
    rect: Rect,
    scale: f32,
) -> egui::Response {
    let resp = ui.interact(rect, ui.id().with(id_str), Sense::click());
    if ui.is_rect_visible(rect) {
        let bg = if resp.is_pointer_button_down_on() {
            fill.gamma_multiply(0.75)
        } else if resp.hovered() {
            fill.lerp_to_gamma(Color32::WHITE, 0.22)
        } else {
            fill
        };
        let r = CornerRadius::same((3.0 * scale).round() as u8);
        ui.painter().rect_filled(rect, r, bg);

        if resp.hovered() {
            ui.painter().rect_stroke(
                rect,
                r,
                Stroke::new(1.0_f32, Color32::from_rgba_unmultiplied(255, 255, 255, 55)),
                StrokeKind::Inside,
            );
        }

        ui.painter().text(
            rect.center(),
            Align2::CENTER_CENTER,
            glyph,
            FontId::proportional((9.5 * scale).round().max(8.5)),
            Color32::from_rgb(235, 240, 248),
        );
    }
    resp
}

#[allow(clippy::too_many_arguments)]
fn render_overlay_header(
    ui: &mut egui::Ui,
    cfg: &mut OverlayConfig,
    win_state: &GameWindowState,
    projected: &OverlayProjectedData,
    rt: &mut OverlayRuntimeState,
    cfg_changed: &mut bool,
    is_inline_preview: bool,
    scale: f32,
) {
    let row_h = (18.0 * scale).round().max(16.0);
    let (row_rect, bg_resp) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), row_h),
        if is_inline_preview {
            Sense::hover()
        } else {
            Sense::click_and_drag()
        },
    );

    // The whole header background is the drag handle (buttons on top of it
    // still receive their own clicks).
    if !is_inline_preview {
        if rt.is_dragging() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
        } else if bg_resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
        }
        if bg_resp.is_pointer_button_down_on()
            || bg_resp.drag_started()
            || bg_resp.dragged()
        {
            begin_overlay_drag(ui.ctx(), cfg, rt);
        }
        if bg_resp.double_clicked() {
            cfg.minimized = !cfg.minimized;
            *cfg_changed = true;
        }
    }

    let cy = row_rect.center().y;
    let btn_w = (16.0 * scale).round().max(14.0);
    let btn_h = (15.0 * scale).round().max(13.0);
    let btn_y = (cy - btn_h * 0.5).round();
    let btn_gap = (3.0 * scale).round().max(2.0);

    // Right-aligned controls are placed at strictly identical vertical positions (btn_y)
    // so they are guaranteed to sit on an exact straight line.
    let mut right_x = row_rect.right();

    // 1. [×] Close Button
    right_x -= btn_w;
    let close_rect = Rect::from_min_size(egui::pos2(right_x, btn_y), egui::vec2(btn_w, btn_h));
    let close_resp = header_icon_button(
        ui,
        "hdr_close",
        "×",
        Color32::from_rgba_unmultiplied(165, 45, 50, 165),
        close_rect,
        scale,
    )
    .on_hover_text("Close overlay");
    if close_resp.clicked() && !is_inline_preview {
        cfg.enabled = false;
        rt.preview_override = false;
        rt.unlocked_override = false;
        rt.quick_settings_open = false;
        *cfg_changed = true;
    }

    // 2. [—] / [+] Minimize / Restore Button
    right_x -= btn_gap + btn_w;
    let min_rect = Rect::from_min_size(egui::pos2(right_x, btn_y), egui::vec2(btn_w, btn_h));
    let min_glyph = if cfg.minimized { "+" } else { "—" };
    let min_resp = header_icon_button(
        ui,
        "hdr_min",
        min_glyph,
        Color32::from_rgba_unmultiplied(55, 70, 92, 165),
        min_rect,
        scale,
    )
    .on_hover_text(if cfg.minimized { "Expand" } else { "Minimize" });
    if min_resp.clicked() {
        cfg.minimized = !cfg.minimized;
        *cfg_changed = true;
    }

    // 3. [⚙] Quick settings bar toggle
    if !is_inline_preview && !cfg.minimized {
        right_x -= btn_gap + btn_w;
        let gear_rect = Rect::from_min_size(egui::pos2(right_x, btn_y), egui::vec2(btn_w, btn_h));
        let gear_fill = if rt.quick_settings_open {
            Color32::from_rgba_unmultiplied(70, 110, 165, 200)
        } else {
            Color32::from_rgba_unmultiplied(55, 70, 92, 165)
        };
        let gear_resp = header_icon_button(
            ui,
            "hdr_gear",
            "⚙",
            gear_fill,
            gear_rect,
            scale,
        )
        .on_hover_text("Quick settings (opacity, icon size)");
        if gear_resp.clicked() {
            rt.quick_settings_open = !rt.quick_settings_open;
        }
    }

    // 4. [📌] Snap back button (if custom coordinates)
    if cfg.anchor == OverlayAnchor::Custom && !is_inline_preview && !rt.is_dragging() {
        right_x -= btn_gap + btn_w;
        let snap_rect = Rect::from_min_size(egui::pos2(right_x, btn_y), egui::vec2(btn_w, btn_h));
        let snap_resp = header_icon_button(
            ui,
            "hdr_snap",
            "📌",
            Color32::from_rgba_unmultiplied(55, 70, 92, 140),
            snap_rect,
            scale,
        )
        .on_hover_text("Snap back to the game window's top-left corner");
        if snap_resp.clicked() {
            cfg.anchor = OverlayAnchor::TopLeft;
            *cfg_changed = true;
        }
    }

    // Left side: status dot, title, compact area badge
    let left_max_x = right_x - (4.0 * scale).round();
    let dot_radius = (3.0 * scale).max(2.5);
    let dot_center = egui::pos2(row_rect.left() + dot_radius + 1.0, cy);
    let dot_color = if win_state.rotmg_running {
        Color32::from_rgb(55, 215, 135)
    } else {
        Color32::from_rgb(135, 145, 160)
    };
    ui.painter().circle_filled(dot_center, dot_radius, dot_color);

    let title_font = FontId::proportional((9.2 * scale).round().max(8.0));
    let title_galley = ui.painter().layout_no_wrap(
        "REALMHOUND".to_string(),
        title_font,
        Color32::from_rgb(210, 220, 235),
    );
    let title_x = dot_center.x + dot_radius + (4.0 * scale).round();
    let title_pos = egui::pos2(title_x, cy - title_galley.size().y * 0.5);
    ui.painter().galley(title_pos, title_galley.clone(), Color32::from_rgb(210, 220, 235));

    // Area / Map badge (plus counts while minimized)
    let area_label = if !win_state.current_map_name.is_empty() {
        win_state.current_map_name.clone()
    } else {
        win_state.area_kind.label().to_string()
    };
    let badge_text = if cfg.minimized && !is_inline_preview {
        let mut counts = Vec::new();
        match cfg.active_tab {
            OverlayTab::LiveDps => {
                if let Some(snap) = projected.live_dps.as_ref() {
                    if let (Some(rank), Some(me)) = (snap.local_rank, snap.local_player.as_ref()) {
                        counts.push(format!("#{rank}"));
                        if let Some(dpm) = snap.dpm_for(me.damage) {
                            counts.push(format!("{} DPM", fmt_compact(dpm)));
                        }
                    }
                }
            }
            OverlayTab::MissionsQuests => {
                if cfg.show_missions && !projected.missions.is_empty() {
                    counts.push(format!("{}M", projected.missions.len()));
                }
                if cfg.show_quests && !projected.quests.is_empty() {
                    counts.push(format!("{}Q", projected.quests.len()));
                }
            }
            OverlayTab::Notes => {}
        }
        if counts.is_empty() {
            area_label
        } else {
            format!("{area_label} · {}", counts.join(" "))
        }
    } else {
        area_label
    };

    let badge_left = title_pos.x + title_galley.size().x + (5.0 * scale).round();
    let avail_badge_w = left_max_x - badge_left;

    // Render a sleek, compact badge that stays strictly inside the row bounds
    if avail_badge_w > 20.0 * scale {
        let badge_c = win_state.area_kind.badge_color();
        let badge_h = (13.0 * scale).round().max(11.0);
        let badge_y = (cy - badge_h * 0.5).round();
        let pad_x = (4.0 * scale).round().max(3.0);
        let badge_font = FontId::proportional((8.2 * scale).round().max(7.5));
        let badge_galley = ui.painter().layout_no_wrap(
            badge_text.clone(),
            badge_font,
            badge_c,
        );
        let text_w = badge_galley.size().x;
        let badge_w = (text_w + pad_x * 2.0).min(avail_badge_w);

        if badge_w >= 16.0 * scale {
            let badge_rect = Rect::from_min_size(
                egui::pos2(badge_left, badge_y),
                egui::vec2(badge_w, badge_h),
            );
            let fill_c = Color32::from_rgba_unmultiplied(
                badge_c.r(),
                badge_c.g(),
                badge_c.b(),
                36,
            );
            let stroke_c = Color32::from_rgba_unmultiplied(
                badge_c.r(),
                badge_c.g(),
                badge_c.b(),
                110,
            );
            let r = CornerRadius::same((3.0 * scale).round() as u8);
            ui.painter().rect_filled(badge_rect, r, fill_c);
            ui.painter().rect_stroke(
                badge_rect,
                r,
                Stroke::new(1.0_f32, stroke_c),
                StrokeKind::Inside,
            );

            if text_w <= badge_w - pad_x * 2.0 {
                let text_pos = egui::pos2(
                    badge_rect.center().x - text_w * 0.5,
                    cy - badge_galley.size().y * 0.5,
                );
                ui.painter().galley(text_pos, badge_galley, badge_c);
            } else {
                let clip = badge_rect.shrink2(egui::vec2(pad_x, 0.0));
                let text_pos = egui::pos2(
                    badge_rect.left() + pad_x,
                    cy - badge_galley.size().y * 0.5,
                );
                ui.painter().with_clip_rect(clip).galley(text_pos, badge_galley, badge_c);
            }

            let badge_resp = ui.interact(badge_rect, ui.id().with("hdr_badge"), Sense::hover());
            badge_resp.on_hover_text(&badge_text);
        }
    }
}

/// Compact in-overlay settings strip: background opacity slider and a
/// Small (icon pills) / Large (cards) display toggle.
fn render_quick_settings_bar(
    ui: &mut egui::Ui,
    cfg: &mut OverlayConfig,
    cfg_changed: &mut bool,
    scale: f32,
) {
    egui::Frame::NONE
        .fill(Color32::from_rgba_unmultiplied(20, 26, 38, 205))
        .stroke(Stroke::new(
            1.0_f32,
            Color32::from_rgba_unmultiplied(90, 115, 150, 110),
        ))
        .corner_radius(CornerRadius::same(4))
        .inner_margin(Margin::symmetric((5.0 * scale) as i8, (3.0 * scale) as i8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0 * scale;
                let txt = |s: &str| {
                    RichText::new(s)
                        .size(9.0 * scale)
                        .color(Color32::from_rgb(200, 212, 230))
                };

                // Size toggle on the right: [Small][Large]
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 2.0 * scale;
                    for (mode, label, tip) in [
                        (
                            OverlayDisplayMode::CompactCards,
                            "Large",
                            "Large: cards with progress bars",
                        ),
                        (
                            OverlayDisplayMode::UltraCompactPills,
                            "Small",
                            "Small: compact icon pills",
                        ),
                    ] {
                        let selected = cfg.display_mode == mode;
                        let fill = if selected {
                            Color32::from_rgba_unmultiplied(55, 120, 200, 220)
                        } else {
                            Color32::from_rgba_unmultiplied(40, 50, 68, 180)
                        };
                        let btn = ui
                            .add(
                                egui::Button::new(
                                    RichText::new(label)
                                        .size(9.0 * scale)
                                        .strong()
                                        .color(Color32::from_rgb(235, 240, 248)),
                                )
                                .fill(fill)
                                .corner_radius(CornerRadius::same(3)),
                            )
                            .on_hover_text(tip);
                        if btn.clicked() && !selected {
                            cfg.display_mode = mode;
                            *cfg_changed = true;
                        }
                    }

                    ui.add_space(4.0 * scale);

                    // Opacity slider fills the remaining space on the left.
                    ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                        ui.label(txt("Opacity"));
                        let value_w = 26.0 * scale;
                        ui.spacing_mut().slider_width =
                            (ui.available_width() - value_w - 8.0 * scale).clamp(36.0, 220.0);
                        let slider = ui
                            .add(
                                egui::Slider::new(&mut cfg.bg_opacity_pct, 10..=98)
                                    .show_value(false),
                            )
                            .on_hover_text("Overlay background opacity");
                        ui.label(txt(&format!("{}%", cfg.bg_opacity_pct)));
                        // Persist once the drag ends instead of every frame.
                        if slider.drag_stopped() || (slider.changed() && !slider.dragged()) {
                            *cfg_changed = true;
                        }
                    });
                });
            });
        });
}

// ---------------------------------------------------------------------------
// Tab strip
// ---------------------------------------------------------------------------

/// Three equal-width segmented buttons: DPS / Tasks / Notes.
fn render_tab_strip(ui: &mut egui::Ui, cfg: &mut OverlayConfig, cfg_changed: &mut bool, scale: f32) {
    let gap = 2.0 * scale;
    let h = (15.0 * scale).max(13.0);
    let count = OverlayTab::ALL.len() as f32;
    let w = ((ui.available_width() - gap * (count - 1.0)) / count).max(30.0);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = gap;
        ui.spacing_mut().interact_size.y = h;
        ui.spacing_mut().button_padding = egui::vec2(2.0, 1.0);
        for tab in OverlayTab::ALL {
            let selected = cfg.active_tab == tab;
            let (fill, text) = if selected {
                (
                    Color32::from_rgba_unmultiplied(55, 120, 200, 215),
                    Color32::from_rgb(240, 245, 252),
                )
            } else {
                (
                    Color32::from_rgba_unmultiplied(35, 44, 60, 170),
                    Color32::from_rgb(170, 182, 200),
                )
            };
            let btn = ui
                .add_sized(
                    egui::vec2(w, h),
                    egui::Button::new(
                        RichText::new(tab.short_label())
                            .size(9.2 * scale)
                            .strong()
                            .color(text),
                    )
                    .fill(fill)
                    .corner_radius(CornerRadius::same(3)),
                )
                .on_hover_text(tab.label());
            if btn.clicked() && !selected {
                cfg.active_tab = tab;
                *cfg_changed = true;
            }
        }
    });
}

// ---------------------------------------------------------------------------
// Live DPS tab
// ---------------------------------------------------------------------------

/// Compact number: `950`, `12.4K`, `1.25M`, `3.1B`.
fn fmt_compact(v: f64) -> String {
    let a = v.abs();
    let sign = if v < 0.0 { "-" } else { "" };
    if a >= 1_000_000_000.0 {
        format!("{sign}{:.2}B", a / 1_000_000_000.0)
    } else if a >= 1_000_000.0 {
        format!("{sign}{:.2}M", a / 1_000_000.0)
    } else if a >= 10_000.0 {
        format!("{sign}{:.1}K", a / 1_000.0)
    } else if a >= 1_000.0 {
        format!("{sign}{:.2}K", a / 1_000.0)
    } else {
        format!("{sign}{:.0}", a)
    }
}

/// Integer with thousands separators (`142,500`).
fn fmt_thousands(n: i64) -> String {
    let digits = n.unsigned_abs().to_string();
    let len = digits.len();
    let mut out = String::with_capacity(len + len / 3 + 1);
    if n < 0 {
        out.push('-');
    }
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (len - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// `m:ss` duration.
fn fmt_mss(ms: i64) -> String {
    let secs = ms.max(0) / 1000;
    format!("{}:{:02}", secs / 60, secs % 60)
}

/// Gold / silver / bronze for the podium, accent blue otherwise.
fn rank_color(rank: usize) -> Color32 {
    match rank {
        1 => Color32::from_rgb(255, 205, 70),
        2 => Color32::from_rgb(205, 215, 228),
        3 => Color32::from_rgb(215, 140, 85),
        _ => Color32::from_rgb(120, 175, 245),
    }
}

/// Character sprite (skin + dyes) for a participant, falling back to the class.
fn draw_participant_icon(
    ui: &mut egui::Ui,
    sprite_renderer: &mut SpriteRenderer,
    p: &LiveDpsParticipant,
    size: f32,
) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(size, size), Sense::hover());
    let icon = if p.skin_id > 0 { p.skin_id } else { p.object_type };
    if icon != 0
        && !sprite_renderer.draw_dyed_outlined_character_sprite(ui, icon, rect, 6, p.tex1, p.tex2)
        && p.object_type != 0
    {
        sprite_renderer.draw_sprite_in_rect(ui, p.object_type, rect);
    }
}

fn small_label(ui: &mut egui::Ui, text: impl Into<String>, size: f32, color: Color32, strong: bool) {
    let mut rt = RichText::new(text.into()).size(size).color(color);
    if strong {
        rt = rt.strong();
    }
    ui.add(egui::Label::new(rt).truncate().selectable(false));
}

/// Compact live boss DPS meter: boss header, your rank/damage/DPM, and the
/// top damage dealer to compare against.
fn render_live_dps_body(
    ui: &mut egui::Ui,
    sprite_renderer: &mut SpriteRenderer,
    snap: Option<&LiveBossDpsSnapshot>,
    card_bg: Color32,
    scale: f32,
) {
    let dim = Color32::from_rgb(150, 162, 180);
    let bright = Color32::from_rgb(232, 238, 246);

    let Some(s) = snap else {
        ui.label(
            RichText::new("Waiting for a boss fight…")
                .size(10.0 * scale)
                .italics()
                .color(Color32::from_rgb(170, 180, 195)),
        );
        ui.label(
            RichText::new("Your rank, damage and DPM appear here once a boss is engaged.")
                .size(9.0 * scale)
                .color(dim),
        );
        return;
    };

    // --- Boss header -------------------------------------------------------
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0 * scale;
        let icon_sz = 18.0 * scale;
        let (rect, _) = ui.allocate_exact_size(egui::vec2(icon_sz, icon_sz), Sense::hover());
        if s.boss_object_type != 0 {
            sprite_renderer.draw_outlined_sprite_fitting_cell(ui, s.boss_object_type, rect);
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 4.0 * scale;
            let (status, color) = if s.is_active {
                ("LIVE", Color32::from_rgb(70, 215, 130))
            } else if s.killed {
                ("KILLED", Color32::from_rgb(150, 200, 150))
            } else {
                ("ENDED", Color32::from_rgb(150, 155, 165))
            };
            egui::Frame::NONE
                .fill(Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 40))
                .corner_radius(CornerRadius::same(3))
                .inner_margin(Margin::symmetric(3, 0))
                .show(ui, |ui| {
                    small_label(ui, status, 8.5 * scale, color, true);
                });
            small_label(ui, fmt_mss(s.duration_ms), 9.0 * scale, dim, false);
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                small_label(ui, s.boss_name.clone(), 10.0 * scale, bright, true);
            });
        });
    });

    // Thin boss HP bar while the fight is live and HP is known.
    if s.is_active {
        if let (Some(hp), true) = (s.boss_hp, s.boss_max_hp > 0) {
            let frac = (hp as f32 / s.boss_max_hp as f32).clamp(0.0, 1.0);
            let (rect, _) = ui.allocate_exact_size(
                egui::vec2(ui.available_width(), (3.0 * scale).max(2.0)),
                Sense::hover(),
            );
            let p = ui.painter();
            p.rect_filled(rect, CornerRadius::same(1), Color32::from_rgba_unmultiplied(60, 25, 30, 170));
            let mut fill = rect;
            fill.set_width(rect.width() * frac);
            p.rect_filled(fill, CornerRadius::same(1), Color32::from_rgb(215, 70, 70));
        }
    }

    ui.add_space(3.0 * scale);

    let card = |ui: &mut egui::Ui, accent: Color32, add: &mut dyn FnMut(&mut egui::Ui)| {
        egui::Frame::NONE
            .fill(card_bg)
            .stroke(Stroke::new(
                1.0_f32,
                Color32::from_rgba_unmultiplied(accent.r(), accent.g(), accent.b(), 90),
            ))
            .corner_radius(CornerRadius::same(4))
            .inner_margin(Margin::symmetric((5.0 * scale) as i8, (3.0 * scale) as i8))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                add(ui);
            });
    };

    // --- You -----------------------------------------------------------------
    let you_accent = s.local_rank.map(rank_color).unwrap_or(dim);
    card(ui, you_accent, &mut |ui| match (s.local_player.as_ref(), s.local_rank) {
        (Some(me), Some(rank)) => {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0 * scale;
                small_label(ui, format!("#{rank}"), 14.0 * scale, rank_color(rank), true);
                small_label(
                    ui,
                    format!("of {}", s.participant_count),
                    9.0 * scale,
                    dim,
                    false,
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let dpm = s.dpm_for(me.damage).unwrap_or(0.0);
                    small_label(ui, "DPM", 8.5 * scale, dim, true);
                    small_label(ui, fmt_compact(dpm), 12.0 * scale, bright, true);
                });
            });
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0 * scale;
                small_label(ui, "YOU", 8.5 * scale, dim, true);
                small_label(ui, fmt_thousands(me.damage), 10.0 * scale, bright, true);
                if let Some(pct) = s.share_pct_for(me.damage) {
                    small_label(ui, format!("({pct:.1}%)"), 9.0 * scale, dim, false);
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if let Some(dps) = s.dps_for(me.damage) {
                        small_label(ui, format!("{}/s", fmt_compact(dps)), 9.0 * scale, dim, false);
                    }
                });
            });
        }
        _ => {
            small_label(
                ui,
                "You haven't damaged this boss yet.",
                9.5 * scale,
                dim,
                false,
            );
        }
    });

    // --- Top damage / comparison ---------------------------------------------
    let you_are_top = s.top_player.as_ref().is_some_and(|t| t.is_local);
    let (compare, label, rank_shown) = if you_are_top {
        (s.runner_up_player.as_ref(), "RUNNER-UP", 2)
    } else {
        (s.top_player.as_ref(), "TOP", 1)
    };
    let Some(other) = compare else {
        if you_are_top {
            ui.add_space(2.0 * scale);
            small_label(ui, "Solo — no one to compare against.", 9.0 * scale, dim, false);
        }
        return;
    };

    ui.add_space(2.5 * scale);
    card(ui, rank_color(rank_shown), &mut |ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0 * scale;
            draw_participant_icon(ui, sprite_renderer, other, 16.0 * scale);
            small_label(ui, format!("#{rank_shown}"), 10.0 * scale, rank_color(rank_shown), true);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let dpm = s.dpm_for(other.damage).unwrap_or(0.0);
                small_label(ui, "DPM", 8.5 * scale, dim, true);
                small_label(ui, fmt_compact(dpm), 11.0 * scale, bright, true);
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    small_label(ui, other.name.clone(), 10.0 * scale, bright, true);
                });
            });
        });
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0 * scale;
            small_label(ui, label, 8.5 * scale, dim, true);
            small_label(ui, fmt_thousands(other.damage), 9.5 * scale, bright, false);
            if let Some(pct) = s.share_pct_for(other.damage) {
                small_label(ui, format!("({pct:.1}%)"), 9.0 * scale, dim, false);
            }
            if let Some(me) = s.local_player.as_ref() {
                let delta = me.damage - other.damage;
                let (text, color) = if delta >= 0 {
                    (
                        format!("+{}", fmt_compact(delta as f64)),
                        Color32::from_rgb(90, 215, 135),
                    )
                } else {
                    (
                        format!("-{}", fmt_compact((-delta) as f64)),
                        Color32::from_rgb(235, 110, 100),
                    )
                };
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    small_label(ui, text, 9.5 * scale, color, true)
                })
                .response
                .on_hover_text(if delta >= 0 {
                    "Your damage lead"
                } else {
                    "Damage behind the top player"
                });
            }
        });
    });
}

// ---------------------------------------------------------------------------
// Notes tab
// ---------------------------------------------------------------------------

/// Free-form notes text box. Edits are flagged on `rt.notes_dirty_at` and
/// persisted by the overlay manager once typing pauses.
fn render_notes_body(
    ui: &mut egui::Ui,
    cfg: &mut OverlayConfig,
    rt: &mut OverlayRuntimeState,
    pending_scroll: f32,
    is_inline_preview: bool,
    scale: f32,
) {
    let max_h = (cfg.max_height * scale).clamp(120.0, 900.0);
    egui::ScrollArea::vertical()
        .id_salt(if is_inline_preview {
            "overlay_preview_notes_scroll"
        } else {
            "overlay_live_notes_scroll"
        })
        .max_height(max_h)
        .auto_shrink([false, true])
        .animated(false)
        .show(ui, |ui| {
            if pending_scroll.abs() > 0.1 {
                ui.scroll_with_delta(egui::vec2(0.0, pending_scroll));
            }
            let edit = egui::TextEdit::multiline(&mut cfg.notes_text)
                .id_salt(if is_inline_preview {
                    "overlay_preview_notes"
                } else {
                    "overlay_live_notes"
                })
                .hint_text("Type notes here… (saved automatically)")
                .font(FontId::proportional(10.5 * scale))
                .text_color(Color32::from_rgb(225, 232, 242))
                .desired_width(f32::INFINITY)
                .desired_rows(6)
                .frame(true)
                .interactive(!is_inline_preview);
            let resp = ui.add(edit);
            if !is_inline_preview {
                if resp.changed() {
                    rt.notes_dirty_at = Some(Instant::now());
                }
                rt.notes_focused = resp.has_focus();
            }
        });
}

fn render_compact_cards_body(
    ui: &mut egui::Ui,
    sprite_renderer: &mut SpriteRenderer,
    cfg: &OverlayConfig,
    projected: &OverlayProjectedData,
    card_bg: Color32,
    scale: f32,
) {
    let show_m = cfg.show_missions && !projected.missions.is_empty();
    let show_q = cfg.show_quests && !projected.quests.is_empty();

    if !show_m && !show_q {
        let hint = if !projected.defs_loaded && cfg.show_missions {
            "No missions/quests loaded yet — press Refresh in RealmHound or visit the Tinkerer."
        } else {
            "No matching missions or daily quests right now."
        };
        ui.label(
            RichText::new(hint)
                .size(10.0 * scale)
                .italics()
                .color(Color32::from_rgb(155, 165, 180)),
        );
        return;
    }

    if show_m {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(format!("MISSIONS ({})", projected.missions.len()))
                    .size(9.0 * scale)
                    .strong()
                    .color(Color32::from_rgb(145, 165, 195)),
            );
        });
        ui.add_space(1.5 * scale);

        for (i, card) in projected.missions.iter().enumerate() {
            if i > 0 {
                ui.add_space(2.5 * scale);
            }
            render_single_mission_card(ui, sprite_renderer, cfg, card, card_bg, scale);
        }
    }

    if show_m && show_q {
        ui.add_space(4.5 * scale);
    }

    if show_q {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(format!("DAILY QUESTS ({})", projected.quests.len()))
                    .size(9.0 * scale)
                    .strong()
                    .color(Color32::from_rgb(145, 165, 195)),
            );
            if projected.quests_stale {
                ui.label(
                    RichText::new("⚠ Stale")
                        .size(9.0 * scale)
                        .color(Color32::from_rgb(235, 125, 95)),
                );
            }
        });
        ui.add_space(1.5 * scale);

        for (i, qrow) in projected.quests.iter().enumerate() {
            if i > 0 {
                ui.add_space(2.5 * scale);
            }
            render_single_quest_row(
                ui,
                sprite_renderer,
                cfg,
                qrow,
                projected.primary_seasonal,
                projected.show_both_quest_sides,
                projected.quests_stale,
                card_bg,
                scale,
            );
        }
    }
}

fn render_single_mission_card(
    ui: &mut egui::Ui,
    sprite_renderer: &mut SpriteRenderer,
    cfg: &OverlayConfig,
    card: &OverlayMissionCard,
    card_bg: Color32,
    scale: f32,
) {
    let (cr, cg, cb) = card.category.color();
    let cat_color = Color32::from_rgb(cr, cg, cb);
    let is_claimable = card.state == MissionState::Claimable;

    let border_c = if is_claimable {
        Color32::from_rgba_unmultiplied(245, 200, 65, 185)
    } else {
        Color32::from_rgba_unmultiplied(cr, cg, cb, 75)
    };

    let card_resp = egui::Frame::NONE
        .fill(card_bg)
        .stroke(Stroke::new(1.0_f32, border_c))
        .corner_radius(CornerRadius::same(4))
        .inner_margin(Margin {
            left: (7.0 * scale) as i8,
            right: (5.0 * scale) as i8,
            top: (3.0 * scale) as i8,
            bottom: (3.0 * scale) as i8,
        })
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            if !card.eligible {
                ui.multiply_opacity(0.60);
            }

            // Top line: Category micro-tag + Mission Name + Compact Rewards / CLAIM badge
            if cfg.show_mission_names || cfg.show_mission_rewards || is_claimable {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0 * scale;

                    let short_cat = match card.category {
                        MissionCategory::Regular => "REG",
                        MissionCategory::Seasonal => "SSN",
                        MissionCategory::Crucible => "CRU",
                        MissionCategory::Event => "EVT",
                    };
                    egui::Frame::NONE
                        .fill(Color32::from_rgba_unmultiplied(cr, cg, cb, 40))
                        .corner_radius(CornerRadius::same(2))
                        .inner_margin(Margin::symmetric(3, 0))
                        .show(ui, |ui| {
                            ui.label(
                                RichText::new(short_cat)
                                    .size(8.0 * scale)
                                    .strong()
                                    .color(cat_color),
                            );
                        });

                    if cfg.show_mission_names {
                        let title = if card.name.is_empty() {
                            &card.desc
                        } else {
                            &card.name
                        };
                        ui.add(
                            egui::Label::new(
                                RichText::new(title)
                                    .size(10.0 * scale)
                                    .strong()
                                    .color(Color32::from_rgb(235, 240, 248)),
                            )
                            .truncate(),
                        );
                    }

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 3.0 * scale;
                        if is_claimable {
                            egui::Frame::NONE
                                .fill(Color32::from_rgba_unmultiplied(240, 190, 40, 55))
                                .stroke(Stroke::new(1.0_f32, Color32::from_rgb(245, 200, 65)))
                                .corner_radius(CornerRadius::same(2))
                                .inner_margin(Margin::symmetric(3, 0))
                                .show(ui, |ui| {
                                    ui.label(
                                        RichText::new("CLAIM")
                                            .size(8.0 * scale)
                                            .strong()
                                            .color(Color32::from_rgb(255, 220, 90)),
                                    );
                                });
                        } else if cfg.show_mission_rewards {
                            for chip in card.item_rewards.iter().take(2).rev() {
                                if chip.amount > 1 {
                                    ui.label(
                                        RichText::new(format!("x{}", chip.amount))
                                            .size(8.5 * scale)
                                            .color(Color32::from_rgb(210, 215, 225)),
                                    );
                                }
                                let (r_rect, _) = ui.allocate_exact_size(
                                    egui::vec2(13.0 * scale, 13.0 * scale),
                                    Sense::hover(),
                                );
                                sprite_renderer.draw_outlined_sprite_in_rect_fit_tinted(
                                    ui,
                                    chip.object_id,
                                    r_rect,
                                    Color32::WHITE,
                                );
                            }
                            if card.bxp > 0 {
                                ui.label(
                                    RichText::new(format_bxp_compact(card.bxp))
                                        .size(8.5 * scale)
                                        .strong()
                                        .color(Color32::from_rgb(75, 225, 145)),
                                );
                            }
                        }
                    });
                });
                ui.add_space(1.5 * scale);
            }

            // Objective rows (Compact icon + Progress bar with label & have/need)
            for (idx, obj) in card.objectives.iter().enumerate() {
                if idx > 0 {
                    ui.add_space(1.5 * scale);
                }
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0 * scale;

                    let icon_sz = 14.0 * scale;
                    let (icon_rect, _) =
                        ui.allocate_exact_size(egui::vec2(icon_sz, icon_sz), Sense::hover());
                    paint_obj_icon(ui, sprite_renderer, &obj.icon, icon_rect, Color32::WHITE);

                    let avail_w = ui.available_width().max(50.0);
                    let bar_h = 12.5 * scale;
                    let (bar_rect, _) =
                        ui.allocate_exact_size(egui::vec2(avail_w, bar_h), Sense::hover());

                    let frac = if obj.need > 0 {
                        (obj.have as f32 / obj.need as f32).clamp(0.0, 1.0)
                    } else {
                        0.0
                    };
                    let done = obj.have >= obj.need && obj.need > 0;

                    let track_bg = Color32::from_rgba_unmultiplied(10, 13, 20, 190);
                    let fill_c = if done || is_claimable {
                        Color32::from_rgba_unmultiplied(215, 165, 35, 205)
                    } else {
                        Color32::from_rgba_unmultiplied(45, 125, 210, 195)
                    };

                    ui.painter()
                        .rect_filled(bar_rect, CornerRadius::same(2), track_bg);
                    if frac > 0.0 {
                        let fill_w = (bar_rect.width() * frac).max(3.0);
                        let fill_rect =
                            Rect::from_min_size(bar_rect.min, egui::vec2(fill_w, bar_rect.height()));
                        ui.painter()
                            .rect_filled(fill_rect, CornerRadius::same(2), fill_c);
                    }
                    ui.painter().rect_stroke(
                        bar_rect,
                        CornerRadius::same(2),
                        Stroke::new(1.0_f32, Color32::from_rgba_unmultiplied(90, 115, 145, 85)),
                        StrokeKind::Inside,
                    );

                    let count_text = format!("{}/{}", obj.have, obj.need);
                    let font = FontId::proportional(9.0 * scale);
                    let count_galley = ui.painter().layout_no_wrap(
                        count_text,
                        font.clone(),
                        Color32::from_rgb(245, 248, 255),
                    );
                    let count_pos = egui::pos2(
                        bar_rect.right() - count_galley.size().x - 4.0 * scale,
                        bar_rect.center().y - count_galley.size().y * 0.5,
                    );

                    let max_lbl_w = (count_pos.x - bar_rect.left() - 6.0 * scale).max(16.0);
                    let prefix = if obj.is_choice { "OR: " } else { "" };
                    let lbl_str = format!("{prefix}{}", obj.label);
                    let lbl_galley = ui.painter().layout_no_wrap(
                        lbl_str,
                        font,
                        Color32::from_rgb(225, 232, 242),
                    );
                    let clip_rect = Rect::from_min_size(
                        bar_rect.min,
                        egui::vec2(max_lbl_w + 4.0 * scale, bar_rect.height()),
                    );
                    ui.painter().with_clip_rect(clip_rect).galley(
                        egui::pos2(
                            bar_rect.left() + 4.0 * scale,
                            bar_rect.center().y - lbl_galley.size().y * 0.5,
                        ),
                        lbl_galley,
                        Color32::WHITE,
                    );
                    ui.painter().galley(count_pos, count_galley, Color32::WHITE);
                });
            }
        });

    let r = card_resp.response.rect;
    let stripe = Rect::from_min_max(
        egui::pos2(r.left() + 1.0, r.top() + 2.0),
        egui::pos2(r.left() + 3.5, r.bottom() - 2.0),
    );
    ui.painter()
        .rect_filled(stripe, CornerRadius::same(1), cat_color);
}

#[allow(clippy::too_many_arguments)]
fn render_single_quest_row(
    ui: &mut egui::Ui,
    sprite_renderer: &mut SpriteRenderer,
    cfg: &OverlayConfig,
    qrow: &OverlayQuestRow,
    primary_seasonal: bool,
    show_both_sides: bool,
    quests_stale: bool,
    card_bg: Color32,
    scale: f32,
) {
    let is_ready = if show_both_sides {
        qrow.seasonal_completable || qrow.regular_completable
    } else if primary_seasonal {
        qrow.seasonal_completable
    } else {
        qrow.regular_completable
    };

    let highlight_ready = cfg.highlight_completable_quests && is_ready && !quests_stale;
    let border_c = if quests_stale {
        Color32::from_rgba_unmultiplied(210, 80, 70, 140)
    } else if highlight_ready {
        Color32::from_rgba_unmultiplied(70, 225, 145, 195)
    } else {
        Color32::from_rgba_unmultiplied(80, 100, 130, 75)
    };

    egui::Frame::NONE
        .fill(card_bg)
        .stroke(Stroke::new(1.0_f32, border_c))
        .corner_radius(CornerRadius::same(4))
        .inner_margin(Margin::symmetric((5.0 * scale) as i8, (2.5 * scale) as i8))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            if quests_stale {
                ui.multiply_opacity(0.65);
            }

            if cfg.show_quest_names || highlight_ready {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0 * scale;

                    if cfg.show_quest_rewards {
                        if let Some(rew_id) = qrow.reward_item_id {
                            let sz = 14.0 * scale;
                            let (rect, _) =
                                ui.allocate_exact_size(egui::vec2(sz, sz), Sense::hover());
                            sprite_renderer.draw_outlined_sprite_in_rect_fit_tinted(
                                ui,
                                rew_id,
                                rect,
                                Color32::WHITE,
                            );
                        }
                    }

                    if cfg.show_quest_names {
                        let display_name = clean_quest_name(&qrow.name);
                        ui.add(
                            egui::Label::new(
                                RichText::new(display_name)
                                    .size(9.5 * scale)
                                    .strong()
                                    .color(Color32::from_rgb(232, 238, 246)),
                            )
                            .truncate(),
                        );
                    }

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if highlight_ready {
                            egui::Frame::NONE
                                .fill(Color32::from_rgba_unmultiplied(35, 185, 110, 55))
                                .stroke(Stroke::new(1.0_f32, Color32::from_rgb(70, 225, 145)))
                                .corner_radius(CornerRadius::same(2))
                                .inner_margin(Margin::symmetric(3, 0))
                                .show(ui, |ui| {
                                    ui.label(
                                        RichText::new("READY")
                                            .size(8.0 * scale)
                                            .strong()
                                            .color(Color32::from_rgb(100, 245, 165)),
                                    );
                                });
                        }
                    });
                });
                ui.add_space(1.0 * scale);
            }

            // Optional leading reward icon, then one framed chip per mark.
            let lead_reward = (!cfg.show_quest_names && cfg.show_quest_rewards)
                .then_some(qrow.reward_item_id)
                .flatten();
            let lead = usize::from(lead_reward.is_some());
            wrapped_flow(
                ui,
                ("overlay_quest_marks", &qrow.name),
                lead + qrow.marks.len(),
                egui::vec2(4.0 * scale, 2.0 * scale),
                |ui, i| {
                    if let (Some(rew_id), 0) = (lead_reward, i) {
                        return ui
                            .horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 4.0 * scale;
                                let sz = 14.0 * scale;
                                let (rect, _) =
                                    ui.allocate_exact_size(egui::vec2(sz, sz), Sense::hover());
                                sprite_renderer.draw_outlined_sprite_in_rect_fit_tinted(
                                    ui,
                                    rew_id,
                                    rect,
                                    Color32::WHITE,
                                );
                                ui.label(
                                    RichText::new("←")
                                        .size(9.0 * scale)
                                        .color(Color32::from_rgb(150, 160, 175)),
                                );
                            })
                            .response
                            .rect;
                    }
                    render_quest_mark_chip(
                        ui,
                        sprite_renderer,
                        &qrow.marks[i - lead],
                        primary_seasonal,
                        show_both_sides,
                        scale,
                    )
                },
            );
        });
}

fn render_quest_mark_chip(
    ui: &mut egui::Ui,
    sprite_renderer: &mut SpriteRenderer,
    m: &OverlayQuestMark,
    primary_seasonal: bool,
    show_both_sides: bool,
    scale: f32,
) -> Rect {
    let have_primary = if primary_seasonal {
        m.seasonal_have
    } else {
        m.regular_have
    };
    let met = if show_both_sides {
        m.seasonal_have >= m.need || m.regular_have >= m.need
    } else {
        have_primary >= m.need
    };

    let chip_bg = if met {
        Color32::from_rgba_unmultiplied(30, 85, 55, 160)
    } else {
        Color32::from_rgba_unmultiplied(14, 18, 26, 175)
    };
    let chip_border = if met {
        Color32::from_rgba_unmultiplied(70, 215, 135, 150)
    } else {
        Color32::from_rgba_unmultiplied(75, 95, 120, 85)
    };

    egui::Frame::NONE
        .fill(chip_bg)
        .stroke(Stroke::new(1.0_f32, chip_border))
        .corner_radius(CornerRadius::same(3))
        .inner_margin(Margin::symmetric(3, 1))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 2.5 * scale;
                let sz = 13.5 * scale;
                let (rect, _) = ui.allocate_exact_size(egui::vec2(sz, sz), Sense::hover());
                sprite_renderer.draw_outlined_sprite_in_rect_fit_tinted(
                    ui,
                    m.item_id,
                    rect,
                    Color32::WHITE,
                );

                if show_both_sides {
                    let s_col = if m.seasonal_have >= m.need {
                        Color32::from_rgb(80, 240, 160)
                    } else {
                        Color32::from_rgb(80, 215, 195)
                    };
                    let r_col = if m.regular_have >= m.need {
                        Color32::from_rgb(80, 240, 160)
                    } else {
                        Color32::from_rgb(235, 200, 85)
                    };
                    ui.label(
                        RichText::new(format!(
                            "S:{}/{}",
                            m.seasonal_have.min(m.need),
                            m.need
                        ))
                        .size(8.8 * scale)
                        .strong()
                        .color(s_col),
                    );
                    ui.label(
                        RichText::new(format!(
                            "R:{}/{}",
                            m.regular_have.min(m.need),
                            m.need
                        ))
                        .size(8.8 * scale)
                        .strong()
                        .color(r_col),
                    );
                } else {
                    let txt_col = if met {
                        Color32::from_rgb(95, 245, 165)
                    } else if have_primary > 0 {
                        Color32::from_rgb(240, 210, 95)
                    } else {
                        Color32::from_rgb(200, 210, 225)
                    };
                    ui.label(
                        RichText::new(format!("{}/{}", have_primary.min(m.need), m.need))
                            .size(9.2 * scale)
                            .strong()
                            .color(txt_col),
                    );
                }
            });
        })
        .response
        .rect
}

fn clean_quest_name(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return "Daily Quest".to_string();
    }
    trimmed.to_string()
}

/// Lay out `count` variable-width items left-to-right, wrapping onto new rows
/// when the available width runs out.
///
/// egui's `horizontal_wrapped` only wraps individual widgets, not grouped
/// widgets such as `Frame`s, so framed pills/chips would otherwise all end up
/// on a single ever-widening row. Item widths are measured each frame and
/// cached in egui temp memory to decide the row breaks on the next frame.
fn wrapped_flow(
    ui: &mut egui::Ui,
    id_salt: impl std::hash::Hash,
    count: usize,
    gap: Vec2,
    mut render_item: impl FnMut(&mut egui::Ui, usize) -> Rect,
) {
    if count == 0 {
        return;
    }
    let base_id = ui.id().with(id_salt);
    let max_w = ui.available_width().max(1.0);
    let widths: Vec<Option<f32>> = (0..count)
        .map(|i| ui.ctx().data(|d| d.get_temp::<f32>(base_id.with(i))))
        .collect();
    let mut needs_repaint = widths.iter().any(Option::is_none);

    // Greedy row breaking using last frame's measured widths.
    let fallback_w = 48.0;
    let mut rows: Vec<std::ops::Range<usize>> = Vec::new();
    let mut row_start = 0;
    let mut row_w = 0.0;
    for (i, w) in widths.iter().enumerate() {
        let w = w.unwrap_or(fallback_w);
        if i > row_start && row_w + gap.x + w > max_w {
            rows.push(row_start..i);
            row_start = i;
            row_w = w;
        } else if i == row_start {
            row_w = w;
        } else {
            row_w += gap.x + w;
        }
    }
    rows.push(row_start..count);

    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = gap.y;
        for row in rows {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = gap.x;
                for i in row {
                    let w = render_item(ui, i).width();
                    if widths[i].is_none_or(|old| (old - w).abs() > 0.5) {
                        ui.ctx().data_mut(|d| d.insert_temp(base_id.with(i), w));
                        needs_repaint = true;
                    }
                }
            });
        }
    });

    if needs_repaint {
        ui.ctx().request_repaint();
    }
}

fn render_section_title(ui: &mut egui::Ui, title: &str, count: usize, stale: bool, scale: f32) {
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(format!("{title} ({count})"))
                .size(9.0 * scale)
                .strong()
                .color(Color32::from_rgb(145, 165, 195)),
        );
        if stale {
            ui.label(
                RichText::new("⚠ Stale")
                    .size(9.0 * scale)
                    .color(Color32::from_rgb(235, 125, 95)),
            );
        }
    });
    ui.add_space(1.5 * scale);
}

fn render_ultra_compact_pills_body(
    ui: &mut egui::Ui,
    sprite_renderer: &mut SpriteRenderer,
    cfg: &OverlayConfig,
    projected: &OverlayProjectedData,
    card_bg: Color32,
    scale: f32,
) {
    // Missions (incl. smart-combined dungeon pills and chains) vs. daily quests.
    let (quest_pills, mission_pills): (Vec<&TaskbarItem>, Vec<&TaskbarItem>) = projected
        .pills
        .iter()
        .partition(|item| matches!(item, TaskbarItem::Single(TaskbarTask::Quest(_))));
    let mission_pills = &mission_pills[..mission_pills.len().min(cfg.max_missions.clamp(1, 60))];
    let quest_pills = &quest_pills[..quest_pills.len().min(cfg.max_quests.clamp(1, 30))];

    if mission_pills.is_empty() && quest_pills.is_empty() {
        ui.label(
            RichText::new("No active pills")
                .size(10.0 * scale)
                .italics()
                .color(Color32::from_rgb(155, 165, 180)),
        );
        return;
    }

    if !mission_pills.is_empty() {
        render_section_title(ui, "MISSIONS", mission_pills.len(), false, scale);
        render_pill_group(
            ui,
            "overlay_mission_pills",
            mission_pills,
            cfg,
            sprite_renderer,
            projected.quests_stale,
            card_bg,
            scale,
        );
    }

    if !mission_pills.is_empty() && !quest_pills.is_empty() {
        ui.add_space(4.5 * scale);
    }

    if !quest_pills.is_empty() {
        render_section_title(
            ui,
            "DAILY QUESTS",
            quest_pills.len(),
            projected.quests_stale,
            scale,
        );
        render_pill_group(
            ui,
            "overlay_quest_pills",
            quest_pills,
            cfg,
            sprite_renderer,
            projected.quests_stale,
            card_bg,
            scale,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn render_pill_group(
    ui: &mut egui::Ui,
    id_salt: &str,
    items: &[&TaskbarItem],
    cfg: &OverlayConfig,
    sprite_renderer: &mut SpriteRenderer,
    quests_stale: bool,
    card_bg: Color32,
    scale: f32,
) {
    match cfg.pill_orientation {
        PillOrientation::Horizontal => {
            wrapped_flow(
                ui,
                id_salt,
                items.len(),
                egui::vec2(4.0 * scale, 3.0 * scale),
                |ui, i| {
                    render_single_compact_pill(
                        ui,
                        sprite_renderer,
                        items[i],
                        quests_stale,
                        card_bg,
                        scale,
                    )
                },
            );
        }
        PillOrientation::Vertical => {
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing = egui::vec2(3.0 * scale, 3.0 * scale);
                for item in items {
                    render_single_compact_pill(
                        ui,
                        sprite_renderer,
                        item,
                        quests_stale,
                        card_bg,
                        scale,
                    );
                }
            });
        }
    }
}

fn render_single_compact_pill(
    ui: &mut egui::Ui,
    sprite_renderer: &mut SpriteRenderer,
    item: &TaskbarItem,
    quests_stale: bool,
    card_bg: Color32,
    scale: f32,
) -> Rect {
    let eligible = item.eligible();
    let border_c = match item {
        TaskbarItem::Single(TaskbarTask::Mission(m)) => {
            let (r, g, b) = m.entry.category.color();
            Color32::from_rgba_unmultiplied(r, g, b, 130)
        }
        TaskbarItem::Single(TaskbarTask::Quest(_)) => {
            if quests_stale {
                Color32::from_rgba_unmultiplied(210, 75, 65, 160)
            } else {
                Color32::from_rgba_unmultiplied(230, 185, 65, 130)
            }
        }
        TaskbarItem::Combined(_) | TaskbarItem::Chain(_) => {
            Color32::from_rgba_unmultiplied(85, 195, 240, 145)
        }
    };

    egui::Frame::NONE
        .fill(card_bg)
        .stroke(Stroke::new(1.0_f32, border_c))
        .corner_radius(CornerRadius::same(4))
        .inner_margin(Margin::symmetric((4.0 * scale) as i8, (2.0 * scale) as i8))
        .show(ui, |ui| {
            if !eligible {
                ui.multiply_opacity(0.55);
            }
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 3.0 * scale;
                match item {
                    TaskbarItem::Single(TaskbarTask::Mission(m)) => {
                        render_mission_task_inline(ui, sprite_renderer, m, scale);
                    }
                    TaskbarItem::Single(TaskbarTask::Quest(q)) => {
                        render_quest_task_inline(ui, sprite_renderer, q, scale);
                    }
                    TaskbarItem::Combined(c) => {
                        render_combined_task_inline(ui, sprite_renderer, c, scale);
                    }
                    TaskbarItem::Chain(ch) => {
                        render_chain_group_inline(ui, sprite_renderer, ch, scale);
                    }
                }
            });
        })
        .response
        .rect
}

fn render_mission_task_inline(
    ui: &mut egui::Ui,
    sprite_renderer: &mut SpriteRenderer,
    m: &MissionTask,
    scale: f32,
) {
    let sep = if m.choice { "|" } else { "+" };
    for (i, obj) in m.objs.iter().take(3).enumerate() {
        if i > 0 {
            ui.label(
                RichText::new(sep)
                    .size(9.5 * scale)
                    .color(Color32::from_rgb(150, 160, 175)),
            );
        }
        if !obj.prefix.is_empty() {
            ui.label(
                RichText::new(&obj.prefix)
                    .size(9.5 * scale)
                    .strong()
                    .color(Color32::from_rgb(215, 225, 238)),
            );
        }
        let sz = 14.5 * scale;
        let (rect, _) = ui.allocate_exact_size(egui::vec2(sz, sz), Sense::hover());
        paint_obj_icon(ui, sprite_renderer, &obj.icon, rect, Color32::WHITE);
        ui.label(
            RichText::new(format!("x{}", obj.remaining))
                .size(9.5 * scale)
                .strong()
                .color(Color32::WHITE),
        );
    }
}

fn render_quest_task_inline(
    ui: &mut egui::Ui,
    sprite_renderer: &mut SpriteRenderer,
    q: &QuestTask,
    scale: f32,
) {
    for (i, m) in q.marks.iter().take(3).enumerate() {
        if i > 0 {
            ui.label(
                RichText::new("+")
                    .size(9.5 * scale)
                    .color(Color32::from_rgb(150, 160, 175)),
            );
        }
        let sz = 14.5 * scale;
        let (rect, _) = ui.allocate_exact_size(egui::vec2(sz, sz), Sense::hover());
        sprite_renderer.draw_outlined_sprite_in_rect_fit_tinted(
            ui,
            m.mark_id,
            rect,
            Color32::WHITE,
        );
        ui.label(
            RichText::new(format!("x{}", m.remaining))
                .size(9.5 * scale)
                .strong()
                .color(Color32::from_rgb(245, 215, 110)),
        );
    }
}

fn render_combined_task_inline(
    ui: &mut egui::Ui,
    sprite_renderer: &mut SpriteRenderer,
    c: &CombinedTask,
    scale: f32,
) {
    for (i, v) in c.variants.iter().take(2).enumerate() {
        if i > 0 {
            ui.label(
                RichText::new("|")
                    .size(9.5 * scale)
                    .color(Color32::from_rgb(150, 160, 175)),
            );
        }
        let sz = 14.5 * scale;
        let (rect, _) = ui.allocate_exact_size(egui::vec2(sz, sz), Sense::hover());
        paint_obj_icon(ui, sprite_renderer, &v.portal_icon, rect, Color32::WHITE);
        ui.label(
            RichText::new(format!("x{}", v.dungeon_runs))
                .size(9.5 * scale)
                .strong()
                .color(Color32::from_rgb(110, 230, 255)),
        );
    }
}

fn render_chain_group_inline(
    ui: &mut egui::Ui,
    sprite_renderer: &mut SpriteRenderer,
    ch: &ChainGroup,
    scale: f32,
) {
    let _ = get_asset_manager();
    if let Some(first) = ch.members.first() {
        match &first.task {
            TaskbarTask::Mission(m) => {
                render_mission_task_inline(ui, sprite_renderer, m, scale);
            }
            TaskbarTask::Quest(q) => {
                render_quest_task_inline(ui, sprite_renderer, q, scale);
            }
        }
        if ch.members.len() > 1 {
            ui.label(
                RichText::new(format!("(+{})", ch.members.len() - 1))
                    .size(9.0 * scale)
                    .color(Color32::from_rgb(140, 210, 245)),
            );
        }
    }
}
