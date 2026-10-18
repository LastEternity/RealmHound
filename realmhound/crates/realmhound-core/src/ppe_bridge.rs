//! RotMG PPE Discord Bot Bridge Integration (`ppe_bridge`)
//!
//! Self-contained extension module that ports the Sniffer (PPE-Tomato) HTTP bridge
//! to RealmHound, sending tracked UT/ST loot drops and test pings in the exact JSON
//! format expected by `rotmgppebot`'s `realmshark_ingest.py`.
//!
//! Automatically syncs tracked item names once on app bootup (and on manual user
//! request) from:
//! `https://raw.githubusercontent.com/tseringgg/rotmgppebot/main/rotmg_loot_drops_updated.csv`
//! using ETag conditional requests, local disk caching, and an embedded fallback CSV.

use std::collections::{HashSet, VecDeque};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chrono::Local;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::api::character::parse_enchant_ids;
use crate::api::CharacterClass;
use crate::assets::{get_asset_manager, ObjectAsset};
use crate::loot::{class_ids, PendingBag, PlayerStateTracker};

const DEFAULT_LOCAL_REVIEW_LOG: &str = "realmshark_missing_utst_review.log";
const MAX_REVIEW_ENTRIES: usize = 500;
const MAX_LOG_ENTRIES: usize = 500;

/// Raw GitHub URL for `https://github.com/tseringgg/rotmgppebot/blob/main/rotmg_loot_drops_updated.csv`.
pub const REMOTE_CSV_URL: &str =
    "https://raw.githubusercontent.com/tseringgg/rotmgppebot/main/rotmg_loot_drops_updated.csv";

/// Minimum cooldown for manual UI refresh button (60 seconds) to prevent spamming GitHub.
const REMOTE_CSV_MANUAL_COOLDOWN_SECS: u64 = 60;

const EMBEDDED_TRACKED_ITEMS_CSV: &str = include_str!("rotmg_loot_drops_updated.csv");

const DEFAULT_CSV_CANDIDATE_PATHS: &[&str] = &[
    "rotmg_loot_drops_updated.csv",
    "../rotmgppebot/rotmg_loot_drops_updated.csv",
    "./rotmgppebot/rotmg_loot_drops_updated.csv",
    "../../rotmgppebot/rotmg_loot_drops_updated.csv",
];

/// Persistent configuration for the RotMG PPE Discord Bot bridge.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PpeBridgeConfig {
    pub enabled: bool,
    pub debug: bool,
    pub endpoint: String,
    pub guild_id: String,
    pub link_token: String,
    pub csv_path: String,
    pub local_review_log: String,
}

impl Default for PpeBridgeConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            debug: false,
            endpoint: String::new(),
            guild_id: String::new(),
            link_token: String::new(),
            csv_path: String::new(),
            local_review_log: DEFAULT_LOCAL_REVIEW_LOG.to_string(),
        }
    }
}

/// Persisted metadata for ETag / Last-Modified caching of the remote CSV.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct RemoteCsvMeta {
    etag: Option<String>,
    last_modified: Option<String>,
    last_checked_unix: u64,
    last_updated_unix: u64,
}

/// Review status of a detected UT/ST item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BridgeReviewStatus {
    Sent,
    Unsent,
}

impl BridgeReviewStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            BridgeReviewStatus::Sent => "SENT",
            BridgeReviewStatus::Unsent => "UNSENT",
        }
    }
}

/// Structured review entry for Sent and Unsent UT/ST items (mirrors `BridgeReviewGUI.java`).
#[derive(Debug, Clone)]
pub struct BridgeReviewEntry {
    pub timestamp: String,
    pub status: BridgeReviewStatus,
    pub item_name: String,
    pub item_id: i32,
    pub shiny: bool,
    pub rarity: String,
    pub rarity_source: String,
    pub divine: bool,
    pub enchant_count: usize,
    pub character_id: i32,
    pub character_name: String,
    pub character_class: String,
    pub group: String,
    pub label: String,
    pub dungeon: String,
    pub reason: String,
    pub normalized_name: String,
}

/// Structured log line for the Bridge Log view (mirrors `BridgeLogGUI.java`).
#[derive(Debug, Clone)]
pub struct BridgeLogEntry {
    pub timestamp: String,
    pub level: String,
    pub message: String,
}

impl BridgeLogEntry {
    pub fn formatted_line(&self) -> String {
        format!("[{}] [{}] {}", self.timestamp, self.level, self.message)
    }
}

/// Snapshot of the bridge state for rendering in the UI without holding locks.
#[derive(Debug, Clone)]
pub struct PpeBridgeSnapshot {
    pub config: PpeBridgeConfig,
    pub tracked_item_count: usize,
    pub loaded_csv_source: String,
    pub review_entries: Vec<BridgeReviewEntry>,
    pub log_entries: Vec<BridgeLogEntry>,
    pub sent_count: usize,
    pub unsent_count: usize,
}

#[derive(Debug, Clone)]
struct ParsedItem {
    base_name: String,
    normalized_name: String,
    shiny: bool,
    divine: bool,
    tracked: bool,
}

#[derive(Debug, Clone)]
struct ResolvedRarity {
    rarity: String,
    source: String,
}

enum WorkerTask {
    PostJson { endpoint: String, payload: String },
    RefreshRemoteCsv { manual: bool },
}

struct BridgeState {
    config: PpeBridgeConfig,
    tracked_items: HashSet<String>,
    loaded_csv_path: Option<String>,
    loaded_csv_modified: Option<u64>,
    missing_csv_warned_for_path: Option<String>,
    last_manual_refresh_unix: u64,
    remote_check_in_flight: bool,
    review_entries: VecDeque<BridgeReviewEntry>,
    log_entries: VecDeque<BridgeLogEntry>,
    tx: mpsc::Sender<WorkerTask>,
}

static BRIDGE_STATE: OnceLock<Mutex<BridgeState>> = OnceLock::new();

fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn app_data_dir() -> PathBuf {
    let base = dirs::data_local_dir().unwrap_or_else(|| PathBuf::from("."));
    base.join("RealmHound")
}

fn config_file_path() -> PathBuf {
    app_data_dir().join("ppe_bridge.json")
}

fn cached_remote_csv_path() -> PathBuf {
    app_data_dir().join("rotmg_loot_drops_updated.csv")
}

fn cached_remote_meta_path() -> PathBuf {
    app_data_dir().join("ppe_bridge_csv_meta.json")
}

fn load_remote_meta() -> RemoteCsvMeta {
    fs::read_to_string(cached_remote_meta_path())
        .ok()
        .and_then(|s| serde_json::from_str::<RemoteCsvMeta>(&s).ok())
        .unwrap_or_default()
}

fn save_remote_meta(meta: &RemoteCsvMeta) {
    let path = cached_remote_meta_path();
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(meta) {
        let _ = fs::write(path, json);
    }
}

fn load_config_from_disk() -> PpeBridgeConfig {
    let path = config_file_path();
    if let Ok(contents) = fs::read_to_string(&path) {
        if let Ok(mut cfg) = serde_json::from_str::<PpeBridgeConfig>(&contents) {
            if cfg.local_review_log.trim().is_empty() {
                cfg.local_review_log = DEFAULT_LOCAL_REVIEW_LOG.to_string();
            }
            return cfg;
        }
    }

    // Attempt one-time migration from Sniffer `config.properties` if present
    for candidate in &[
        "config.properties",
        "../Sniffer/config.properties",
        "../../Sniffer/config.properties",
    ] {
        let p = Path::new(candidate);
        if let Ok(contents) = fs::read_to_string(p) {
            if let Some(cfg) = parse_legacy_properties(&contents) {
                let _ = save_config_to_disk(&cfg);
                return cfg;
            }
        }
    }

    PpeBridgeConfig::default()
}

fn parse_legacy_properties(contents: &str) -> Option<PpeBridgeConfig> {
    let mut cfg = PpeBridgeConfig::default();
    let mut found_any = false;
    for raw_line in contents.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with('!') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let key = k.trim();
        let val = v.trim().replace("\\:", ":").replace("\\=", "=");
        match key {
            "realmshark.bridge.enabled" => {
                cfg.enabled = val.eq_ignore_ascii_case("true");
                found_any = true;
            }
            "realmshark.bridge.debug" => {
                cfg.debug = val.eq_ignore_ascii_case("true");
                found_any = true;
            }
            "realmshark.bridge.endpoint" => {
                cfg.endpoint = val;
                found_any = true;
            }
            "realmshark.bridge.guildId" => {
                cfg.guild_id = val;
                found_any = true;
            }
            "realmshark.bridge.linkToken" => {
                cfg.link_token = val;
                found_any = true;
            }
            "realmshark.bridge.csvPath" => {
                cfg.csv_path = val;
                found_any = true;
            }
            "realmshark.bridge.localReviewLog" => {
                if !val.is_empty() {
                    cfg.local_review_log = val;
                }
                found_any = true;
            }
            _ => {}
        }
    }
    if found_any {
        Some(cfg)
    } else {
        None
    }
}

fn save_config_to_disk(config: &PpeBridgeConfig) -> Result<(), String> {
    let path = config_file_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let json = serde_json::to_string_pretty(config).map_err(|e| e.to_string())?;
    fs::write(&path, json).map_err(|e| e.to_string())
}

fn init_bridge_state() -> Mutex<BridgeState> {
    let (tx, rx) = mpsc::channel::<WorkerTask>();

    std::thread::Builder::new()
        .name("ppe-bridge-http".to_string())
        .spawn(move || {
            let client = reqwest::blocking::Client::builder()
                .user_agent("RealmHound-PPEBridge")
                .connect_timeout(Duration::from_millis(2000))
                .timeout(Duration::from_millis(5000))
                .build()
                .unwrap_or_else(|_| reqwest::blocking::Client::new());

            while let Ok(task) = rx.recv() {
                match task {
                    WorkerTask::PostJson { endpoint, payload } => {
                        send_http_post(&client, &endpoint, &payload);
                    }
                    WorkerTask::RefreshRemoteCsv { manual } => {
                        perform_remote_csv_refresh(&client, manual);
                    }
                }
            }
        })
        .ok();

    let config = load_config_from_disk();
    let mut state = BridgeState {
        config,
        tracked_items: HashSet::new(),
        loaded_csv_path: None,
        loaded_csv_modified: None,
        missing_csv_warned_for_path: None,
        last_manual_refresh_unix: 0,
        remote_check_in_flight: false,
        review_entries: VecDeque::new(),
        log_entries: VecDeque::new(),
        tx: tx.clone(),
    };
    load_tracked_items_if_needed(&mut state);

    // Automatically refresh the CSV once on app bootup (using ETag / If-None-Match).
    // No recurring background polling or missing-item network requests are performed.
    if state.config.csv_path.trim().is_empty() {
        state.remote_check_in_flight = true;
        let _ = tx.send(WorkerTask::RefreshRemoteCsv { manual: false });
    }

    Mutex::new(state)
}

fn state_lock() -> &'static Mutex<BridgeState> {
    BRIDGE_STATE.get_or_init(init_bridge_state)
}

fn push_log_locked(state: &mut BridgeState, level: &str, message: impl Into<String>) {
    let msg = message.into();
    let now = Local::now().format("%H:%M:%S").to_string();
    state.log_entries.push_back(BridgeLogEntry {
        timestamp: now,
        level: level.to_string(),
        message: msg,
    });
    while state.log_entries.len() > MAX_LOG_ENTRIES {
        state.log_entries.pop_front();
    }
}

fn log_info_locked(state: &mut BridgeState, message: impl Into<String>) {
    push_log_locked(state, "INFO", message);
}

fn log_warn_locked(state: &mut BridgeState, message: impl Into<String>) {
    push_log_locked(state, "WARN", message);
}

fn log_debug_locked(state: &mut BridgeState, message: impl Into<String>) {
    if state.config.debug {
        push_log_locked(state, "DEBUG", message);
    }
}

/// Public logging helper.
pub fn log_message(level: &str, message: impl Into<String>) {
    if let Ok(mut state) = state_lock().lock() {
        push_log_locked(&mut state, level, message);
    }
}

/// Trigger a manual refresh of the remote CSV from GitHub (rate-limited by a 60s cooldown).
pub fn request_remote_csv_refresh() -> Result<String, String> {
    let Ok(mut state) = state_lock().lock() else {
        return Err("Bridge state unavailable.".to_string());
    };

    if !state.config.csv_path.trim().is_empty() {
        state.loaded_csv_path = None;
        state.loaded_csv_modified = None;
        load_tracked_items_if_needed(&mut state);
        return Ok(format!(
            "Reloaded custom CSV path ({} items).",
            state.tracked_items.len()
        ));
    }

    if state.remote_check_in_flight {
        return Ok("A remote CSV refresh is already in progress.".to_string());
    }

    let now = now_unix_secs();
    let elapsed = now.saturating_sub(state.last_manual_refresh_unix);
    if state.last_manual_refresh_unix > 0 && elapsed < REMOTE_CSV_MANUAL_COOLDOWN_SECS {
        return Err(format!(
            "Please wait {}s before manually refreshing from GitHub again.",
            REMOTE_CSV_MANUAL_COOLDOWN_SECS - elapsed
        ));
    }

    state.last_manual_refresh_unix = now;
    state.remote_check_in_flight = true;
    let _ = state.tx.send(WorkerTask::RefreshRemoteCsv { manual: true });
    Ok("Refreshing CSV from GitHub...".to_string())
}

/// Obtain a snapshot of the current bridge configuration, logs, and review entries.
pub fn snapshot() -> PpeBridgeSnapshot {
    let Ok(mut state) = state_lock().lock() else {
        return PpeBridgeSnapshot {
            config: PpeBridgeConfig::default(),
            tracked_item_count: 0,
            loaded_csv_source: String::new(),
            review_entries: Vec::new(),
            log_entries: Vec::new(),
            sent_count: 0,
            unsent_count: 0,
        };
    };

    load_tracked_items_if_needed(&mut state);

    let mut sent_count = 0;
    let mut unsent_count = 0;
    for entry in &state.review_entries {
        match entry.status {
            BridgeReviewStatus::Sent => sent_count += 1,
            BridgeReviewStatus::Unsent => unsent_count += 1,
        }
    }

    PpeBridgeSnapshot {
        config: state.config.clone(),
        tracked_item_count: state.tracked_items.len(),
        loaded_csv_source: state
            .loaded_csv_path
            .clone()
            .unwrap_or_else(|| "<none>".to_string()),
        review_entries: state.review_entries.iter().cloned().collect(),
        log_entries: state.log_entries.iter().cloned().collect(),
        sent_count,
        unsent_count,
    }
}

/// Get a copy of the current bridge configuration.
pub fn get_config() -> PpeBridgeConfig {
    state_lock()
        .lock()
        .map(|s| s.config.clone())
        .unwrap_or_default()
}

/// Update and persist the bridge configuration.
pub fn update_config(mut new_config: PpeBridgeConfig) -> Result<(), String> {
    new_config.endpoint = new_config.endpoint.trim().to_string();
    new_config.guild_id = new_config.guild_id.trim().to_string();
    new_config.link_token = new_config.link_token.trim().to_string();
    new_config.csv_path = new_config.csv_path.trim().to_string();
    new_config.local_review_log = new_config.local_review_log.trim().to_string();
    if new_config.local_review_log.is_empty() {
        new_config.local_review_log = DEFAULT_LOCAL_REVIEW_LOG.to_string();
    }

    save_config_to_disk(&new_config)?;

    if let Ok(mut state) = state_lock().lock() {
        let csv_changed = state.config.csv_path != new_config.csv_path;
        state.config = new_config;
        if csv_changed {
            state.loaded_csv_path = None;
            state.loaded_csv_modified = None;
            state.missing_csv_warned_for_path = None;
        }
        load_tracked_items_if_needed(&mut state);
        log_info_locked(&mut state, "Saved PPE bridge settings.");
    }

    Ok(())
}

/// Toggle debug logging immediately and persist.
pub fn set_debug_enabled(debug: bool) {
    if let Ok(mut state) = state_lock().lock() {
        state.config.debug = debug;
        let _ = save_config_to_disk(&state.config);
        log_info_locked(
            &mut state,
            if debug {
                "Debug logging enabled."
            } else {
                "Debug logging disabled."
            },
        );
    }
}

/// Clear all review entries.
pub fn clear_review_entries() {
    if let Ok(mut state) = state_lock().lock() {
        state.review_entries.clear();
    }
}

/// Clear all log entries.
pub fn clear_log_entries() {
    if let Ok(mut state) = state_lock().lock() {
        state.log_entries.clear();
    }
}

/// Send a settings confirmation test ping (mirrors `SendLoot.sendSettingsConfirmationPing`).
pub fn send_settings_confirmation_ping() {
    let Ok(mut state) = state_lock().lock() else {
        return;
    };

    if !state.config.enabled {
        log_info_locked(
            &mut state,
            "Settings saved (bridge disabled; skipped test ping).",
        );
        return;
    }

    let endpoint = state.config.endpoint.trim().to_string();
    let guild_id_raw = state.config.guild_id.trim().to_string();
    let link_token = state.config.link_token.trim().to_string();

    if endpoint.is_empty() || guild_id_raw.is_empty() || link_token.is_empty() {
        log_warn_locked(
            &mut state,
            "Settings saved, but test ping was skipped because endpoint, guild ID, or link token is missing.",
        );
        return;
    }

    let Ok(guild_id) = guild_id_raw.parse::<i64>() else {
        log_warn_locked(
            &mut state,
            format!(
                "Settings saved, but test ping was skipped because guild ID is invalid: {}",
                guild_id_raw
            ),
        );
        return;
    };
    if guild_id <= 0 {
        log_warn_locked(
            &mut state,
            format!(
                "Settings saved, but test ping was skipped because guild ID must be positive: {}",
                guild_id_raw
            ),
        );
        return;
    }

    let payload = json!({
        "guild_id": guild_id,
        "link_token": link_token,
        "event_type": "bridge_settings_test",
        "source": "tomato"
    })
    .to_string();

    log_info_locked(
        &mut state,
        "Queued settings confirmation ping to bot endpoint.",
    );
    log_debug_locked(&mut state, format!("Settings test payload={}", payload));
    let _ = state.tx.send(WorkerTask::PostJson { endpoint, payload });
}

/// Process a newly detected pending loot bag from `LootTracker::on_tick`.
pub fn on_pending_bag(
    bag: &PendingBag,
    player_state: &PlayerStateTracker,
    dungeon_name: &str,
    time_ms: u64,
) {
    let Ok(mut state) = state_lock().lock() else {
        return;
    };

    if !state.config.enabled {
        return;
    }

    if bag.bag_type().is_none() {
        return;
    }

    let dungeon = dungeon_name.trim().to_string();
    if dungeon.eq_ignore_ascii_case("{s.nexus}") || dungeon.eq_ignore_ascii_case("nexus") {
        return;
    }

    load_tracked_items_if_needed(&mut state);
    if state.tracked_items.is_empty() {
        return;
    }

    let endpoint = state.config.endpoint.trim().to_string();
    let guild_id_raw = state.config.guild_id.trim().to_string();
    let link_token = state.config.link_token.trim().to_string();
    if endpoint.is_empty() || guild_id_raw.is_empty() || link_token.is_empty() {
        log_warn_locked(
            &mut state,
            "Bridge enabled, but endpoint/guildId/linkToken is incomplete.",
        );
        return;
    }

    let Ok(guild_id) = guild_id_raw.parse::<i64>() else {
        log_warn_locked(&mut state, format!("Invalid guild ID: {}", guild_id_raw));
        return;
    };
    if guild_id <= 0 {
        log_warn_locked(
            &mut state,
            format!("Invalid guild ID (must be positive): {}", guild_id_raw),
        );
        return;
    }

    let asset_mgr = get_asset_manager();
    let current_player = player_state.current_player();
    let character_id = current_player
        .map(|p| p.char_id)
        .filter(|&id| id > 0)
        .unwrap_or(-1);
    let character_name = current_player
        .map(|p| p.name.trim().to_string())
        .unwrap_or_default();
    let character_class = current_player
        .filter(|p| class_ids::is_player_class(p.object_type))
        .map(|p| {
            CharacterClass::from_id(p.object_type as u16)
                .name()
                .to_string()
        })
        .unwrap_or_default();
    let is_seasonal = current_player.map(|p| p.is_seasonal).unwrap_or(false);
    let loot_drop_bonus = current_player
        .map(|p| p.loot_drop_time_remaining(time_ms) > 0)
        .unwrap_or(false);

    for (slot_index, &item_id) in bag.items.iter().enumerate() {
        if item_id <= 0 {
            continue;
        }

        let enchant_count = bag
            .enchants
            .get(slot_index)
            .and_then(|e| e.as_deref())
            .map(|s| parse_enchant_ids(s).len())
            .unwrap_or(0);

        let asset = asset_mgr.get_object(item_id);
        let item_group = asset
            .as_ref()
            .map(|a| a.group.trim().to_string())
            .unwrap_or_default();
        let item_label = asset
            .as_ref()
            .map(|a| a.labels.trim().to_string())
            .unwrap_or_default();

        let parsed = parse_item_name(item_id, asset.as_ref(), &state.tracked_items);
        let rarity_info = resolve_item_rarity(asset.as_ref(), enchant_count);
        let divine_resolved = parsed.divine || rarity_info.rarity == "divine";

        log_debug_locked(
            &mut state,
            format!(
                "Observed bag item id={} name=\"{}\" normalized=\"{}\" shiny={} rarity={} raritySource={} divine={} enchants={} tracked={} charId={} charName=\"{}\" class=\"{}\" group=\"{}\" label=\"{}\" dungeon=\"{}\" seasonal={} lootBoost={}",
                item_id,
                parsed.base_name,
                parsed.normalized_name,
                parsed.shiny,
                rarity_info.rarity,
                rarity_info.source,
                divine_resolved,
                enchant_count,
                parsed.tracked,
                character_id,
                character_name,
                character_class,
                item_group,
                item_label,
                dungeon,
                is_seasonal,
                loot_drop_bonus
            ),
        );

        if !parsed.tracked {
            if is_likely_ut_or_st_item(asset.as_ref()) {
                log_missing_ut_st_for_review(
                    &mut state,
                    item_id,
                    &parsed,
                    &rarity_info,
                    divine_resolved,
                    enchant_count,
                    character_id,
                    &character_name,
                    &character_class,
                    &item_group,
                    &item_label,
                    &dungeon,
                    "ut_st_not_in_csv",
                );
            }
            continue;
        }

        queue_tracked_drop_locked(
            &mut state,
            &endpoint,
            guild_id,
            &link_token,
            item_id,
            &parsed,
            &rarity_info,
            divine_resolved,
            enchant_count,
            character_id,
            &character_name,
            &character_class,
            &item_group,
            &item_label,
            &dungeon,
            is_seasonal,
            loot_drop_bonus,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn queue_tracked_drop_locked(
    state: &mut BridgeState,
    endpoint: &str,
    guild_id: i64,
    link_token: &str,
    item_id: i32,
    parsed: &ParsedItem,
    rarity_info: &ResolvedRarity,
    divine_resolved: bool,
    enchant_count: usize,
    character_id: i32,
    character_name: &str,
    character_class: &str,
    item_group: &str,
    item_label: &str,
    dungeon: &str,
    is_seasonal: bool,
    loot_drop_bonus: bool,
) {
    let payload = build_loot_payload(
        guild_id,
        link_token,
        &parsed.base_name,
        parsed.shiny,
        item_id,
        character_id,
        character_name,
        character_class,
        item_group,
        item_label,
        &rarity_info.rarity,
        divine_resolved,
        dungeon,
        is_seasonal,
        loot_drop_bonus,
    );

    push_review_entry_locked(
        state,
        BridgeReviewEntry {
            timestamp: Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
            status: BridgeReviewStatus::Sent,
            item_name: parsed.base_name.clone(),
            item_id,
            shiny: parsed.shiny,
            rarity: rarity_info.rarity.clone(),
            rarity_source: rarity_info.source.clone(),
            divine: divine_resolved,
            enchant_count,
            character_id,
            character_name: character_name.to_string(),
            character_class: character_class.to_string(),
            group: item_group.to_string(),
            label: item_label.to_string(),
            dungeon: dungeon.to_string(),
            reason: "matched_csv_queued_for_bot".to_string(),
            normalized_name: parsed.normalized_name.clone(),
        },
    );

    log_info_locked(
        state,
        format!(
            "Queued tracked drop: {} (id={}, shiny={}, rarity={}, divine={}, seasonal={}, charId={})",
            parsed.base_name,
            item_id,
            parsed.shiny,
            rarity_info.rarity,
            divine_resolved,
            is_seasonal,
            character_id
        ),
    );
    log_debug_locked(state, format!("Payload={}", payload));
    let _ = state.tx.send(WorkerTask::PostJson {
        endpoint: endpoint.to_string(),
        payload,
    });
}

#[allow(clippy::too_many_arguments)]
pub fn build_loot_payload(
    guild_id: i64,
    link_token: &str,
    item_name: &str,
    shiny: bool,
    item_id: i32,
    character_id: i32,
    character_name: &str,
    character_class: &str,
    item_group: &str,
    item_label: &str,
    item_rarity: &str,
    divine: bool,
    dungeon: &str,
    is_seasonal: bool,
    loot_drop_bonus: bool,
) -> String {
    let mut map = Map::new();
    map.insert("guild_id".to_string(), Value::Number(guild_id.into()));
    map.insert(
        "link_token".to_string(),
        Value::String(link_token.to_string()),
    );
    map.insert(
        "item_name".to_string(),
        Value::String(item_name.to_string()),
    );
    map.insert("shiny".to_string(), Value::Bool(shiny));
    map.insert("item_id".to_string(), Value::Number(item_id.into()));
    if character_id > 0 {
        map.insert(
            "character_id".to_string(),
            Value::Number(character_id.into()),
        );
    }
    if !character_name.is_empty() {
        map.insert(
            "character_name".to_string(),
            Value::String(character_name.to_string()),
        );
    }
    if !character_class.is_empty() {
        map.insert(
            "character_class".to_string(),
            Value::String(character_class.to_string()),
        );
    }
    if !item_group.is_empty() {
        map.insert(
            "item_group".to_string(),
            Value::String(item_group.to_string()),
        );
    }
    if !item_label.is_empty() {
        map.insert(
            "item_label".to_string(),
            Value::String(item_label.to_string()),
        );
    }
    map.insert(
        "item_rarity".to_string(),
        Value::String(item_rarity.to_string()),
    );
    map.insert("divine".to_string(), Value::Bool(divine));
    if !dungeon.is_empty() {
        map.insert("dungeon".to_string(), Value::String(dungeon.to_string()));
    }
    map.insert("is_seasonal".to_string(), Value::Bool(is_seasonal));
    map.insert("loot_drop_bonus".to_string(), Value::Bool(loot_drop_bonus));
    map.insert("source".to_string(), Value::String("tomato".to_string()));
    Value::Object(map).to_string()
}

fn push_review_entry_locked(state: &mut BridgeState, entry: BridgeReviewEntry) {
    state.review_entries.push_back(entry);
    while state.review_entries.len() > MAX_REVIEW_ENTRIES {
        state.review_entries.pop_front();
    }
}

#[allow(clippy::too_many_arguments)]
fn log_missing_ut_st_for_review(
    state: &mut BridgeState,
    item_id: i32,
    item: &ParsedItem,
    rarity_info: &ResolvedRarity,
    divine_resolved: bool,
    enchant_count: usize,
    character_id: i32,
    character_name: &str,
    character_class: &str,
    item_group: &str,
    item_label: &str,
    dungeon: &str,
    reason: &str,
) {
    let timestamp = Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    push_review_entry_locked(
        state,
        BridgeReviewEntry {
            timestamp: timestamp.clone(),
            status: BridgeReviewStatus::Unsent,
            item_name: item.base_name.clone(),
            item_id,
            shiny: item.shiny,
            rarity: rarity_info.rarity.clone(),
            rarity_source: rarity_info.source.clone(),
            divine: divine_resolved,
            enchant_count,
            character_id,
            character_name: character_name.to_string(),
            character_class: character_class.to_string(),
            group: item_group.to_string(),
            label: item_label.to_string(),
            dungeon: dungeon.to_string(),
            reason: reason.to_string(),
            normalized_name: item.normalized_name.clone(),
        },
    );

    let line = format!(
        "[{}] reason={} id={} name=\"{}\" normalized=\"{}\" shiny={} rarity={} raritySource={} divine={} enchants={} charId={} charName=\"{}\" class=\"{}\" group=\"{}\" label=\"{}\" dungeon=\"{}\"",
        timestamp,
        reason,
        item_id,
        item.base_name,
        item.normalized_name,
        item.shiny,
        rarity_info.rarity,
        rarity_info.source,
        divine_resolved,
        enchant_count,
        character_id,
        character_name,
        character_class,
        item_group,
        item_label,
        dungeon
    );

    log_warn_locked(state, format!("Local review candidate: {}", line));

    let review_path = if state.config.local_review_log.trim().is_empty() {
        PathBuf::from(DEFAULT_LOCAL_REVIEW_LOG)
    } else {
        PathBuf::from(state.config.local_review_log.trim())
    };

    if let Some(parent) = review_path.parent() {
        if !parent.as_os_str().is_empty() {
            let _ = fs::create_dir_all(parent);
        }
    }

    match OpenOptions::new()
        .create(true)
        .append(true)
        .open(&review_path)
    {
        Ok(mut file) => {
            if let Err(e) = writeln!(file, "{}", line) {
                log_warn_locked(
                    state,
                    format!(
                        "Failed writing local review log ({}): {}",
                        review_path.display(),
                        e
                    ),
                );
            }
        }
        Err(e) => {
            log_warn_locked(
                state,
                format!(
                    "Failed opening local review log ({}): {}",
                    review_path.display(),
                    e
                ),
            );
        }
    }
}

fn send_http_post(client: &reqwest::blocking::Client, endpoint: &str, payload: &str) {
    match client
        .post(endpoint)
        .header("Content-Type", "application/json; charset=UTF-8")
        .body(payload.to_string())
        .send()
    {
        Ok(resp) => {
            let status = resp.status().as_u16();
            let body = resp.text().unwrap_or_default();
            if (200..300).contains(&status) {
                if let Ok(mut state) = state_lock().lock() {
                    log_debug_locked(
                        &mut state,
                        format!("POST success ({}): {}", status, body.trim()),
                    );
                }
            } else {
                log_message("WARN", format!("POST failed ({}): {}", status, body.trim()));
            }
        }
        Err(err) => {
            log_message("WARN", format!("POST error: {}", err));
        }
    }
}

/// Fetch the latest CSV from `tseringgg/rotmgppebot` using ETag / Last-Modified conditional headers.
/// Called ONLY once on app bootup and when the user manually clicks "Refresh CSV".
fn perform_remote_csv_refresh(client: &reqwest::blocking::Client, manual: bool) {
    let mut meta = load_remote_meta();

    let mut req = client.get(REMOTE_CSV_URL);
    if cached_remote_csv_path().is_file() {
        if let Some(ref etag) = meta.etag {
            req = req.header("If-None-Match", etag);
        }
        if let Some(ref lm) = meta.last_modified {
            req = req.header("If-Modified-Since", lm);
        }
    }

    match req.send() {
        Ok(resp) => {
            let status = resp.status().as_u16();
            meta.last_checked_unix = now_unix_secs();

            if status == 304 {
                save_remote_meta(&meta);
                if let Ok(mut state) = state_lock().lock() {
                    state.remote_check_in_flight = false;
                    if manual {
                        let count = state.tracked_items.len();
                        log_info_locked(
                            &mut state,
                            format!(
                                "GitHub CSV is already up to date (304 Not Modified, {} items).",
                                count
                            ),
                        );
                    } else {
                        log_debug_locked(
                            &mut state,
                            "Bootup GitHub CSV check: 304 Not Modified (cache is current).",
                        );
                    }
                }
                return;
            }

            if (200..300).contains(&status) {
                let new_etag = resp
                    .headers()
                    .get("etag")
                    .and_then(|v| v.to_str().ok())
                    .map(|s| s.to_string());
                let new_lm = resp
                    .headers()
                    .get("last-modified")
                    .and_then(|v| v.to_str().ok())
                    .map(|s| s.to_string());

                if let Ok(body) = resp.text() {
                    match parse_tracked_items_csv(&body) {
                        Ok(remote_items) if !remote_items.is_empty() => {
                            let cache_path = cached_remote_csv_path();
                            if let Some(parent) = cache_path.parent() {
                                let _ = fs::create_dir_all(parent);
                            }
                            let _ = fs::write(&cache_path, &body);

                            meta.etag = new_etag;
                            meta.last_modified = new_lm;
                            meta.last_updated_unix = meta.last_checked_unix;
                            save_remote_meta(&meta);

                            if let Ok(mut state) = state_lock().lock() {
                                state.remote_check_in_flight = false;
                                if state.config.csv_path.trim().is_empty() {
                                    state.loaded_csv_path = None;
                                    load_tracked_items_if_needed(&mut state);
                                    let count = state.tracked_items.len();
                                    log_info_locked(
                                        &mut state,
                                        format!(
                                            "Synced tracked items CSV from GitHub ({} total tracked items).",
                                            count
                                        ),
                                    );
                                }
                            }
                            return;
                        }
                        Ok(_) => {
                            log_message(
                                "WARN",
                                "Fetched GitHub CSV was empty; keeping cached items.",
                            );
                        }
                        Err(e) => {
                            log_message(
                                "WARN",
                                format!("Failed parsing fetched GitHub CSV: {}", e),
                            );
                        }
                    }
                }
            } else {
                log_message(
                    "WARN",
                    format!(
                        "GitHub CSV check returned HTTP {}; using cached CSV.",
                        status
                    ),
                );
            }
        }
        Err(e) => {
            log_message(
                "DEBUG",
                format!(
                    "GitHub CSV check offline/unreachable ({}); using cached CSV.",
                    e
                ),
            );
        }
    }

    meta.last_checked_unix = now_unix_secs();
    save_remote_meta(&meta);
    if let Ok(mut state) = state_lock().lock() {
        state.remote_check_in_flight = false;
    }
}

fn load_tracked_items_if_needed(state: &mut BridgeState) {
    let custom_csv = state.config.csv_path.trim().to_string();

    if !custom_csv.is_empty() {
        let p = Path::new(&custom_csv);
        if !p.is_file() {
            let abs = p
                .canonicalize()
                .unwrap_or_else(|_| p.to_path_buf())
                .display()
                .to_string();
            if state.missing_csv_warned_for_path.as_deref() != Some(&abs) {
                log_warn_locked(
                    state,
                    format!("Missing CSV file at configured path: {}", abs),
                );
                state.missing_csv_warned_for_path = Some(abs);
            }
            state.tracked_items.clear();
            state.loaded_csv_path = None;
            state.loaded_csv_modified = None;
            return;
        }
        load_csv_file_from_disk(state, p);
        return;
    }

    // Automatic mode: combine embedded baseline CSV + cached GitHub CSV + any local repo CSV
    // so no manual CSV download is ever required.
    let cache_path = cached_remote_csv_path();
    let cache_modified = fs::metadata(&cache_path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs());

    let auto_source_label = if cache_path.is_file() {
        "Auto-synced on boot from GitHub (tseringgg/rotmgppebot) + Built-in".to_string()
    } else {
        "Built-in CSV (GitHub boot sync pending)".to_string()
    };

    if state.loaded_csv_path.as_deref() == Some(auto_source_label.as_str())
        && state.loaded_csv_modified == cache_modified
        && !state.tracked_items.is_empty()
    {
        return;
    }

    let mut combined = parse_tracked_items_csv(EMBEDDED_TRACKED_ITEMS_CSV).unwrap_or_default();

    if cache_path.is_file() {
        if let Ok(contents) = fs::read_to_string(&cache_path) {
            if let Ok(cached_items) = parse_tracked_items_csv(&contents) {
                combined.extend(cached_items);
            }
        }
    }

    for candidate in DEFAULT_CSV_CANDIDATE_PATHS {
        let p = Path::new(candidate);
        if p.is_file() {
            if let Ok(contents) = fs::read_to_string(p) {
                if let Ok(local_items) = parse_tracked_items_csv(&contents) {
                    combined.extend(local_items);
                }
            }
        }
    }

    let count = combined.len();
    state.tracked_items = combined;
    state.loaded_csv_path = Some(auto_source_label.clone());
    state.loaded_csv_modified = cache_modified;
    state.missing_csv_warned_for_path = None;
    log_info_locked(
        state,
        format!(
            "Loaded {} tracked item names ({})",
            count, auto_source_label
        ),
    );
}

fn load_csv_file_from_disk(state: &mut BridgeState, path: &Path) {
    let abs = path
        .canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())
        .display()
        .to_string();
    let modified = fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs());

    if state.loaded_csv_path.as_deref() == Some(abs.as_str())
        && state.loaded_csv_modified == modified
        && !state.tracked_items.is_empty()
    {
        return;
    }

    match fs::read_to_string(path) {
        Ok(contents) => match parse_tracked_items_csv(&contents) {
            Ok(items) => {
                let count = items.len();
                state.tracked_items = items;
                state.loaded_csv_path = Some(abs.clone());
                state.loaded_csv_modified = modified;
                state.missing_csv_warned_for_path = None;
                log_info_locked(
                    state,
                    format!("Loaded {} tracked item names from {}", count, abs),
                );
            }
            Err(err) => {
                log_warn_locked(state, format!("Failed parsing CSV ({}): {}", abs, err));
            }
        },
        Err(err) => {
            log_warn_locked(state, format!("Failed reading CSV ({}): {}", abs, err));
        }
    }
}

pub fn parse_tracked_items_csv(contents: &str) -> Result<HashSet<String>, String> {
    let mut lines = contents.lines();
    let Some(header_line) = lines.next() else {
        return Ok(HashSet::new());
    };

    let headers = split_csv_line(header_line.trim_start_matches('\u{feff}'));
    let mut item_name_index = None;
    for (i, h) in headers.iter().enumerate() {
        if unquote(h).trim().eq_ignore_ascii_case("Item Name") {
            item_name_index = Some(i);
            break;
        }
    }

    let Some(col_idx) = item_name_index else {
        return Err("CSV header is missing 'Item Name' column.".to_string());
    };

    let mut parsed = HashSet::new();
    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        let cols = split_csv_line(line);
        if col_idx >= cols.len() {
            continue;
        }
        let raw_name = unquote(&cols[col_idx]).trim().to_string();
        if raw_name.is_empty() {
            continue;
        }
        let norm = normalized_name(&raw_name);
        if !norm.is_empty() {
            parsed.insert(norm);
        }
    }

    Ok(parsed)
}

fn split_csv_line(line: &str) -> Vec<String> {
    let mut cols = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        let c = chars[i];
        if c == '"' {
            if in_quotes && i + 1 < chars.len() && chars[i + 1] == '"' {
                current.push('"');
                i += 2;
                continue;
            }
            in_quotes = !in_quotes;
            i += 1;
            continue;
        }
        if c == ',' && !in_quotes {
            cols.push(std::mem::take(&mut current));
            i += 1;
            continue;
        }
        current.push(c);
        i += 1;
    }
    cols.push(current);
    cols
}

fn unquote(value: &str) -> &str {
    let v = value.trim();
    if v.len() >= 2 && v.starts_with('"') && v.ends_with('"') {
        &v[1..v.len() - 1]
    } else {
        v
    }
}

/// Normalize item names using the exact rules from `SendLoot.normalizedName` and `realmshark_ingest.py`.
pub fn normalized_name(name: &str) -> String {
    let mut mapped = String::with_capacity(name.len());
    for c in name.chars() {
        match c {
            '\u{2018}' | '\u{2019}' | '\u{02bc}' | '\u{2032}' | '\u{00b4}' | '`' => {
                mapped.push('\'');
            }
            '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' | '\u{2014}' | '\u{2015}'
            | '\u{2212}' => {
                mapped.push('-');
            }
            _ => mapped.push(c),
        }
    }

    // Collapse whitespace around '-' ("\s*-\s*" -> "-") and collapse whitespace runs to single ' '
    let mut dash_collapsed = String::with_capacity(mapped.len());
    let mut pending_space = false;
    for c in mapped.chars() {
        if c == '-' {
            pending_space = false;
            while dash_collapsed.ends_with(' ') {
                dash_collapsed.pop();
            }
            dash_collapsed.push('-');
        } else if c.is_whitespace() {
            if !dash_collapsed.ends_with('-') && !dash_collapsed.is_empty() {
                pending_space = true;
            }
        } else {
            if pending_space {
                dash_collapsed.push(' ');
                pending_space = false;
            }
            dash_collapsed.push(c);
        }
    }

    dash_collapsed.trim().to_lowercase()
}

fn parse_item_name(
    item_id: i32,
    asset: Option<&ObjectAsset>,
    tracked_items: &HashSet<String>,
) -> ParsedItem {
    let raw_name = match asset {
        Some(a) => {
            let display_or_id = if !a.display_name.is_empty() {
                a.display_name.as_str()
            } else {
                a.id_name.as_str()
            };
            // `a.name()` applies RealmHound's display-name overrides:
            // 1. Disambiguating items like "Green Beehemoth Quiver" (which IS in the CSV)
            // 2. Appending " Shiny" to shiny item variants (where Sniffer uses `displayName`)
            let named = a.name();
            let named_lower = named.trim().to_ascii_lowercase();
            if !named_lower.ends_with(" shiny") {
                named
            } else {
                display_or_id
            }
        }
        None => "",
    };

    let trimmed = if raw_name.trim().is_empty() {
        format!("Unknown Item {}", item_id)
    } else {
        raw_name.trim().to_string()
    };

    let mut shiny_from_suffix = false;
    let mut base_name = trimmed.clone();
    if base_name.to_ascii_lowercase().ends_with("(shiny)") {
        let new_len = base_name.len().saturating_sub(7);
        base_name = base_name[..new_len].trim().to_string();
        shiny_from_suffix = true;
    }

    let shiny_from_metadata = has_metadata_token(asset, "SHINY");
    let shiny = shiny_from_suffix || shiny_from_metadata;
    let divine = has_metadata_token(asset, "DIVINE") || name_has_token(&base_name, "DIVINE");

    let normalized = normalized_name(&base_name);
    let tracked = !normalized.is_empty() && tracked_items.contains(&normalized);

    ParsedItem {
        base_name,
        normalized_name: normalized,
        shiny,
        divine,
        tracked,
    }
}

fn resolve_item_rarity(asset: Option<&ObjectAsset>, enchant_count: usize) -> ResolvedRarity {
    for token in metadata_tokens(asset) {
        match token.as_str() {
            "COMMON" | "UNCOMMON" | "RARE" | "LEGENDARY" | "DIVINE" => {
                return ResolvedRarity {
                    rarity: token.to_ascii_lowercase(),
                    source: "metadata_token".to_string(),
                };
            }
            _ => {}
        }
    }

    let from_enchants = match enchant_count {
        0 => Some("common"),
        1 => Some("uncommon"),
        2 => Some("rare"),
        3 => Some("legendary"),
        4 => Some("divine"),
        _ => None,
    };

    if let Some(r) = from_enchants {
        return ResolvedRarity {
            rarity: r.to_string(),
            source: "enchant_count".to_string(),
        };
    }

    ResolvedRarity {
        rarity: "unknown".to_string(),
        source: "fallback_unknown_default".to_string(),
    }
}

fn metadata_tokens(asset: Option<&ObjectAsset>) -> HashSet<String> {
    let mut tokens = HashSet::new();
    let Some(a) = asset else {
        return tokens;
    };
    let combined = format!("{} {}", a.labels, a.group).to_ascii_uppercase();
    for part in combined.split(|c: char| !c.is_ascii_alphanumeric()) {
        if !part.is_empty() {
            tokens.insert(part.to_string());
        }
    }
    tokens
}

fn has_metadata_token(asset: Option<&ObjectAsset>, expected: &str) -> bool {
    let target = expected.trim().to_ascii_uppercase();
    if target.is_empty() {
        return false;
    }
    metadata_tokens(asset).contains(&target)
}

fn name_has_token(name: &str, expected: &str) -> bool {
    let target = expected.trim().to_ascii_uppercase();
    if target.is_empty() || name.trim().is_empty() {
        return false;
    }
    for part in name
        .to_ascii_uppercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
    {
        if part == target {
            return true;
        }
    }
    false
}

fn is_likely_ut_or_st_item(asset: Option<&ObjectAsset>) -> bool {
    let Some(a) = asset else {
        return false;
    };
    let tokens = metadata_tokens(Some(a));
    if tokens.contains("UT") || tokens.contains("ST") {
        return true;
    }

    const EXCLUDED_TOKENS: &[&str] = &[
        "PET",
        "PETEGG",
        "EGG",
        "TOKEN",
        "MARK",
        "QUEST",
        "CONSUMABLE",
        "KEY",
        "DYE",
        "CLOTH",
        "SKIN",
        "EMOTE",
        "GRAVESTONE",
        "TITLE",
        "BLUEPRINT",
        "SCHEMATIC",
        "BOOSTER",
        "MODIFIER",
        "ENCHANTMENT",
        "TAROT",
        "TREASURE",
    ];
    for &ex in EXCLUDED_TOKENS {
        if tokens.contains(ex) {
            return false;
        }
    }

    let combined = format!("{} {}", a.group, a.labels).to_ascii_uppercase();
    const EXCLUDED_SUBSTRINGS: &[&str] = &[
        "CONSUMABLE",
        "TOKEN",
        "QUEST",
        "MARK",
        "EGG",
        "KEY",
        "SKIN",
        "DYE",
        "CLOTH",
        "PET",
    ];
    for &sub in EXCLUDED_SUBSTRINGS {
        if combined.contains(sub) {
            return false;
        }
    }

    combined.contains("UT") || combined.contains("ST")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assets::StatBonuses;

    fn make_test_asset(
        id: i32,
        id_name: &str,
        display_name: &str,
        group: &str,
        labels: &str,
    ) -> ObjectAsset {
        ObjectAsset {
            id,
            id_name: id_name.to_string(),
            display_name: display_name.to_string(),
            class: "Equipment".to_string(),
            group: group.to_string(),
            labels: labels.to_string(),
            textures: Vec::new(),
            projectiles: Vec::new(),
            mask: None,
            tex1: 0,
            tex2: 0,
            slot_type: 0,
            defense: 0,
            hitbox_scale: 1.0,
            tier: None,
            feed_power: 0,
            fame_bonus: 0,
            bag_type: 6,
            set_name: None,
            lethal_strike: None,
            collection_icon: None,
            rate_of_fire: 1.0,
            num_projectiles: 1,
            stat_bonuses: StatBonuses::default(),
            weapon_procs: Vec::new(),
        }
    }

    #[test]
    fn test_normalized_name_matches_bot_and_sniffer() {
        assert_eq!(normalized_name("   Doom   Bow  "), "doom bow");
        assert_eq!(
            normalized_name("Staff of the Vital Unity\u{2019}s Core - Test"),
            "staff of the vital unity's core-test"
        );
        assert_eq!(normalized_name(" Koala\u{2013}POW "), "koala-pow");
    }

    #[test]
    fn test_embedded_csv_loads_tracked_items() {
        let items =
            parse_tracked_items_csv(EMBEDDED_TRACKED_ITEMS_CSV).expect("valid embedded CSV");
        assert!(
            items.len() > 500,
            "expected >500 items, got {}",
            items.len()
        );
        assert!(items.contains(&normalized_name("Doom Bow")));
        assert!(items.contains(&normalized_name("Green Beehemoth Quiver")));
        assert!(items.contains(&normalized_name("Vial of Soul Extract")));
    }

    #[test]
    fn test_parse_item_name_shiny_and_override() {
        let tracked = parse_tracked_items_csv(EMBEDDED_TRACKED_ITEMS_CSV).unwrap();

        let shiny_vial = make_test_asset(
            5338,
            "Vial of Soul Extract Shiny",
            "Vial of Soul Extract",
            "",
            "EQUIPMENT,UT,SHINY",
        );
        let parsed = parse_item_name(5338, Some(&shiny_vial), &tracked);
        assert_eq!(parsed.base_name, "Vial of Soul Extract");
        assert!(parsed.shiny);
        assert!(parsed.tracked);

        let green_quiver = make_test_asset(
            17564,
            "Green Beehemoth Quiver",
            "Beehemoth Quiver",
            "Quivers",
            "EQUIPMENT,UT",
        );
        let parsed_q = parse_item_name(17564, Some(&green_quiver), &tracked);
        assert_eq!(parsed_q.base_name, "Green Beehemoth Quiver");
        assert!(!parsed_q.shiny);
        assert!(parsed_q.tracked);
    }

    #[test]
    fn test_resolve_rarity_and_payload_format() {
        let r0 = resolve_item_rarity(None, 0);
        assert_eq!(r0.rarity, "common");
        assert_eq!(r0.source, "enchant_count");

        let r4 = resolve_item_rarity(None, 4);
        assert_eq!(r4.rarity, "divine");
        assert_eq!(r4.source, "enchant_count");

        let json_str = build_loot_payload(
            123456789,
            "secret_token",
            "Doom Bow",
            true,
            0x0c02,
            42,
            "PlayerOne",
            "Archer",
            "Bows",
            "EQUIPMENT,UT,SHINY",
            "legendary",
            false,
            "Undead Lair",
            true,
            false,
        );
        let val: Value = serde_json::from_str(&json_str).unwrap();
        assert_eq!(val["guild_id"], 123456789);
        assert_eq!(val["link_token"], "secret_token");
        assert_eq!(val["item_name"], "Doom Bow");
        assert_eq!(val["shiny"], true);
        assert_eq!(val["item_id"], 0x0c02);
        assert_eq!(val["character_id"], 42);
        assert_eq!(val["character_name"], "PlayerOne");
        assert_eq!(val["character_class"], "Archer");
        assert_eq!(val["item_group"], "Bows");
        assert_eq!(val["item_label"], "EQUIPMENT,UT,SHINY");
        assert_eq!(val["item_rarity"], "legendary");
        assert_eq!(val["divine"], false);
        assert_eq!(val["dungeon"], "Undead Lair");
        assert_eq!(val["is_seasonal"], true);
        assert_eq!(val["loot_drop_bonus"], false);
        assert_eq!(val["source"], "tomato");
    }
}
