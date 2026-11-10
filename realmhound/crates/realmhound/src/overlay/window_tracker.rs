//! Cross-platform RotMG process/window state tracking, in-game area
//! classification, and Discord-style game-window attachment math.
//!
//! Supported platforms:
//! - **Windows**: Native Win32 window discovery for both `"RotMGExalt"` and
//!   `"RotMG Exalt"`, client-area attachment (`GetClientRect` + `ClientToScreen`),
//!   fullscreen topmost enforcement (`SetWindowPos(HWND_TOPMOST, SWP_NOACTIVATE)`),
//!   DWM hardware translucency (`SetLayeredWindowAttributes`), low-level mouse
//!   wheel capture (`WH_MOUSE_LL`) over the overlay, and non-activating mouse
//!   interaction (`WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW`) so dragging, the quick
//!   settings bar, `-` (minimize), `✕` (close), and mouse-wheel scrolling all
//!   work while RotMG is open without stealing focus.
//! - **Linux**: Process detection via `client_process` (`/proc`), plus optional
//!   X11 active-window focus check (`xprop`). Silently falls back on Wayland.
//! - **macOS**: Process detection via `client_process` (`libproc`) with safe
//!   fallback.

use std::time::{Duration, Instant};

use eframe::egui;
use realmhound_core::GameEvent;

use super::config::{OverlayAnchor, OverlayConfig, VisibilityTrigger};

/// Classification of the player's current in-game map.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AreaKind {
    /// Player has not entered a map in this session yet.
    #[default]
    Unknown,
    /// Nexus, Vault, Daily Quest Room, Pet Yard, Guild Hall, etc.
    SafeArea,
    /// Realm of the Mad God overworld.
    Realm,
    /// Any dungeon or boss encounter map.
    Dungeon,
}

impl AreaKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Unknown => "In-Game",
            Self::SafeArea => "Safe Area",
            Self::Realm => "Realm",
            Self::Dungeon => "Dungeon",
        }
    }

    pub fn badge_color(self) -> egui::Color32 {
        match self {
            Self::Unknown => egui::Color32::from_rgb(150, 160, 175),
            Self::SafeArea => egui::Color32::from_rgb(70, 200, 145),
            Self::Realm => egui::Color32::from_rgb(90, 175, 245),
            Self::Dungeon => egui::Color32::from_rgb(235, 145, 70),
        }
    }
}

/// Clean RotMG localization tokens like `"{s.nexus}"` or `"{s.rotmg}"` into
/// human-readable map names (`"Nexus"`, `"Realm"`, etc.).
pub fn clean_map_name(raw: &str) -> String {
    let mut s = raw.trim();
    if s.starts_with('{') && s.ends_with('}') && s.len() > 2 {
        s = &s[1..s.len() - 1];
    }
    if let Some(stripped) = s.strip_prefix("s.") {
        s = stripped;
    }
    match s.to_ascii_lowercase().as_str() {
        "nexus" => "Nexus".to_string(),
        "vault" => "Vault".to_string(),
        "rotmg" | "realm of the mad god" => "Realm".to_string(),
        "daily quest room" | "tinkerer" => "Tinkerer".to_string(),
        "pet yard" => "Pet Yard".to_string(),
        "guild hall" => "Guild Hall".to_string(),
        _ => {
            if s.is_empty() {
                String::new()
            } else {
                let mut chars = s.chars();
                match chars.next() {
                    Some(first) if first.is_ascii_lowercase() => {
                        format!("{}{}", first.to_ascii_uppercase(), chars.as_str())
                    }
                    _ => s.to_string(),
                }
            }
        }
    }
}

/// Classify a RotMG map name and boost-timer pause flag into an [`AreaKind`].
pub fn classify_area(map_name: &str, pauses_boost_timers: bool) -> AreaKind {
    let cleaned = clean_map_name(map_name);
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        return if pauses_boost_timers {
            AreaKind::SafeArea
        } else {
            AreaKind::Unknown
        };
    }

    let lower = trimmed.to_ascii_lowercase();
    if pauses_boost_timers
        || lower == "nexus"
        || lower == "vault"
        || lower == "daily quest room"
        || lower == "tinkerer"
        || lower.starts_with("pet yard")
        || lower.starts_with("guild hall")
        || lower.contains("bazaar")
        || lower.contains("nexus_tutorial")
    {
        return AreaKind::SafeArea;
    }

    if lower == "realm" || lower == "realm of the mad god" || lower.starts_with("realm") {
        return AreaKind::Realm;
    }

    AreaKind::Dungeon
}

/// Snapshot of the RotMG game process, window client rectangle, and in-game area.
#[derive(Debug, Clone)]
pub struct GameWindowState {
    /// True if a RotMG Exalt process is running or an active packet session is connected.
    pub rotmg_running: bool,
    /// True if the RotMG Exalt window is currently the OS foreground window.
    pub rotmg_focused: bool,
    /// True if the current OS/compositor backend supports per-window focus detection.
    pub focus_detection_supported: bool,
    /// Human-readable description of the active platform backend (shown in Settings).
    pub backend_status: &'static str,
    /// Inner game client canvas rectangle on screen (if detected and not minimized).
    pub game_window_rect: Option<egui::Rect>,
    /// Current in-game map name from the most recent `MAPINFO` packet.
    pub current_map_name: String,
    /// Classified area kind for the current map.
    pub area_kind: AreaKind,
    last_poll: Option<Instant>,
}

impl Default for GameWindowState {
    fn default() -> Self {
        Self {
            rotmg_running: false,
            rotmg_focused: false,
            focus_detection_supported: cfg!(windows),
            backend_status: default_backend_label(),
            game_window_rect: None,
            current_map_name: String::new(),
            area_kind: AreaKind::Unknown,
            last_poll: None,
        }
    }
}

const fn default_backend_label() -> &'static str {
    if cfg!(windows) {
        "Windows Win32 (Game Canvas Attach + Topmost + No-Activate)"
    } else if cfg!(target_os = "linux") {
        "Linux (Process + X11/Wayland Safe Fallback)"
    } else if cfg!(target_os = "macos") {
        "macOS (Process + Quartz/AppKit Safe Fallback)"
    } else {
        "Generic Fallback"
    }
}

impl GameWindowState {
    const POLL_INTERVAL: Duration = Duration::from_millis(200);

    /// Observe a live [`GameEvent`] broadcast to keep the current map name updated.
    pub fn on_game_event(&mut self, event: &GameEvent) {
        if let GameEvent::MapChanged {
            name,
            display_name,
            is_dungeon,
            ..
        } = event
        {
            let raw = if !display_name.trim().is_empty() {
                display_name.as_str()
            } else {
                name.as_str()
            };
            self.current_map_name = clean_map_name(raw);
            self.area_kind = if *is_dungeon {
                AreaKind::Dungeon
            } else {
                classify_area(&self.current_map_name, false)
            };
        }
    }

    /// Poll process and window state (throttled to [`Self::POLL_INTERVAL`]).
    /// Never panics or blocks the UI thread: OS process/window probing runs on
    /// a dedicated background thread and publishes snapshots into shared state.
    pub fn poll_throttled(&mut self, session_connected: bool, map_pauses_boost_timers: bool) {
        self.area_kind = classify_area(&self.current_map_name, map_pauses_boost_timers);

        let now = Instant::now();
        let should_kick = self
            .last_poll
            .is_none_or(|last| now.saturating_duration_since(last) >= Self::POLL_INTERVAL);
        if should_kick {
            self.last_poll = Some(now);
            bg_probe::request_poll(session_connected);
        }

        if let Some(snap) = bg_probe::latest_snapshot() {
            self.rotmg_running = session_connected || snap.rotmg_running;
            self.rotmg_focused = if snap.focus_detection_supported {
                snap.rotmg_focused
            } else {
                self.rotmg_running
            };
            self.focus_detection_supported = snap.focus_detection_supported;
            self.game_window_rect = snap.game_window_rect;
            self.backend_status = snap.backend_status;
        } else if session_connected {
            self.rotmg_running = true;
            self.rotmg_focused = true;
        }
    }
}

mod bg_probe {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Condvar, Mutex, OnceLock};
    use std::time::{Duration, Instant};

    use eframe::egui;

    #[derive(Debug, Clone)]
    pub struct ProbeSnapshot {
        pub rotmg_running: bool,
        pub rotmg_focused: bool,
        pub focus_detection_supported: bool,
        pub game_window_rect: Option<egui::Rect>,
        pub backend_status: &'static str,
    }

    struct BgPoller {
        wake_lock: Mutex<bool>,
        wake_cv: Condvar,
        session_connected: AtomicBool,
        latest: Mutex<Option<ProbeSnapshot>>,
    }

    static POLLER: OnceLock<BgPoller> = OnceLock::new();

    fn poller() -> &'static BgPoller {
        POLLER.get_or_init(|| {
            let _ = std::thread::Builder::new()
                .name("realmhound-overlay-probe".into())
                .spawn(worker_loop);
            BgPoller {
                wake_lock: Mutex::new(true),
                wake_cv: Condvar::new(),
                session_connected: AtomicBool::new(false),
                latest: Mutex::new(None),
            }
        })
    }

    pub fn request_poll(session_connected: bool) {
        let p = poller();
        p.session_connected
            .store(session_connected, Ordering::Relaxed);
        if let Ok(mut pending) = p.wake_lock.lock() {
            if !*pending {
                *pending = true;
                p.wake_cv.notify_one();
            }
        }
    }

    pub fn latest_snapshot() -> Option<ProbeSnapshot> {
        let p = poller();
        p.latest.lock().ok().and_then(|g| g.clone())
    }

    fn worker_loop() {
        let mut last_proc_scan: Option<Instant> = None;
        let mut cached_proc_running = false;

        loop {
            let p = poller();
            {
                let Ok(mut pending) = p.wake_lock.lock() else {
                    std::thread::sleep(Duration::from_millis(200));
                    continue;
                };
                while !*pending {
                    match p.wake_cv.wait_timeout(pending, Duration::from_secs(2)) {
                        Ok((guard, _)) => {
                            pending = guard;
                        }
                        Err(poisoned) => {
                            pending = poisoned.into_inner().0;
                            break;
                        }
                    }
                }
                *pending = false;
            }

            let session_connected = p.session_connected.load(Ordering::Relaxed);
            let snap = run_platform_probe(
                session_connected,
                &mut last_proc_scan,
                &mut cached_proc_running,
            );
            if let Ok(mut guard) = p.latest.lock() {
                *guard = Some(snap);
            }
        }
    }

    fn check_proc_running_throttled(
        session_connected: bool,
        last_proc_scan: &mut Option<Instant>,
        cached_proc_running: &mut bool,
    ) -> bool {
        if session_connected {
            *cached_proc_running = true;
            return true;
        }
        let now = Instant::now();
        let due = last_proc_scan
            .is_none_or(|last| now.saturating_duration_since(last) >= Duration::from_millis(1500));
        if due {
            *last_proc_scan = Some(now);
            *cached_proc_running = !matches!(
                crate::client_process::earliest_rotmg_client_start_unix(),
                crate::client_process::ClientStart::NoneRunning
            );
        }
        *cached_proc_running
    }

    fn run_platform_probe(
        session_connected: bool,
        last_proc_scan: &mut Option<Instant>,
        cached_proc_running: &mut bool,
    ) -> ProbeSnapshot {
        #[cfg(windows)]
        {
            let win_probe = super::win32_probe::probe_rotmg_window();
            let rotmg_running = if win_probe.window_found || session_connected {
                *cached_proc_running = true;
                true
            } else {
                check_proc_running_throttled(
                    session_connected,
                    last_proc_scan,
                    cached_proc_running,
                )
            };
            let rotmg_focused = if win_probe.window_found {
                win_probe.is_foreground || win_probe.overlay_is_foreground
            } else {
                rotmg_running
            };
            return ProbeSnapshot {
                rotmg_running,
                rotmg_focused,
                focus_detection_supported: true,
                game_window_rect: win_probe.rect,
                backend_status: "Windows Win32 (Game Canvas Attach + Topmost + No-Activate)",
            };
        }

        #[cfg(target_os = "linux")]
        {
            let proc_running = check_proc_running_throttled(
                session_connected,
                last_proc_scan,
                cached_proc_running,
            );
            let mut rotmg_running = session_connected || proc_running;
            let linux_probe = super::linux_probe::probe_rotmg_window(rotmg_running);
            if linux_probe.window_found {
                rotmg_running = true;
            }
            let rotmg_focused = if linux_probe.focus_supported {
                linux_probe.is_foreground
            } else {
                rotmg_running
            };
            return ProbeSnapshot {
                rotmg_running,
                rotmg_focused,
                focus_detection_supported: linux_probe.focus_supported,
                game_window_rect: linux_probe.rect,
                backend_status: linux_probe.backend_status,
            };
        }

        #[cfg(target_os = "macos")]
        {
            let proc_running = check_proc_running_throttled(
                session_connected,
                last_proc_scan,
                cached_proc_running,
            );
            let rotmg_running = session_connected || proc_running;
            let mac_probe = super::macos_probe::probe_rotmg_window(rotmg_running);
            let rotmg_focused = if mac_probe.focus_supported {
                mac_probe.is_foreground
            } else {
                rotmg_running
            };
            return ProbeSnapshot {
                rotmg_running,
                rotmg_focused,
                focus_detection_supported: mac_probe.focus_supported,
                game_window_rect: None,
                backend_status: mac_probe.backend_status,
            };
        }

        #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
        {
            let proc_running = check_proc_running_throttled(
                session_connected,
                last_proc_scan,
                cached_proc_running,
            );
            let rotmg_running = session_connected || proc_running;
            ProbeSnapshot {
                rotmg_running,
                rotmg_focused: rotmg_running,
                focus_detection_supported: false,
                game_window_rect: None,
                backend_status: "Generic Process Fallback",
            }
        }
    }
}

/// Pure predicate deciding whether the overlay should be displayed this frame.
pub fn should_show_overlay(
    cfg: &OverlayConfig,
    state: &GameWindowState,
    preview_override: bool,
    unlocked_for_drag: bool,
    has_items_to_show: bool,
) -> bool {
    if preview_override || unlocked_for_drag {
        return true;
    }
    if !cfg.enabled {
        return false;
    }
    if cfg.hide_when_empty && !has_items_to_show && !cfg.minimized {
        return false;
    }

    let trigger_ok = match cfg.trigger {
        VisibilityTrigger::AlwaysVisible => true,
        VisibilityTrigger::WhenRotmgRunning => state.rotmg_running,
        VisibilityTrigger::WhenRotmgFocused => {
            if state.focus_detection_supported {
                state.rotmg_focused
            } else {
                state.rotmg_running
            }
        }
    };
    if !trigger_ok {
        return false;
    }

    match state.area_kind {
        AreaKind::SafeArea => cfg.show_in_safe_areas,
        AreaKind::Realm => cfg.show_in_realm,
        AreaKind::Dungeon => cfg.show_in_dungeons,
        AreaKind::Unknown => cfg.show_in_safe_areas || cfg.show_in_realm || cfg.show_in_dungeons,
    }
}

/// Compute the target top-left screen position of the overlay window given the
/// game client canvas (or primary monitor) bounds and the overlay's outer size.
pub fn compute_overlay_position(
    cfg: &OverlayConfig,
    bounds: egui::Rect,
    overlay_size: egui::Vec2,
) -> egui::Pos2 {
    let w = overlay_size.x.max(64.0);
    let h = overlay_size.y.max(24.0);
    let ox = cfg.offset_x.max(0.0);
    let oy = cfg.offset_y.max(0.0);

    let raw = match cfg.anchor {
        OverlayAnchor::TopLeft => egui::pos2(bounds.left() + ox, bounds.top() + oy),
        OverlayAnchor::BottomLeft => egui::pos2(bounds.left() + ox, bounds.bottom() - h - oy),
        OverlayAnchor::TopRight => egui::pos2(bounds.right() - w - ox, bounds.top() + oy),
        OverlayAnchor::BottomRight => egui::pos2(bounds.right() - w - ox, bounds.bottom() - h - oy),
        OverlayAnchor::TopCenter => {
            egui::pos2(bounds.center().x - w * 0.5 + ox, bounds.top() + oy)
        }
        OverlayAnchor::Custom => egui::pos2(cfg.custom_x, cfg.custom_y),
    };

    if cfg.anchor == OverlayAnchor::Custom {
        raw
    } else {
        let max_x = (bounds.right() - w).max(bounds.left());
        let max_y = (bounds.bottom() - h).max(bounds.top());
        egui::pos2(
            raw.x.clamp(bounds.left(), max_x),
            raw.y.clamp(bounds.top(), max_y),
        )
    }
}

/// Drain any mouse-wheel scroll delta captured by the platform hook while the
/// cursor was hovering over the overlay (in logical pixels, positive = scroll up).
pub fn take_overlay_wheel_scroll_px() -> f32 {
    #[cfg(windows)]
    {
        win32_probe::take_wheel_scroll_px()
    }
    #[cfg(not(windows))]
    {
        0.0
    }
}

/// Query the current global screen cursor position `(x, y)` (physical pixels)
/// on Windows so header-bar dragging works smoothly in screen space while
/// RotMG is open.
pub fn query_screen_cursor_pos() -> Option<egui::Pos2> {
    #[cfg(windows)]
    {
        win32_probe::cursor_screen_pos()
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// Query whether the primary (left) mouse button is physically held right now,
/// independent of which window has focus or mouse capture. Returns `None` on
/// platforms where a global query is unavailable (callers fall back to egui's
/// pointer state).
pub fn primary_mouse_button_down() -> Option<bool> {
    #[cfg(windows)]
    {
        Some(win32_probe::primary_button_down())
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// Notify platform tracker when the overlay viewport is hidden so wheel hooks do
/// not intercept scrolling when the overlay is closed.
pub fn notify_overlay_hidden() {
    #[cfg(windows)]
    win32_probe::disable_wheel_capture();
}

/// Allow the overlay window to take keyboard focus (needed for typing in the
/// Notes tab). When `false`, the overlay stays non-activating so clicking it
/// never steals focus from RotMG.
pub fn set_overlay_allow_activate(allow: bool) {
    #[cfg(windows)]
    win32_probe::set_allow_activate(allow);
    #[cfg(not(windows))]
    let _ = allow;
}

/// Apply platform-specific window flags (`WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW |
/// WS_EX_LAYERED`, `SetLayeredWindowAttributes` for true DWM translucency over
/// the game, topmost assertion for fullscreen games, and header-bar/scrollbar
/// hit-testing when `body_click_through` is enabled) and return `(passthrough, cursor_inside_overlay)`.
pub fn sync_platform_overlay_window(
    body_click_through: bool,
    interactive_top_height_px: f32,
    bg_opacity_pct: u8,
    overlay_minimized: bool,
    is_dragging: bool,
) -> (bool, bool) {
    #[cfg(windows)]
    {
        win32_probe::sync_overlay_hwnd(
            body_click_through,
            interactive_top_height_px,
            bg_opacity_pct,
            overlay_minimized,
            is_dragging,
        )
    }
    #[cfg(not(windows))]
    {
        // winit cannot hit-test only part of a window (header vs. body) and we
        // cannot observe the global cursor while the window is fully
        // passthrough, so enabling passthrough here would make the header,
        // drag handle, quick settings bar, and scrolling permanently
        // unreachable. Keep the (small) overlay interactive instead.
        let _ = (
            body_click_through,
            interactive_top_height_px,
            bg_opacity_pct,
            overlay_minimized,
            is_dragging,
        );
        (false, false)
    }
}

#[cfg(windows)]
mod win32_probe {
    use std::sync::atomic::{AtomicBool, AtomicI32, AtomicI64, AtomicIsize, Ordering};

    use eframe::egui;
    use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
    use windows_sys::Win32::Graphics::Gdi::ClientToScreen;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON, VK_RBUTTON};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, FindWindowW, GetClientRect, GetCursorPos, GetForegroundWindow,
        GetSystemMetrics, GetWindowLongPtrW, GetWindowRect, GetWindowTextW, IsIconic, IsWindow,
        IsWindowVisible, SetLayeredWindowAttributes, SetWindowLongPtrW, SetWindowPos,
        SetWindowsHookExW, GWL_EXSTYLE, HWND_TOPMOST, LWA_ALPHA, MSLLHOOKSTRUCT, SM_SWAPBUTTON,
        SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW, WH_MOUSE_LL, WM_LBUTTONDOWN,
        WM_MOUSEMOVE, WM_MOUSEWHEEL, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
        WS_EX_TOPMOST, WS_EX_TRANSPARENT,
    };

    static HOOK_INSTALLED: AtomicBool = AtomicBool::new(false);
    static OVERLAY_HWND: AtomicIsize = AtomicIsize::new(0);
    static OVERLAY_ACTIVE_FOR_WHEEL: AtomicBool = AtomicBool::new(false);
    static OVERLAY_BODY_CLICK_THROUGH: AtomicBool = AtomicBool::new(false);
    static OVERLAY_DRAGGING: AtomicBool = AtomicBool::new(false);
    static OVERLAY_TOP_ZONE_PX: AtomicI32 = AtomicI32::new(32);
    static OVERLAY_RECT_LEFT: AtomicI32 = AtomicI32::new(0);
    static OVERLAY_RECT_TOP: AtomicI32 = AtomicI32::new(0);
    static OVERLAY_RECT_RIGHT: AtomicI32 = AtomicI32::new(0);
    static OVERLAY_RECT_BOTTOM: AtomicI32 = AtomicI32::new(0);
    static WHEEL_ACCUM_DELTA: AtomicI32 = AtomicI32::new(0);

    unsafe fn apply_exstyle_passthrough(hwnd: HWND, passthrough: bool) {
        if hwnd == 0 {
            return;
        }
        let cur = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as usize;
        let mut target = cur
            | (WS_EX_TOOLWINDOW as usize)
            | (WS_EX_TOPMOST as usize)
            | (WS_EX_LAYERED as usize);
        // The Notes tab needs keyboard focus for typing; every other tab keeps
        // the overlay non-activating so RotMG never loses focus.
        if OVERLAY_ALLOW_ACTIVATE.load(Ordering::Relaxed) {
            target &= !(WS_EX_NOACTIVATE as usize);
        } else {
            target |= WS_EX_NOACTIVATE as usize;
        }
        if passthrough {
            target |= WS_EX_TRANSPARENT as usize;
        } else {
            target &= !(WS_EX_TRANSPARENT as usize);
        }
        if target != cur {
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, target as isize);
        }
    }

    unsafe extern "system" fn low_level_mouse_proc(
        n_code: i32,
        w_param: WPARAM,
        l_param: LPARAM,
    ) -> LRESULT {
        if n_code >= 0 && l_param != 0 {
            let msg = w_param as u32;
            let info = &*(l_param as *const MSLLHOOKSTRUCT);
            let left = OVERLAY_RECT_LEFT.load(Ordering::Relaxed);
            let top = OVERLAY_RECT_TOP.load(Ordering::Relaxed);
            let right = OVERLAY_RECT_RIGHT.load(Ordering::Relaxed);
            let bottom = OVERLAY_RECT_BOTTOM.load(Ordering::Relaxed);
            let inside = right > left
                && bottom > top
                && info.pt.x >= left
                && info.pt.x <= right
                && info.pt.y >= top
                && info.pt.y <= bottom;

            // Update WS_EX_TRANSPARENT before Windows hit-tests the mouse event
            // so moving onto the header/quick-settings bar and clicking or
            // dragging works with zero frame delay while RotMG is open.
            if msg == WM_MOUSEMOVE || msg == WM_LBUTTONDOWN {
                let hwnd = OVERLAY_HWND.load(Ordering::Relaxed) as HWND;
                if hwnd != 0 {
                    let body_ct = OVERLAY_BODY_CLICK_THROUGH.load(Ordering::Relaxed);
                    let dragging = OVERLAY_DRAGGING.load(Ordering::Relaxed);
                    let top_px = OVERLAY_TOP_ZONE_PX.load(Ordering::Relaxed).max(28);
                    let in_interactive = dragging
                        || (inside
                            && (info.pt.y <= (top + top_px).min(bottom)
                                || info.pt.x >= (right - 16).max(left)));
                    apply_exstyle_passthrough(hwnd, body_ct && !in_interactive);
                }
            }

            if msg == WM_MOUSEWHEEL && OVERLAY_ACTIVE_FOR_WHEEL.load(Ordering::Relaxed) && inside {
                let delta = ((info.mouseData >> 16) as u16) as i16 as i32;
                if delta != 0 {
                    WHEEL_ACCUM_DELTA.fetch_add(delta, Ordering::Relaxed);
                    // Swallow the wheel event so RotMG doesn't zoom the camera while
                    // scrolling the mission/quest list!
                    return 1;
                }
            }
        }
        CallNextHookEx(0, n_code, w_param, l_param)
    }

    fn ensure_wheel_hook_installed() {
        if !HOOK_INSTALLED.swap(true, Ordering::SeqCst) {
            unsafe {
                let _ = SetWindowsHookExW(WH_MOUSE_LL, Some(low_level_mouse_proc), 0, 0);
            }
        }
    }

    pub fn take_wheel_scroll_px() -> f32 {
        let raw = WHEEL_ACCUM_DELTA.swap(0, Ordering::Relaxed);
        if raw == 0 {
            0.0
        } else {
            (raw as f32 / 120.0) * 42.0
        }
    }

    pub fn disable_wheel_capture() {
        OVERLAY_ACTIVE_FOR_WHEEL.store(false, Ordering::Relaxed);
        OVERLAY_HWND.store(0, Ordering::Relaxed);
    }

    pub fn cursor_screen_pos() -> Option<egui::Pos2> {
        unsafe {
            let mut pt = POINT { x: 0, y: 0 };
            if GetCursorPos(&mut pt) != 0 {
                Some(egui::pos2(pt.x as f32, pt.y as f32))
            } else {
                None
            }
        }
    }

    pub fn primary_button_down() -> bool {
        unsafe {
            // `GetAsyncKeyState` reports physical buttons, so honour the
            // "swap primary and secondary buttons" accessibility setting.
            let vk = if GetSystemMetrics(SM_SWAPBUTTON) != 0 {
                VK_RBUTTON
            } else {
                VK_LBUTTON
            };
            (GetAsyncKeyState(vk as i32) as u16 & 0x8000) != 0
        }
    }

    pub struct WinProbeResult {
        pub window_found: bool,
        pub is_foreground: bool,
        pub overlay_is_foreground: bool,
        pub rect: Option<egui::Rect>,
    }

    fn wide_null(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn window_title(hwnd: HWND) -> String {
        if hwnd == 0 {
            return String::new();
        }
        unsafe {
            let mut buf = [0u16; 256];
            let len = GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32);
            if len > 0 {
                String::from_utf16_lossy(&buf[..len as usize])
            } else {
                String::new()
            }
        }
    }

    fn is_rotmg_title(title: &str) -> bool {
        let lower = title.trim().to_ascii_lowercase();
        lower == "rotmgexalt" || lower == "rotmg exalt" || lower.starts_with("rotmg")
    }

    fn find_rotmg_hwnd(fg: HWND) -> HWND {
        unsafe {
            let t1 = wide_null("RotMGExalt");
            let h1 = FindWindowW(std::ptr::null(), t1.as_ptr());
            if h1 != 0 && IsWindowVisible(h1) != 0 {
                return h1;
            }
            let t2 = wide_null("RotMG Exalt");
            let h2 = FindWindowW(std::ptr::null(), t2.as_ptr());
            if h2 != 0 && IsWindowVisible(h2) != 0 {
                return h2;
            }
            if fg != 0 && IsWindowVisible(fg) != 0 && is_rotmg_title(&window_title(fg)) {
                return fg;
            }
            let cls = wide_null("UnityWndClass");
            let h3 = FindWindowW(cls.as_ptr(), std::ptr::null());
            if h3 != 0 && IsWindowVisible(h3) != 0 && is_rotmg_title(&window_title(h3)) {
                return h3;
            }
            0
        }
    }

    fn client_screen_rect(hwnd: HWND) -> Option<egui::Rect> {
        unsafe {
            let mut cr = RECT {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            };
            let mut pt = POINT { x: 0, y: 0 };
            if GetClientRect(hwnd, &mut cr) != 0
                && cr.right > cr.left
                && cr.bottom > cr.top
                && ClientToScreen(hwnd, &mut pt) != 0
            {
                let left = pt.x as f32;
                let top = pt.y as f32;
                let width = (cr.right - cr.left) as f32;
                let height = (cr.bottom - cr.top) as f32;
                return Some(egui::Rect::from_min_size(
                    egui::pos2(left, top),
                    egui::vec2(width, height),
                ));
            }

            let mut wr = RECT {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            };
            if GetWindowRect(hwnd, &mut wr) != 0 && wr.right > wr.left && wr.bottom > wr.top {
                Some(egui::Rect::from_min_max(
                    egui::pos2(wr.left as f32, wr.top as f32),
                    egui::pos2(wr.right as f32, wr.bottom as f32),
                ))
            } else {
                None
            }
        }
    }

    pub fn probe_rotmg_window() -> WinProbeResult {
        unsafe {
            let fg: HWND = GetForegroundWindow();
            let hwnd: HWND = find_rotmg_hwnd(fg);
            if hwnd == 0 {
                return WinProbeResult {
                    window_found: false,
                    is_foreground: false,
                    overlay_is_foreground: false,
                    rect: None,
                };
            }

            let overlay_title = wide_null("RealmHound Overlay");
            let overlay_hwnd: HWND = FindWindowW(std::ptr::null(), overlay_title.as_ptr());

            let is_foreground = fg != 0 && fg == hwnd;
            let overlay_is_foreground = fg != 0 && overlay_hwnd != 0 && fg == overlay_hwnd;

            let rect = if IsIconic(hwnd) == 0 {
                client_screen_rect(hwnd)
            } else {
                None
            };

            WinProbeResult {
                window_found: true,
                is_foreground,
                overlay_is_foreground,
                rect,
            }
        }
    }

    /// Cached overlay HWND (validated with `IsWindow`) so the per-frame sync never
    /// walks the desktop's top-level windows with `FindWindowW`.
    static CACHED_OVERLAY_HWND: AtomicIsize = AtomicIsize::new(0);
    /// Last alpha applied with `SetLayeredWindowAttributes` (0 = never applied).
    static LAST_APPLIED_ALPHA: AtomicI32 = AtomicI32::new(-1);
    /// Last topmost assertion time in ms since `TOPMOST_EPOCH`.
    static LAST_TOPMOST_MS: AtomicI64 = AtomicI64::new(i64::MIN);
    /// When true the overlay may take keyboard focus (Notes tab text input).
    static OVERLAY_ALLOW_ACTIVATE: AtomicBool = AtomicBool::new(false);

    fn now_ms() -> i64 {
        use std::sync::OnceLock;
        use std::time::Instant;
        static EPOCH: OnceLock<Instant> = OnceLock::new();
        EPOCH.get_or_init(Instant::now).elapsed().as_millis() as i64
    }

    fn resolve_overlay_hwnd() -> HWND {
        unsafe {
            let cached = CACHED_OVERLAY_HWND.load(Ordering::Relaxed) as HWND;
            if cached != 0 && IsWindow(cached) != 0 {
                return cached;
            }
            let overlay_title = wide_null("RealmHound Overlay");
            let hwnd: HWND = FindWindowW(std::ptr::null(), overlay_title.as_ptr());
            CACHED_OVERLAY_HWND.store(hwnd as isize, Ordering::Relaxed);
            if hwnd != cached {
                // New window: force alpha + topmost to be re-applied.
                LAST_APPLIED_ALPHA.store(-1, Ordering::Relaxed);
                LAST_TOPMOST_MS.store(i64::MIN, Ordering::Relaxed);
            }
            hwnd
        }
    }

    /// Allow (or forbid) the overlay window to take keyboard focus. Used by the
    /// Notes tab so typing reaches the overlay's text box.
    pub fn set_allow_activate(allow: bool) {
        OVERLAY_ALLOW_ACTIVATE.store(allow, Ordering::Relaxed);
    }

    pub fn sync_overlay_hwnd(
        body_click_through: bool,
        interactive_top_height_px: f32,
        bg_opacity_pct: u8,
        overlay_minimized: bool,
        is_dragging: bool,
    ) -> (bool, bool) {
        ensure_wheel_hook_installed();
        unsafe {
            let hwnd: HWND = resolve_overlay_hwnd();
            if hwnd == 0 {
                OVERLAY_HWND.store(0, Ordering::Relaxed);
                OVERLAY_ACTIVE_FOR_WHEEL.store(false, Ordering::Relaxed);
                return (body_click_through && !is_dragging, false);
            }
            OVERLAY_HWND.store(hwnd as isize, Ordering::Relaxed);
            OVERLAY_BODY_CLICK_THROUGH.store(body_click_through, Ordering::Relaxed);
            OVERLAY_DRAGGING.store(is_dragging, Ordering::Relaxed);
            let top_px = (interactive_top_height_px.ceil() as i32).max(28);
            OVERLAY_TOP_ZONE_PX.store(top_px, Ordering::Relaxed);

            let mut cursor_in_interactive_zone = is_dragging;
            let mut inside_window = false;
            let mut wr = RECT {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            };
            let mut pt = POINT { x: 0, y: 0 };
            if GetWindowRect(hwnd, &mut wr) != 0 {
                OVERLAY_RECT_LEFT.store(wr.left, Ordering::Relaxed);
                OVERLAY_RECT_TOP.store(wr.top, Ordering::Relaxed);
                OVERLAY_RECT_RIGHT.store(wr.right, Ordering::Relaxed);
                OVERLAY_RECT_BOTTOM.store(wr.bottom, Ordering::Relaxed);
                OVERLAY_ACTIVE_FOR_WHEEL.store(!overlay_minimized, Ordering::Relaxed);

                if GetCursorPos(&mut pt) != 0 {
                    inside_window =
                        pt.x >= wr.left && pt.x <= wr.right && pt.y >= wr.top && pt.y <= wr.bottom;
                    if inside_window {
                        let top_zone_bottom = wr.top + top_px;
                        let in_top_zone = pt.y <= top_zone_bottom.min(wr.bottom);
                        let in_scrollbar = pt.x >= (wr.right - 16).max(wr.left);
                        if in_top_zone || in_scrollbar {
                            cursor_in_interactive_zone = true;
                        }
                    }
                }
            } else {
                OVERLAY_ACTIVE_FOR_WHEEL.store(false, Ordering::Relaxed);
            }

            let passthrough = body_click_through && !cursor_in_interactive_zone;
            apply_exstyle_passthrough(hwnd, passthrough);

            // Apply hardware DWM layered window alpha so the OpenGL overlay
            // window is genuinely translucent over RotMG underneath.
            // Only touched when the alpha actually changes, so DWM is not forced
            // to recompose the layered window every frame.
            let norm = (bg_opacity_pct.clamp(10, 98) as f32) / 100.0;
            let win_alpha = ((0.14 + norm * 0.84) * 255.0).round().clamp(35.0, 250.0) as u8;
            if LAST_APPLIED_ALPHA.swap(win_alpha as i32, Ordering::Relaxed) != win_alpha as i32 {
                let _ = SetLayeredWindowAttributes(hwnd, 0, win_alpha, LWA_ALPHA);
            }

            // Re-assert topmost only when it was lost, or on a slow heartbeat
            // (fullscreen games can silently push us down the z-order).
            let now = now_ms();
            let exstyle = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
            let lost_topmost = exstyle & WS_EX_TOPMOST == 0;
            let last = LAST_TOPMOST_MS.load(Ordering::Relaxed);
            if lost_topmost || last == i64::MIN || now.saturating_sub(last) >= 750 {
                LAST_TOPMOST_MS.store(now, Ordering::Relaxed);
                let show = if last == i64::MIN { SWP_SHOWWINDOW } else { 0 };
                SetWindowPos(
                    hwnd,
                    HWND_TOPMOST,
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | show,
                );
            }

            (passthrough, inside_window)
        }
    }
}

#[cfg(target_os = "linux")]
mod linux_probe {
    use std::process::Command;
    use std::sync::atomic::{AtomicU8, Ordering};

    use eframe::egui;

    static XPROP_STATE: AtomicU8 = AtomicU8::new(0);

    pub struct LinuxProbeResult {
        pub window_found: bool,
        pub focus_supported: bool,
        pub is_foreground: bool,
        pub rect: Option<egui::Rect>,
        pub backend_status: &'static str,
    }

    pub fn probe_rotmg_window(rotmg_running: bool) -> LinuxProbeResult {
        let is_wayland = std::env::var_os("WAYLAND_DISPLAY").is_some();
        let has_x11_display = std::env::var_os("DISPLAY").is_some();

        if !rotmg_running || !has_x11_display {
            return LinuxProbeResult {
                window_found: false,
                focus_supported: false,
                is_foreground: rotmg_running,
                rect: None,
                backend_status: if is_wayland {
                    "Linux Wayland (Process Trigger Fallback)"
                } else {
                    "Linux (Process Trigger)"
                },
            };
        }

        if XPROP_STATE.load(Ordering::Relaxed) == 2 {
            return LinuxProbeResult {
                window_found: false,
                focus_supported: false,
                is_foreground: rotmg_running,
                rect: None,
                backend_status: "Linux (Process Trigger — xprop not installed)",
            };
        }

        let output = match Command::new("xprop")
            .args(["-root", "_NET_ACTIVE_WINDOW"])
            .output()
        {
            Ok(out) if out.status.success() => {
                XPROP_STATE.store(1, Ordering::Relaxed);
                String::from_utf8_lossy(&out.stdout).into_owned()
            }
            _ => {
                XPROP_STATE.store(2, Ordering::Relaxed);
                return LinuxProbeResult {
                    window_found: false,
                    focus_supported: false,
                    is_foreground: rotmg_running,
                    rect: None,
                    backend_status: "Linux (Process Trigger Fallback)",
                };
            }
        };

        let Some(win_id) = output
            .split_whitespace()
            .last()
            .filter(|s| s.starts_with("0x") && *s != "0x0")
        else {
            return LinuxProbeResult {
                window_found: false,
                focus_supported: false,
                is_foreground: rotmg_running,
                rect: None,
                backend_status: if is_wayland {
                    "Linux XWayland/Wayland (Process Trigger Fallback)"
                } else {
                    "Linux X11 (Active Window Idle)"
                },
            };
        };

        if let Ok(title_out) = Command::new("xprop")
            .args(["-id", win_id, "_NET_WM_NAME", "WM_NAME"])
            .output()
        {
            if title_out.status.success() {
                let text = String::from_utf8_lossy(&title_out.stdout);
                let lower = text.to_ascii_lowercase();
                let is_rotmg = lower.contains("rotmgexalt") || lower.contains("rotmg exalt");
                let is_overlay = lower.contains("realmhound overlay");
                return LinuxProbeResult {
                    window_found: is_rotmg,
                    focus_supported: !is_wayland,
                    is_foreground: is_rotmg || is_overlay,
                    rect: None,
                    backend_status: if is_wayland {
                        "Linux Wayland/XWayland (Process + X11 Hybrid)"
                    } else {
                        "Linux X11 (Active Window Focus Supported)"
                    },
                };
            }
        }

        LinuxProbeResult {
            window_found: false,
            focus_supported: false,
            is_foreground: rotmg_running,
            rect: None,
            backend_status: "Linux (Process Trigger Fallback)",
        }
    }
}

#[cfg(target_os = "macos")]
mod macos_probe {
    pub struct MacProbeResult {
        pub focus_supported: bool,
        pub is_foreground: bool,
        pub backend_status: &'static str,
    }

    pub fn probe_rotmg_window(rotmg_running: bool) -> MacProbeResult {
        MacProbeResult {
            focus_supported: false,
            is_foreground: rotmg_running,
            backend_status: "macOS (Process Trigger + Floating Overlay)",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_rotmg_maps_and_cleans_tokens() {
        assert_eq!(clean_map_name("{s.nexus}"), "Nexus");
        assert_eq!(clean_map_name("{s.rotmg}"), "Realm");
        assert_eq!(classify_area("{s.nexus}", false), AreaKind::SafeArea);
        assert_eq!(classify_area("Nexus", false), AreaKind::SafeArea);
        assert_eq!(classify_area("Vault", false), AreaKind::SafeArea);
        assert_eq!(classify_area("Daily Quest Room", false), AreaKind::SafeArea);
        assert_eq!(classify_area("Pet Yard", false), AreaKind::SafeArea);
        assert_eq!(classify_area("Some Custom Hub", true), AreaKind::SafeArea);
        assert_eq!(
            classify_area("Realm of the Mad God", false),
            AreaKind::Realm
        );
        assert_eq!(classify_area("The Shatters", false), AreaKind::Dungeon);
        assert_eq!(classify_area("Oryx's Sanctuary", false), AreaKind::Dungeon);
        assert_eq!(classify_area("", false), AreaKind::Unknown);
    }

    #[test]
    fn visibility_trigger_and_area_filters_work() {
        let mut cfg = OverlayConfig {
            enabled: true,
            trigger: VisibilityTrigger::WhenRotmgFocused,
            ..OverlayConfig::default()
        };
        let mut state = GameWindowState {
            rotmg_running: true,
            rotmg_focused: false,
            focus_detection_supported: true,
            backend_status: "Test",
            game_window_rect: None,
            current_map_name: "Realm".to_string(),
            area_kind: AreaKind::Realm,
            last_poll: None,
        };

        assert!(!should_show_overlay(&cfg, &state, false, false, true));
        assert!(should_show_overlay(&cfg, &state, true, false, true));

        state.rotmg_focused = true;
        assert!(should_show_overlay(&cfg, &state, false, false, true));

        state.rotmg_focused = false;
        state.focus_detection_supported = false;
        assert!(should_show_overlay(&cfg, &state, false, false, true));
        state.focus_detection_supported = true;
        state.rotmg_focused = true;

        cfg.show_in_realm = false;
        assert!(!should_show_overlay(&cfg, &state, false, false, true));

        state.area_kind = AreaKind::Dungeon;
        assert!(should_show_overlay(&cfg, &state, false, false, true));

        cfg.hide_when_empty = true;
        assert!(!should_show_overlay(&cfg, &state, false, false, false));
        assert!(should_show_overlay(&cfg, &state, false, false, true));
    }

    #[test]
    fn anchor_positioning_clamps_within_bounds() {
        let mut cfg = OverlayConfig {
            anchor: OverlayAnchor::TopRight,
            offset_x: 20.0,
            offset_y: 30.0,
            ..OverlayConfig::default()
        };
        let bounds =
            egui::Rect::from_min_max(egui::pos2(100.0, 50.0), egui::pos2(2020.0, 1130.0));
        let size = egui::vec2(300.0, 200.0);

        let pos = compute_overlay_position(&cfg, bounds, size);
        assert_eq!(pos, egui::pos2(2020.0 - 300.0 - 20.0, 50.0 + 30.0));

        cfg.anchor = OverlayAnchor::BottomLeft;
        let pos_bl = compute_overlay_position(&cfg, bounds, size);
        assert_eq!(pos_bl, egui::pos2(100.0 + 20.0, 1130.0 - 200.0 - 30.0));
    }
}
