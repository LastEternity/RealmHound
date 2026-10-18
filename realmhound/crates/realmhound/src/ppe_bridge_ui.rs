//! Pull-up modal UI for the RotMG PPE Discord Bot Bridge (`ppe_bridge_ui`).
//!
//! Integrates Bridge Review (Sent/Unsent UT/ST table), Bridge Log (filterable
//! console), and PPE Bot Configuration into a single pull-up dialog accessible
//! from the top header bar, rather than cluttering the main tab bar.

use std::sync::{Mutex, OnceLock};

use eframe::egui::{self, Color32, RichText};
use realmhound_core::ppe_bridge::{
    self, BridgeLogEntry, BridgeReviewEntry, BridgeReviewStatus, PpeBridgeConfig, PpeBridgeSnapshot,
};

use crate::shadcn_ui::Shadcn;
use crate::ui_ext::HoverTooltipExt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BridgeSection {
    Review,
    Log,
    Config,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReviewFilter {
    All,
    SentOnly,
    UnsentOnly,
}

impl ReviewFilter {
    fn label(self) -> &'static str {
        match self {
            ReviewFilter::All => "All",
            ReviewFilter::SentOnly => "Sent Only",
            ReviewFilter::UnsentOnly => "Unsent Only",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LogLevelFilter {
    All,
    Info,
    Warn,
    Debug,
    Error,
}

impl LogLevelFilter {
    fn label(self) -> &'static str {
        match self {
            LogLevelFilter::All => "All",
            LogLevelFilter::Info => "INFO",
            LogLevelFilter::Warn => "WARN",
            LogLevelFilter::Debug => "DEBUG",
            LogLevelFilter::Error => "ERROR",
        }
    }

    fn matches(self, level: &str) -> bool {
        match self {
            LogLevelFilter::All => true,
            LogLevelFilter::Info => level.eq_ignore_ascii_case("INFO"),
            LogLevelFilter::Warn => level.eq_ignore_ascii_case("WARN"),
            LogLevelFilter::Debug => level.eq_ignore_ascii_case("DEBUG"),
            LogLevelFilter::Error => level.eq_ignore_ascii_case("ERROR"),
        }
    }
}

struct BridgeUiState {
    open: bool,
    active_section: BridgeSection,
    // Review view state
    review_filter: ReviewFilter,
    review_search: String,
    // Log view state
    log_level_filter: LogLevelFilter,
    log_search: String,
    log_auto_scroll: bool,
    // Config draft state
    draft_config: PpeBridgeConfig,
    draft_initialized: bool,
    show_link_token: bool,
    status_banner: Option<(String, bool)>,
}

impl Default for BridgeUiState {
    fn default() -> Self {
        Self {
            open: false,
            active_section: BridgeSection::Review,
            review_filter: ReviewFilter::All,
            review_search: String::new(),
            log_level_filter: LogLevelFilter::All,
            log_search: String::new(),
            log_auto_scroll: true,
            draft_config: PpeBridgeConfig::default(),
            draft_initialized: false,
            show_link_token: false,
            status_banner: None,
        }
    }
}

static UI_STATE: OnceLock<Mutex<BridgeUiState>> = OnceLock::new();

fn ui_state_lock() -> &'static Mutex<BridgeUiState> {
    UI_STATE.get_or_init(|| Mutex::new(BridgeUiState::default()))
}

/// Returns true if the PPE Bot Bridge modal is currently open.
pub fn is_open() -> bool {
    ui_state_lock().lock().map(|s| s.open).unwrap_or(false)
}

/// Render the top-bar button that pulls up the PPE Bot Bridge modal.
pub fn render_top_bar_button(
    ui: &mut egui::Ui,
    shadcn: &Shadcn,
    icon_only: bool,
    text_only: bool,
    tab_tooltips: bool,
) {
    let cfg = ppe_bridge::get_config();
    let label = if icon_only {
        "🤖".to_string()
    } else if text_only {
        "PPE Bot".to_string()
    } else {
        "🤖 PPE Bot".to_string()
    };

    let resp = shadcn.btn(ui, label);
    let tip = if cfg.enabled {
        "PPE Bot Bridge (Enabled) — Click to open Review, Log & Settings"
    } else {
        "PPE Bot Bridge (Disabled) — Click to open Review, Log & Settings"
    };
    if icon_only && tab_tooltips {
        resp.clone().hover_tip(tip);
    } else {
        resp.clone().hover_tip(tip);
    }

    if resp.clicked() {
        if let Ok(mut st) = ui_state_lock().lock() {
            st.open = !st.open;
            if st.open {
                st.draft_config = ppe_bridge::get_config();
                st.draft_initialized = true;
                st.status_banner = None;
            }
        }
    }
}

/// Render the PPE Bot Bridge modal if open.
pub fn render_modal(ctx: &egui::Context, shadcn: &Shadcn) {
    let mut open = is_open();
    if !open {
        return;
    }

    let snap = ppe_bridge::snapshot();
    let shadcn = shadcn.clone();

    egui::Area::new(egui::Id::new("ppe_bridge_dialog_host"))
        .fixed_pos(egui::pos2(0.0, 0.0))
        .show(ctx, |ui| {
            shadcn.dialog(
                ui,
                "ppe_bridge_modal",
                &mut open,
                "RotMG PPE Bot Bridge",
                980.0,
                560.0,
                |ui| {
                    let Ok(mut st) = ui_state_lock().lock() else {
                        return;
                    };
                    if !st.draft_initialized {
                        st.draft_config = snap.config.clone();
                        st.draft_initialized = true;
                    }

                    ui.horizontal_top(|ui| {
                        // Left sidebar navigation
                        let sidebar_width = 168.0;
                        ui.allocate_ui_with_layout(
                            egui::vec2(sidebar_width, ui.available_height()),
                            egui::Layout::top_down(egui::Align::Min),
                            |ui| {
                                ui.set_width(sidebar_width);

                                let review_label =
                                    format!("📋 Bridge Review ({})", snap.review_entries.len());
                                if shadcn
                                    .nav_item(
                                        ui,
                                        &review_label,
                                        st.active_section == BridgeSection::Review,
                                    )
                                    .clicked()
                                {
                                    st.active_section = BridgeSection::Review;
                                }
                                ui.add_space(4.0);

                                let log_label =
                                    format!("📜 Bridge Log ({})", snap.log_entries.len());
                                if shadcn
                                    .nav_item(
                                        ui,
                                        &log_label,
                                        st.active_section == BridgeSection::Log,
                                    )
                                    .clicked()
                                {
                                    st.active_section = BridgeSection::Log;
                                }
                                ui.add_space(4.0);

                                if shadcn
                                    .nav_item(
                                        ui,
                                        "⚙ Bot Config",
                                        st.active_section == BridgeSection::Config,
                                    )
                                    .clicked()
                                {
                                    st.active_section = BridgeSection::Config;
                                    st.draft_config = snap.config.clone();
                                    st.status_banner = None;
                                }

                                ui.add_space(16.0);
                                ui.separator();
                                ui.add_space(8.0);

                                // Live status summary card in sidebar
                                let (status_text, status_color) = if snap.config.enabled {
                                    ("Bridge Enabled", Color32::from_rgb(80, 210, 120))
                                } else {
                                    ("Bridge Disabled", Color32::from_rgb(160, 160, 170))
                                };
                                ui.horizontal(|ui| {
                                    let (rect, _) = ui.allocate_exact_size(
                                        egui::vec2(8.0, 8.0),
                                        egui::Sense::hover(),
                                    );
                                    ui.painter().circle_filled(rect.center(), 4.0, status_color);
                                    ui.label(
                                        RichText::new(status_text).color(status_color).strong(),
                                    );
                                });
                                ui.add_space(4.0);
                                ui.label(
                                    RichText::new(format!(
                                        "Tracked CSV: {} items",
                                        snap.tracked_item_count
                                    ))
                                    .size(11.5)
                                    .color(shadcn.colors().muted_foreground),
                                );
                                ui.label(
                                    RichText::new(format!(
                                        "Sent: {}  |  Unsent: {}",
                                        snap.sent_count, snap.unsent_count
                                    ))
                                    .size(11.5)
                                    .color(shadcn.colors().muted_foreground),
                                );
                            },
                        );

                        ui.separator();

                        // Right content pane
                        ui.allocate_ui_with_layout(
                            ui.available_size(),
                            egui::Layout::top_down(egui::Align::Min),
                            |ui| match st.active_section {
                                BridgeSection::Review => {
                                    render_review_section(ui, &shadcn, &mut st, &snap);
                                }
                                BridgeSection::Log => {
                                    render_log_section(ui, &shadcn, &mut st, &snap);
                                }
                                BridgeSection::Config => {
                                    render_config_section(ui, &shadcn, &mut st, &snap);
                                }
                            },
                        );
                    });
                },
            );
        });

    if let Ok(mut st) = ui_state_lock().lock() {
        st.open = open;
    }
}

fn render_review_section(
    ui: &mut egui::Ui,
    shadcn: &Shadcn,
    st: &mut BridgeUiState,
    snap: &PpeBridgeSnapshot,
) {
    let filtered: Vec<&BridgeReviewEntry> = snap
        .review_entries
        .iter()
        .filter(|e| match st.review_filter {
            ReviewFilter::All => true,
            ReviewFilter::SentOnly => e.status == BridgeReviewStatus::Sent,
            ReviewFilter::UnsentOnly => e.status == BridgeReviewStatus::Unsent,
        })
        .filter(|e| {
            let needle = st.review_search.trim().to_ascii_lowercase();
            if needle.is_empty() {
                return true;
            }
            let hay = format!(
                "{} {} {} {} {} {} {} {} {} {} {}",
                e.status.as_str(),
                e.item_name,
                e.item_id,
                e.rarity,
                e.rarity_source,
                e.character_name,
                e.character_class,
                e.group,
                e.label,
                e.dungeon,
                e.reason
            )
            .to_ascii_lowercase();
            hay.contains(&needle)
        })
        .collect();

    // Controls toolbar
    ui.horizontal(|ui| {
        ui.label("Status:");
        for mode in [
            ReviewFilter::All,
            ReviewFilter::SentOnly,
            ReviewFilter::UnsentOnly,
        ] {
            let selected = st.review_filter == mode;
            let btn = if selected {
                egui::Button::new(RichText::new(mode.label()).strong())
                    .fill(shadcn.colors().secondary)
            } else {
                egui::Button::new(mode.label())
            };
            if ui.add(btn).clicked() {
                st.review_filter = mode;
            }
        }

        ui.add_space(8.0);
        ui.label("Search:");
        ui.add(
            egui::TextEdit::singleline(&mut st.review_search)
                .desired_width(180.0)
                .hint_text("Filter item, rarity, dungeon..."),
        );

        if shadcn.button_sm(ui, "🔄 Refresh CSV").clicked() {
            match ppe_bridge::request_remote_csv_refresh() {
                Ok(msg) => st.status_banner = Some((msg, false)),
                Err(err) => st.status_banner = Some((err, true)),
            }
        }
        if shadcn.button_sm(ui, "Copy Filtered").clicked() {
            let tsv = format_review_entries_tsv(&filtered);
            ui.ctx().copy_text(tsv);
        }
        if shadcn.button_sm(ui, "Clear").clicked() {
            ppe_bridge::clear_review_entries();
        }
    });

    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(format!(
                "Sent: {}   |   Unsent: {}   |   Showing: {}   |   CSV: {} ({} items)",
                snap.sent_count,
                snap.unsent_count,
                filtered.len(),
                snap.loaded_csv_source,
                snap.tracked_item_count
            ))
            .size(12.0)
            .color(shadcn.colors().muted_foreground),
        );
        if let Some((ref msg, is_error)) = st.status_banner {
            let color = if is_error {
                Color32::from_rgb(240, 95, 95)
            } else {
                Color32::from_rgb(80, 210, 120)
            };
            ui.label(RichText::new(format!("• {msg}")).size(12.0).color(color));
        }
    });
    ui.separator();

    // Table header + scrollable rows
    egui::ScrollArea::both()
        .id_salt("ppe_bridge_review_scroll")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            egui::Grid::new("ppe_bridge_review_grid")
                .striped(true)
                .spacing([10.0, 4.0])
                .min_col_width(40.0)
                .show(ui, |ui| {
                    let headers = [
                        "Time", "Status", "Item", "ID", "Shiny", "Rarity", "Source", "Divine",
                        "Ench", "Char", "Class", "Dungeon", "Reason",
                    ];
                    for h in headers {
                        ui.label(RichText::new(h).strong());
                    }
                    ui.end_row();

                    for e in filtered.iter().rev() {
                        ui.label(&e.timestamp);
                        let (status_str, status_col) = match e.status {
                            BridgeReviewStatus::Sent => ("SENT", Color32::from_rgb(80, 210, 120)),
                            BridgeReviewStatus::Unsent => {
                                ("UNSENT", Color32::from_rgb(245, 180, 70))
                            }
                        };
                        ui.label(RichText::new(status_str).color(status_col).strong());
                        ui.label(&e.item_name);
                        ui.label(e.item_id.to_string());
                        ui.label(if e.shiny { "yes" } else { "no" });
                        ui.label(&e.rarity);
                        ui.label(&e.rarity_source);
                        ui.label(if e.divine { "yes" } else { "no" });
                        ui.label(e.enchant_count.to_string());
                        let char_display = if !e.character_name.is_empty() {
                            format!("{} (#{})", e.character_name, e.character_id)
                        } else if e.character_id > 0 {
                            format!("#{}", e.character_id)
                        } else {
                            "-".to_string()
                        };
                        ui.label(char_display);
                        ui.label(if e.character_class.is_empty() {
                            "-"
                        } else {
                            &e.character_class
                        });
                        ui.label(if e.dungeon.is_empty() {
                            "-"
                        } else {
                            &e.dungeon
                        });
                        ui.label(&e.reason);
                        ui.end_row();
                    }
                });
        });
}

fn format_review_entries_tsv(entries: &[&BridgeReviewEntry]) -> String {
    let mut out = String::from(
        "Time\tStatus\tItem\tID\tShiny\tRarity\tRarity Source\tDivine\tEnchants\tChar ID\tCharacter\tClass\tGroup\tLabel\tDungeon\tReason\tNormalized\n",
    );
    for e in entries {
        out.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
            e.timestamp,
            e.status.as_str(),
            e.item_name,
            e.item_id,
            if e.shiny { "yes" } else { "no" },
            e.rarity,
            e.rarity_source,
            if e.divine { "yes" } else { "no" },
            e.enchant_count,
            if e.character_id > 0 {
                e.character_id.to_string()
            } else {
                String::new()
            },
            e.character_name,
            e.character_class,
            e.group,
            e.label,
            e.dungeon,
            e.reason,
            e.normalized_name
        ));
    }
    out
}

fn render_log_section(
    ui: &mut egui::Ui,
    shadcn: &Shadcn,
    st: &mut BridgeUiState,
    snap: &PpeBridgeSnapshot,
) {
    let filtered: Vec<&BridgeLogEntry> = snap
        .log_entries
        .iter()
        .filter(|e| st.log_level_filter.matches(&e.level))
        .filter(|e| {
            let needle = st.log_search.trim().to_ascii_lowercase();
            if needle.is_empty() {
                return true;
            }
            e.formatted_line().to_ascii_lowercase().contains(&needle)
        })
        .collect();

    ui.horizontal(|ui| {
        ui.label("Level:");
        for lvl in [
            LogLevelFilter::All,
            LogLevelFilter::Info,
            LogLevelFilter::Warn,
            LogLevelFilter::Debug,
            LogLevelFilter::Error,
        ] {
            let selected = st.log_level_filter == lvl;
            let btn = if selected {
                egui::Button::new(RichText::new(lvl.label()).strong())
                    .fill(shadcn.colors().secondary)
            } else {
                egui::Button::new(lvl.label())
            };
            if ui.add(btn).clicked() {
                st.log_level_filter = lvl;
            }
        }

        ui.add_space(6.0);
        ui.label("Search:");
        ui.add(
            egui::TextEdit::singleline(&mut st.log_search)
                .desired_width(150.0)
                .hint_text("Search logs..."),
        );

        let mut debug_on = snap.config.debug;
        if ui.checkbox(&mut debug_on, "Debug").changed() {
            ppe_bridge::set_debug_enabled(debug_on);
            st.draft_config.debug = debug_on;
        }

        ui.checkbox(&mut st.log_auto_scroll, "Auto-scroll");

        if shadcn.button_sm(ui, "Copy").clicked() {
            let text = filtered
                .iter()
                .map(|e| e.formatted_line())
                .collect::<Vec<_>>()
                .join("\n");
            ui.ctx().copy_text(text);
        }
        if shadcn.button_sm(ui, "Clear").clicked() {
            ppe_bridge::clear_log_entries();
        }
    });

    ui.add_space(4.0);
    ui.label(
        RichText::new(format!(
            "Total: {}   |   Showing: {}   |   Debug: {}",
            snap.log_entries.len(),
            filtered.len(),
            if snap.config.debug { "ON" } else { "OFF" }
        ))
        .size(12.0)
        .color(shadcn.colors().muted_foreground),
    );
    ui.separator();

    egui::ScrollArea::vertical()
        .id_salt("ppe_bridge_log_scroll")
        .auto_shrink([false, false])
        .stick_to_bottom(st.log_auto_scroll)
        .show(ui, |ui| {
            for entry in &filtered {
                let color = match entry.level.as_str() {
                    "WARN" => Color32::from_rgb(245, 180, 70),
                    "ERROR" => Color32::from_rgb(240, 95, 95),
                    "DEBUG" => shadcn.colors().muted_foreground,
                    _ => shadcn.colors().foreground,
                };
                ui.label(
                    RichText::new(entry.formatted_line())
                        .monospace()
                        .size(12.0)
                        .color(color),
                );
            }
        });
}

fn render_config_section(
    ui: &mut egui::Ui,
    shadcn: &Shadcn,
    st: &mut BridgeUiState,
    snap: &PpeBridgeSnapshot,
) {
    egui::ScrollArea::vertical()
        .id_salt("ppe_bridge_config_scroll")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.heading("PPE Discord Bot Bridge Configuration");
            ui.label(
                RichText::new(
                    "Configure the HTTP bridge to send tracked UT/ST loot drops to your RotMG PPE Discord Bot.",
                )
                .color(shadcn.colors().muted_foreground),
            );
            ui.add_space(10.0);

            egui::Grid::new("ppe_bridge_config_grid")
                .num_columns(2)
                .spacing([16.0, 10.0])
                .show(ui, |ui| {
                    ui.label(RichText::new("Enable Bot Bridge").strong());
                    ui.checkbox(
                        &mut st.draft_config.enabled,
                        "Send tracked loot drops to the Discord bot",
                    );
                    ui.end_row();

                    ui.label(RichText::new("Debug Logging").strong());
                    ui.checkbox(
                        &mut st.draft_config.debug,
                        "Log all observed bag items and HTTP responses",
                    );
                    ui.end_row();

                    ui.label(RichText::new("Bot Endpoint URL").strong());
                    ui.add(
                        egui::TextEdit::singleline(&mut st.draft_config.endpoint)
                            .desired_width(480.0)
                            .hint_text("https://your-bot-host/realmshark/loot"),
                    );
                    ui.end_row();

                    ui.label(RichText::new("Discord Guild ID").strong());
                    ui.add(
                        egui::TextEdit::singleline(&mut st.draft_config.guild_id)
                            .desired_width(480.0)
                            .hint_text("e.g. 123456789012345678"),
                    );
                    ui.end_row();

                    ui.label(RichText::new("User Link Token").strong());
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut st.draft_config.link_token)
                                .password(!st.show_link_token)
                                .desired_width(410.0)
                                .hint_text("Paste token from /sniffer link"),
                        );
                        let toggle_label = if st.show_link_token { "Hide" } else { "Show" };
                        if shadcn.button_sm(ui, toggle_label).clicked() {
                            st.show_link_token = !st.show_link_token;
                        }
                    });
                    ui.end_row();

                    ui.label(RichText::new("Custom CSV Path (Optional)").strong());
                    ui.vertical(|ui| {
                        ui.horizontal(|ui| {
                            ui.add(
                                egui::TextEdit::singleline(&mut st.draft_config.csv_path)
                                    .desired_width(350.0)
                                    .hint_text("Optional — leave blank to use auto-synced GitHub CSV"),
                            );
                            if shadcn.button_sm(ui, "🔄 Refresh CSV").clicked() {
                                match ppe_bridge::request_remote_csv_refresh() {
                                    Ok(msg) => st.status_banner = Some((msg, false)),
                                    Err(err) => st.status_banner = Some((err, true)),
                                }
                            }
                        });
                        ui.label(
                            RichText::new(
                                "Downloading the CSV manually is optional. Tracked items refresh automatically on app startup or when you click Refresh CSV.",
                            )
                            .size(11.5)
                            .color(shadcn.colors().muted_foreground),
                        );
                        ui.label(
                            RichText::new(format!(
                                "Active CSV Source: {} ({} items)",
                                snap.loaded_csv_source, snap.tracked_item_count
                            ))
                            .size(11.5)
                            .color(shadcn.colors().muted_foreground),
                        );
                    });
                    ui.end_row();

                    ui.label(RichText::new("Local Review Log File").strong());
                    ui.add(
                        egui::TextEdit::singleline(&mut st.draft_config.local_review_log)
                            .desired_width(480.0)
                            .hint_text("realmshark_missing_utst_review.log"),
                    );
                    ui.end_row();
                });

            ui.add_space(16.0);
            ui.horizontal(|ui| {
                if shadcn.button(ui, "Save & Send Test Ping").clicked() {
                    match validate_and_save(&st.draft_config) {
                        Ok(()) => {
                            ppe_bridge::send_settings_confirmation_ping();
                            st.status_banner = Some((
                                "Settings saved and test ping queued (check Bridge Log for response)."
                                    .to_string(),
                                false,
                            ));
                        }
                        Err(err) => {
                            st.status_banner = Some((err, true));
                        }
                    }
                }

                if shadcn.button(ui, "Save Settings").clicked() {
                    match validate_and_save(&st.draft_config) {
                        Ok(()) => {
                            st.status_banner =
                                Some(("Settings saved.".to_string(), false));
                        }
                        Err(err) => {
                            st.status_banner = Some((err, true));
                        }
                    }
                }

                if shadcn.button(ui, "Send Test Ping Only").clicked() {
                    ppe_bridge::send_settings_confirmation_ping();
                    st.status_banner = Some((
                        "Test ping queued using saved settings.".to_string(),
                        false,
                    ));
                }
            });

            if let Some((ref msg, is_error)) = st.status_banner {
                ui.add_space(10.0);
                let color = if is_error {
                    Color32::from_rgb(240, 95, 95)
                } else {
                    Color32::from_rgb(80, 210, 120)
                };
                ui.label(RichText::new(msg).color(color).strong());
            }
        });
}

fn validate_and_save(cfg: &PpeBridgeConfig) -> Result<(), String> {
    if cfg.enabled {
        if cfg.endpoint.trim().is_empty()
            || cfg.guild_id.trim().is_empty()
            || cfg.link_token.trim().is_empty()
        {
            return Err(
                "Endpoint URL, Discord Guild ID, and User Link Token are required when the bridge is enabled."
                    .to_string(),
            );
        }
        match cfg.guild_id.trim().parse::<i64>() {
            Ok(id) if id > 0 => {}
            _ => {
                return Err("Discord Guild ID must be a positive integer.".to_string());
            }
        }
    }
    ppe_bridge::update_config(cfg.clone())
}
