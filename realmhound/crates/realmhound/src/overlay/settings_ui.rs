//! Top-bar button and self-contained Overlay Settings modal dialog.
//!
//! Styled after `ppe_bridge_ui.rs` so it integrates into the top bar with a
//! single call and renders a full-featured `Shadcn` dialog with sidebar
//! navigation and a live preview card.

use eframe::egui::{self, Color32, CornerRadius, Margin, RichText, Stroke};

use super::config::{
    MissionFilterSource, OverlayAnchor, OverlayConfig, OverlayDisplayMode, OverlayQuestSide,
    OverlayTab, PillOrientation, QuestFilterSource, VisibilityTrigger,
};
use super::renderer::{render_overlay_panel_contents, OverlayProjectedData, OverlayRuntimeState};
use super::window_tracker::GameWindowState;
use crate::rendering::sprite_renderer::SpriteRenderer;
use crate::shadcn_ui::Shadcn;
use crate::ui_ext::HoverTooltipExt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SettingsSection {
    #[default]
    GeneralAndTriggers,
    AppearanceAndPosition,
    MissionsAndQuests,
}

#[derive(Default)]
pub struct OverlaySettingsModalState {
    pub open: bool,
    pub active_section: SettingsSection,
    pub show_live_preview: bool,
}

impl OverlaySettingsModalState {
    pub fn new() -> Self {
        Self {
            open: false,
            active_section: SettingsSection::GeneralAndTriggers,
            show_live_preview: true,
        }
    }
}

/// Render the top-bar button (`🎯 Overlay`) next to Settings / PPE Bridge.
/// Right-clicking the button toggles the overlay on/off immediately.
pub fn render_top_bar_button(
    ui: &mut egui::Ui,
    shadcn: &Shadcn,
    icon_only: bool,
    text_only: bool,
    tab_tooltips: bool,
    cfg: &mut OverlayConfig,
    modal: &mut OverlaySettingsModalState,
    rt: &mut OverlayRuntimeState,
    overlay_currently_shown: bool,
) -> bool {
    let status_dot = if overlay_currently_shown {
        "🟢"
    } else if cfg.enabled {
        "🎯"
    } else {
        "🎯"
    };

    let label = if icon_only {
        status_dot.to_string()
    } else if text_only {
        if cfg.enabled {
            "Overlay (On)".to_string()
        } else {
            "Overlay".to_string()
        }
    } else if cfg.enabled {
        format!("{status_dot} Overlay")
    } else {
        "🎯 Overlay".to_string()
    };

    let resp = shadcn.btn(ui, &label);
    if tab_tooltips {
        let tip = if cfg.enabled {
            "RotMG In-Game Overlay (Enabled)\nLeft-click: Open Overlay Settings\nRight-click: Toggle Overlay On/Off"
        } else {
            "RotMG In-Game Overlay (Disabled)\nLeft-click: Open Overlay Settings\nRight-click: Enable Overlay"
        };
        resp.clone().hover_tip(tip);
    }

    let mut cfg_changed = false;
    if resp.clicked() {
        modal.open = true;
    }
    if resp.secondary_clicked() {
        cfg.enabled = !cfg.enabled;
        if !cfg.enabled {
            rt.preview_override = false;
            rt.unlocked_override = false;
        }
        cfg_changed = true;
    }
    cfg_changed
}

/// Render the Overlay Settings modal dialog when `modal.open` is true.
/// Returns `true` if `cfg` was modified and should be persisted to disk.
#[allow(clippy::too_many_arguments)]
pub fn render_settings_modal(
    ctx: &egui::Context,
    shadcn: &Shadcn,
    sprite_renderer: &mut SpriteRenderer,
    cfg: &mut OverlayConfig,
    win_state: &GameWindowState,
    projected: &OverlayProjectedData,
    rt: &mut OverlayRuntimeState,
    modal: &mut OverlaySettingsModalState,
    overlay_currently_shown: bool,
) -> bool {
    if !modal.open {
        return false;
    }

    let mut open = modal.open;
    let mut cfg_changed = false;
    let before = cfg.clone();
    let shadcn = shadcn.clone();

    egui::Area::new(egui::Id::new("rotmg_overlay_dialog_host"))
        .fixed_pos(egui::pos2(0.0, 0.0))
        .show(ctx, |ui| {
            shadcn.dialog(
                ui,
                "rotmg_overlay_settings_dialog",
                &mut open,
                "RotMG In-Game Overlay",
                840.0,
                600.0,
                |ui| {
                    let sidebar_width = 205.0;

                    ui.horizontal_top(|ui| {
                        // --- Left Navigation Sidebar ---
                ui.allocate_ui_with_layout(
                    egui::vec2(sidebar_width, ui.available_height()),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        ui.set_width(sidebar_width);

                        if shadcn
                            .nav_item(
                                ui,
                                "🎯 General & Triggers",
                                modal.active_section == SettingsSection::GeneralAndTriggers,
                            )
                            .clicked()
                        {
                            modal.active_section = SettingsSection::GeneralAndTriggers;
                        }
                        ui.add_space(4.0);

                        if shadcn
                            .nav_item(
                                ui,
                                "🎨 Appearance & Position",
                                modal.active_section == SettingsSection::AppearanceAndPosition,
                            )
                            .clicked()
                        {
                            modal.active_section = SettingsSection::AppearanceAndPosition;
                        }
                        ui.add_space(4.0);

                        if shadcn
                            .nav_item(
                                ui,
                                "📜 Missions & Quests",
                                modal.active_section == SettingsSection::MissionsAndQuests,
                            )
                            .clicked()
                        {
                            modal.active_section = SettingsSection::MissionsAndQuests;
                        }

                        ui.add_space(12.0);
                        ui.separator();
                        ui.add_space(8.0);

                        // Quick Actions in Sidebar
                        ui.label(
                            RichText::new("QUICK CONTROLS")
                                .size(10.5)
                                .strong()
                                .color(shadcn.colors().muted_foreground),
                        );
                        ui.add_space(4.0);

                        let preview_lbl = if rt.preview_override {
                            "👁 Stop Force-Preview"
                        } else {
                            "👁 Force-Show Overlay Now"
                        };
                        if shadcn.button_sm(ui, preview_lbl).clicked() {
                            rt.preview_override = !rt.preview_override;
                        }
                        ui.add_space(4.0);

                        let unlock_lbl = if rt.unlocked_override || !cfg.click_through {
                            "🔒 Lock Overlay (Click-Through)"
                        } else {
                            "✥ Unlock to Drag Overlay"
                        };
                        if shadcn.button_sm(ui, unlock_lbl).clicked() {
                            if rt.unlocked_override || !cfg.click_through {
                                rt.unlocked_override = false;
                                cfg.click_through = true;
                            } else {
                                rt.unlocked_override = true;
                                rt.preview_override = true;
                            }
                        }

                        ui.add_space(12.0);
                        ui.separator();
                        ui.add_space(8.0);

                        // Live Status Card
                        let (status_txt, status_col) = if overlay_currently_shown {
                            ("Overlay Active", Color32::from_rgb(75, 220, 130))
                        } else if cfg.enabled {
                            ("Standby (Waiting for Game)", Color32::from_rgb(235, 190, 75))
                        } else {
                            ("Overlay Disabled", Color32::from_rgb(155, 160, 170))
                        };

                        ui.horizontal(|ui| {
                            let (r, _) =
                                ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
                            ui.painter().circle_filled(r.center(), 4.0, status_col);
                            ui.label(RichText::new(status_txt).color(status_col).strong());
                        });

                        ui.add_space(4.0);
                        let game_str = if win_state.rotmg_focused {
                            "RotMG: Running (Focused)"
                        } else if win_state.rotmg_running {
                            "RotMG: Running (Background)"
                        } else {
                            "RotMG: Not Detected"
                        };
                        ui.label(
                            RichText::new(game_str)
                                .size(11.5)
                                .color(shadcn.colors().muted_foreground),
                        );

                        let map_str = if win_state.current_map_name.is_empty() {
                            format!("Area: {}", win_state.area_kind.label())
                        } else {
                            format!(
                                "Map: {} ({})",
                                win_state.current_map_name,
                                win_state.area_kind.label()
                            )
                        };
                        ui.label(
                            RichText::new(map_str)
                                .size(11.5)
                                .color(shadcn.colors().muted_foreground),
                        );
                        ui.label(
                            RichText::new(format!(
                                "Showing: {} Missions, {} Quests",
                                projected.missions.len(),
                                projected.quests.len()
                            ))
                            .size(11.5)
                            .color(shadcn.colors().muted_foreground),
                        );
                        ui.add_space(4.0);
                        ui.label(
                            RichText::new(win_state.backend_status)
                                .size(10.5)
                                .color(shadcn.colors().muted_foreground),
                        );
                    },
                );

                ui.separator();

                // --- Right Content Pane ---
                ui.allocate_ui_with_layout(
                    ui.available_size(),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        egui::ScrollArea::vertical()
                            .id_salt("overlay_settings_content_scroll")
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                match modal.active_section {
                                    SettingsSection::GeneralAndTriggers => {
                                        render_general_section(ui, &shadcn, cfg, rt);
                                    }
                                    SettingsSection::AppearanceAndPosition => {
                                        render_appearance_section(ui, &shadcn, cfg, rt);
                                    }
                                    SettingsSection::MissionsAndQuests => {
                                        render_missions_quests_section(ui, &shadcn, cfg);
                                    }
                                }

                                ui.add_space(14.0);
                                ui.separator();
                                ui.add_space(8.0);

                                // Inline Live Preview inside the modal
                                ui.horizontal(|ui| {
                                    ui.checkbox(
                                        &mut modal.show_live_preview,
                                        RichText::new("Show Live Preview Card").strong(),
                                    );
                                    ui.label(
                                        RichText::new(
                                            "(reflects your current opacity, scale, and mission/quest filters)",
                                        )
                                        .size(11.5)
                                        .color(shadcn.colors().muted_foreground),
                                    );
                                });

                                if modal.show_live_preview {
                                    ui.add_space(6.0);
                                    render_inline_preview_box(
                                        ui,
                                        sprite_renderer,
                                        cfg,
                                        win_state,
                                        projected,
                                    );
                                }
                            });
                    },
                );
            });
                },
            );
        });

    modal.open = open;
    cfg.clamp_values();
    if *cfg != before {
        cfg_changed = true;
    }
    cfg_changed
}

fn render_general_section(
    ui: &mut egui::Ui,
    shadcn: &Shadcn,
    cfg: &mut OverlayConfig,
    rt: &mut OverlayRuntimeState,
) {
    ui.heading("General & Automatic Visibility");
    ui.label(
        RichText::new(
            "Configure when the translucent RotMG overlay automatically pops up and how it interacts with the game.",
        )
        .color(shadcn.colors().muted_foreground),
    );
    ui.add_space(10.0);

    egui::Frame::NONE
        .fill(shadcn.colors().secondary)
        .stroke(Stroke::new(1.0_f32, shadcn.colors().border))
        .corner_radius(CornerRadius::same(6))
        .inner_margin(Margin::symmetric(12, 10))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.checkbox(
                    &mut cfg.enabled,
                    RichText::new("Enable RotMG In-Game Overlay")
                        .size(14.0)
                        .strong(),
                );
            });
            ui.label(
                RichText::new(
                    "Tip: You can also right-click the 🎯 Overlay button in the top bar at any time to toggle the overlay on or off.",
                )
                .size(11.5)
                .color(shadcn.colors().muted_foreground),
            );
        });

    ui.add_space(12.0);
    ui.label(RichText::new("Automatic Popup Trigger").size(13.5).strong());
    ui.add_space(4.0);

    for trig in VisibilityTrigger::ALL {
        let selected = cfg.trigger == trig;
        if ui
            .radio(selected, RichText::new(trig.label()).strong())
            .clicked()
        {
            cfg.trigger = trig;
        }
        ui.indent(trig.label(), |ui| {
            ui.label(
                RichText::new(trig.description())
                    .size(11.5)
                    .color(shadcn.colors().muted_foreground),
            );
        });
        ui.add_space(4.0);
    }

    ui.add_space(10.0);
    ui.label(RichText::new("In-Game Area Filters").size(13.5).strong());
    ui.label(
        RichText::new(
            "Choose which in-game zones should display the overlay (automatically detected from live map changes).",
        )
        .size(11.5)
        .color(shadcn.colors().muted_foreground),
    );
    ui.add_space(4.0);

    ui.checkbox(
        &mut cfg.show_in_safe_areas,
        "Show in Safe Areas (Nexus, Vault, Tinkerer / Daily Quest Room, Pet Yard, Guild Hall)",
    );
    ui.checkbox(
        &mut cfg.show_in_realm,
        "Show in Realm Overworld (Realm of the Mad God)",
    );
    ui.checkbox(
        &mut cfg.show_in_dungeons,
        "Show inside Dungeons & Boss Encounters",
    );
    ui.checkbox(
        &mut cfg.hide_when_empty,
        "Auto-hide overlay completely when 0 missions and 0 quests match filters",
    );

    ui.add_space(12.0);
    ui.label(
        RichText::new("Scrolling, Controls & Fullscreen Behavior")
            .size(13.5)
            .strong(),
    );
    ui.add_space(4.0);
    if ui
        .checkbox(
            &mut cfg.click_through,
            "Click-through overlay body (disable mouse-wheel scrolling over cards)",
        )
        .changed()
        && cfg.click_through
    {
        rt.unlocked_override = false;
    }
    ui.label(
        RichText::new(
            "• By default (unchecked), you can scroll the mission/quest list with the mouse wheel and click [—] (minimize) or [✕] (close) without ever stealing focus from RotMG.\n\
             • Even if you enable body click-through, the top header bar ([—] and [✕] buttons) always remains active.\n\
             • Fullscreen / Borderless Tip: In RotMG Options → Video, set Display Mode to 'Borderless' (or 'Borderless Windowed'). In exclusive 'Fullscreen' mode, the OS bypasses desktop window composition, preventing any external overlay from rendering over the game. 'Borderless' mode fills your entire screen with zero borders while keeping the overlay visible and pinned on top.",
        )
        .size(11.5)
        .color(shadcn.colors().muted_foreground),
    );
}

fn render_appearance_section(
    ui: &mut egui::Ui,
    shadcn: &Shadcn,
    cfg: &mut OverlayConfig,
    rt: &mut OverlayRuntimeState,
) {
    ui.heading("Appearance, Translucency & Position");
    ui.label(
        RichText::new(
            "Customize the overlay's layout mode, translucency, size, and screen anchor.",
        )
        .color(shadcn.colors().muted_foreground),
    );
    ui.add_space(10.0);

    egui::Grid::new("overlay_appearance_grid")
        .num_columns(2)
        .spacing([18.0, 10.0])
        .show(ui, |ui| {
            ui.label(RichText::new("Default Active Tab").strong());
            ui.horizontal(|ui| {
                for tab in OverlayTab::ALL {
                    if ui
                        .selectable_label(cfg.active_tab == tab, tab.label())
                        .clicked()
                    {
                        cfg.active_tab = tab;
                    }
                }
            });
            ui.end_row();

            ui.label(RichText::new("Display Style").strong());
            ui.horizontal(|ui| {
                for mode in OverlayDisplayMode::ALL {
                    if ui
                        .selectable_label(cfg.display_mode == mode, mode.label())
                        .clicked()
                    {
                        cfg.display_mode = mode;
                    }
                }
            });
            ui.end_row();

            if cfg.display_mode == OverlayDisplayMode::UltraCompactPills {
                ui.label(RichText::new("Pill Strip Direction").strong());
                ui.horizontal(|ui| {
                    for dir in [PillOrientation::Horizontal, PillOrientation::Vertical] {
                        if ui
                            .selectable_label(cfg.pill_orientation == dir, dir.label())
                            .clicked()
                        {
                            cfg.pill_orientation = dir;
                        }
                    }
                });
                ui.end_row();
            }

            ui.label(RichText::new("Screen / Window Anchor").strong());
            ui.horizontal_wrapped(|ui| {
                for anchor in OverlayAnchor::ALL {
                    if ui
                        .selectable_label(cfg.anchor == anchor, anchor.label())
                        .clicked()
                    {
                        cfg.anchor = anchor;
                        rt.last_applied_pos = None;
                    }
                }
            });
            ui.end_row();

            if cfg.anchor == OverlayAnchor::Custom {
                ui.label(RichText::new("Custom Position (X, Y)").strong());
                ui.horizontal(|ui| {
                    ui.label("X:");
                    ui.add(egui::DragValue::new(&mut cfg.custom_x).speed(2.0));
                    ui.add_space(8.0);
                    ui.label("Y:");
                    ui.add(egui::DragValue::new(&mut cfg.custom_y).speed(2.0));
                });
                ui.end_row();
            } else {
                ui.label(RichText::new("Corner Offset (X, Y)").strong());
                ui.horizontal(|ui| {
                    ui.label("X px:");
                    ui.add(
                        egui::Slider::new(&mut cfg.offset_x, 0.0..=400.0)
                            .show_value(true)
                            .step_by(1.0),
                    );
                    ui.label("Y px:");
                    ui.add(
                        egui::Slider::new(&mut cfg.offset_y, 0.0..=400.0)
                            .show_value(true)
                            .step_by(1.0),
                    );
                });
                ui.end_row();
            }

            ui.label(RichText::new("Background Opacity").strong());
            ui.add(
                egui::Slider::new(&mut cfg.bg_opacity_pct, 10..=98)
                    .suffix("%")
                    .text("Glass backdrop alpha"),
            );
            ui.end_row();

            ui.label(RichText::new("Content Opacity").strong());
            ui.add(
                egui::Slider::new(&mut cfg.content_opacity_pct, 35..=100)
                    .suffix("%")
                    .text("Icons, bars & text"),
            );
            ui.end_row();

            ui.label(RichText::new("Overlay Scale").strong());
            ui.add(
                egui::Slider::new(&mut cfg.scale_pct, 65..=150)
                    .suffix("%")
                    .text("UI zoom"),
            );
            ui.end_row();

            if cfg.display_mode == OverlayDisplayMode::CompactCards {
                ui.label(RichText::new("Panel Width").strong());
                ui.add(
                    egui::Slider::new(&mut cfg.card_width, 200.0..=520.0)
                        .suffix(" px")
                        .step_by(5.0),
                );
                ui.end_row();
            }

            ui.label(RichText::new("Max Scroll Height").strong());
            ui.add(
                egui::Slider::new(&mut cfg.max_height, 140.0..=900.0)
                    .suffix(" px")
                    .step_by(10.0)
                    .text("Scrolls when taller"),
            );
            ui.end_row();

            ui.label(RichText::new("Chrome Elements").strong());
            ui.horizontal(|ui| {
                ui.checkbox(&mut cfg.show_header, "Show Top Status Header");
                ui.add_space(8.0);
                ui.checkbox(&mut cfg.show_border, "Show Translucent Border");
            });
            ui.end_row();
        });

    ui.add_space(12.0);
    if shadcn
        .button_sm(ui, "↺ Reset Appearance & Position to Defaults")
        .clicked()
    {
        let def = OverlayConfig::default();
        cfg.display_mode = def.display_mode;
        cfg.pill_orientation = def.pill_orientation;
        cfg.anchor = def.anchor;
        cfg.offset_x = def.offset_x;
        cfg.offset_y = def.offset_y;
        cfg.bg_opacity_pct = def.bg_opacity_pct;
        cfg.content_opacity_pct = def.content_opacity_pct;
        cfg.scale_pct = def.scale_pct;
        cfg.card_width = def.card_width;
        cfg.max_height = def.max_height;
        cfg.show_border = def.show_border;
        cfg.show_header = def.show_header;
        rt.last_applied_pos = None;
    }
}

fn render_missions_quests_section(ui: &mut egui::Ui, shadcn: &Shadcn, cfg: &mut OverlayConfig) {
    ui.heading("Missions & Daily Quests Content");
    ui.label(
        RichText::new(
            "Choose which Seasonal/Crucible/Regular Missions and Tinkerer Daily Quests appear on the overlay.",
        )
        .color(shadcn.colors().muted_foreground),
    );
    ui.add_space(10.0);

    // --- Missions Card ---
    egui::Frame::NONE
        .fill(shadcn.colors().secondary)
        .stroke(Stroke::new(1.0_f32, shadcn.colors().border))
        .corner_radius(CornerRadius::same(6))
        .inner_margin(Margin::symmetric(12, 10))
        .show(ui, |ui| {
            ui.checkbox(
                &mut cfg.show_missions,
                RichText::new("Show Missions on Overlay")
                    .size(13.5)
                    .strong(),
            );
            if cfg.show_missions {
                ui.add_space(6.0);
                egui::Grid::new("overlay_missions_cfg_grid")
                    .num_columns(2)
                    .spacing([16.0, 8.0])
                    .show(ui, |ui| {
                        ui.label(RichText::new("Mission Source").strong());
                        egui::ComboBox::from_id_salt("overlay_mission_source_combo")
                            .selected_text(cfg.mission_source.label())
                            .width(280.0)
                            .show_ui(ui, |ui| {
                                for src in MissionFilterSource::ALL {
                                    ui.selectable_value(
                                        &mut cfg.mission_source,
                                        src,
                                        src.label(),
                                    );
                                }
                            });
                        ui.end_row();

                        ui.label(RichText::new("Max Mission Cards").strong());
                        ui.add(egui::Slider::new(&mut cfg.max_missions, 1..=60));
                        ui.end_row();
                    });

                ui.add_space(4.0);
                ui.checkbox(
                    &mut cfg.only_eligible_for_current_char,
                    "Only show missions eligible for my current live character (Seasonal/Crucible/gear check)",
                );
                ui.checkbox(
                    &mut cfg.show_claimable_missions,
                    "Include completed missions that are ready to claim (CLAIM badge)",
                );
                ui.checkbox(&mut cfg.show_mission_names, "Show mission titles on cards");
                ui.checkbox(
                    &mut cfg.show_mission_rewards,
                    "Show BXP & item reward icons on mission cards",
                );
                ui.checkbox(
                    &mut cfg.show_all_choice_options,
                    "Expand all OR-choice objectives instead of collapsing to the most-progressed option",
                );
            }
        });

    ui.add_space(12.0);

    // --- Daily Quests Card ---
    egui::Frame::NONE
        .fill(shadcn.colors().secondary)
        .stroke(Stroke::new(1.0_f32, shadcn.colors().border))
        .corner_radius(CornerRadius::same(6))
        .inner_margin(Margin::symmetric(12, 10))
        .show(ui, |ui| {
            ui.checkbox(
                &mut cfg.show_quests,
                RichText::new("Show Daily Quests on Overlay")
                    .size(13.5)
                    .strong(),
            );
            if cfg.show_quests {
                ui.add_space(6.0);
                egui::Grid::new("overlay_quests_cfg_grid")
                    .num_columns(2)
                    .spacing([16.0, 8.0])
                    .show(ui, |ui| {
                        ui.label(RichText::new("Quest Filter").strong());
                        egui::ComboBox::from_id_salt("overlay_quest_source_combo")
                            .selected_text(cfg.quest_source.label())
                            .width(280.0)
                            .show_ui(ui, |ui| {
                                for src in QuestFilterSource::ALL {
                                    ui.selectable_value(&mut cfg.quest_source, src, src.label());
                                }
                            });
                        ui.end_row();

                        ui.label(RichText::new("Mark Storage Side").strong());
                        egui::ComboBox::from_id_salt("overlay_quest_side_combo")
                            .selected_text(cfg.quest_side.label())
                            .width(280.0)
                            .show_ui(ui, |ui| {
                                for side in OverlayQuestSide::ALL {
                                    ui.selectable_value(&mut cfg.quest_side, side, side.label());
                                }
                            });
                        ui.end_row();

                        ui.label(RichText::new("Max Quest Rows").strong());
                        ui.add(egui::Slider::new(&mut cfg.max_quests, 1..=30));
                        ui.end_row();
                    });

                ui.add_space(4.0);
                ui.checkbox(
                    &mut cfg.highlight_completable_quests,
                    "Highlight quests with a green READY badge when all marks are collected",
                );
                ui.checkbox(
                    &mut cfg.only_quests_with_progress,
                    "Only show quests where I own at least 1 required mark",
                );
                ui.checkbox(
                    &mut cfg.show_quest_names,
                    "Show quest titles above mark icons",
                );
                ui.checkbox(
                    &mut cfg.show_quest_rewards,
                    "Show quest chest / reward icon on each row",
                );
            }
        });
}

fn render_inline_preview_box(
    ui: &mut egui::Ui,
    sprite_renderer: &mut SpriteRenderer,
    cfg: &mut OverlayConfig,
    win_state: &GameWindowState,
    projected: &OverlayProjectedData,
) {
    // Draw a subtle simulated dark game-backdrop container so translucency is visible
    let backdrop = Color32::from_rgb(28, 34, 44);
    egui::Frame::NONE
        .fill(backdrop)
        .stroke(Stroke::new(1.0_f32, Color32::from_rgb(60, 72, 92)))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::symmetric(14, 12))
        .show(ui, |ui| {
            let mut dummy_rt = OverlayRuntimeState::default();
            let mut dummy_changed = false;
            render_overlay_panel_contents(
                ui,
                sprite_renderer,
                cfg,
                win_state,
                projected,
                &mut dummy_rt,
                &mut dummy_changed,
                true,
            );
        });
}
