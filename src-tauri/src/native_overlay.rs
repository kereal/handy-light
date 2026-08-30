//! Native Windows recording-overlay indicator.
//!
//! A lightweight Win32 window that replaces the WebView-based
//! `recording_overlay` on Windows. The WebView overlay kept an entire
//! WebView2/Edge subprocess resident at all times just to draw a small pill
//! ("● Recording…") for the few seconds the user dictates. This module draws
//! the same pill with plain GDI on a topmost tool window, created lazily in
//! `show` and destroyed in `hide`, so no WebView — and no second Edge
//! instance — is kept alive for the overlay.
//!
//! Threading: a dedicated thread owns the window and its message pump. The
//! window lives for the duration of one recording cycle; it is created on
//! show and destroyed on hide. The window class, once registered, stays
//! registered for the rest of the process lifetime: registration is guarded
//! by a `Once`, so unregistering it on teardown would make every later
//! `CreateWindowExW` fail with ERROR_CLASS_DOES_NOT_EXIST (the pill would
//! appear on the first recording after launch and never again).

#![cfg(target_os = "windows")]

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Mutex, Once};
use std::thread;

use log::error;
use tauri::{AppHandle, PhysicalPosition, PhysicalSize};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateFontW, CreatePen, CreateSolidBrush, DeleteObject, DrawTextW, Ellipse,
    EndPaint, FillRect, GetMonitorInfoW, GetStockObject, InvalidateRect, MonitorFromWindow,
    SelectObject, SetBkMode, SetTextColor, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET,
    DEFAULT_PITCH, DRAW_TEXT_FORMAT, DT_NOPREFIX, DT_SINGLELINE, DT_VCENTER, FF_DONTCARE,
    FW_NORMAL, HDC, HGDIOBJ, MONITORINFO, MONITOR_DEFAULTTONEAREST, NULL_PEN, OUT_DEFAULT_PRECIS,
    PAINTSTRUCT, PS_SOLID, TRANSPARENT,
};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetClientRect, GetMessageW,
    GetWindowTextW, KillTimer, LoadCursorW, RegisterClassExW, SetTimer, SetWindowPos,
    SetWindowTextW, ShowWindow, TranslateMessage, CS_HREDRAW, CS_VREDRAW, HTTRANSPARENT,
    HWND_TOPMOST, IDC_ARROW, MSG, SWP_NOACTIVATE, SWP_SHOWWINDOW, SW_SHOWNOACTIVATE, WM_DPICHANGED,
    WM_ERASEBKGND, WM_NCHITTEST, WM_PAINT, WNDCLASSEXW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_POPUP,
};

use crate::settings::{self, OverlayPosition};

// ── Geometry ──────────────────────────────────────────────────────────────
//
// Base units at 96 DPI; everything on screen is multiplied by the window's
// current DPI scale (see `dpi_scale`), matching how the WebView overlay
// sized itself in logical pixels.
//
// Sized to hug the content: 12px padding + 8px dot + 10px gap + widest label
// ("Transcribing\u{2026}" at 14pt Segoe UI \u2248 95px) + 12px padding \u2248 137px.
// 144px leaves a small breathing margin and fits every state.
const CARD_W: f32 = 144.0;
const CARD_H: f32 = 34.0;
const PADDING: f32 = 12.0;
const DOT_DIAMETER: f32 = 8.0;
const FONT_H: f32 = 14.0;
// Offsets from the monitor edge, in 96-DPI units; mirror the Windows values
// of OVERLAY_TOP_OFFSET / OVERLAY_BOTTOM_OFFSET in overlay.rs.
const TOP_OFFSET: f32 = 4.0;
const BOTTOM_OFFSET: f32 = 40.0;

const CLASS_NAME: PCWSTR = w!("HandyNativeOverlay");
const TIMER_ID: usize = 1;
// The message pump blocks in GetMessageW; the timer wakes it at this cadence
// so queued commands (state changes, hide) are drained promptly.
const TIMER_INTERVAL_MS: u32 = 80;

// ── Colour helpers ────────────────────────────────────────────────────────

/// Build a COLORREF the way the RGB macro does (0x00BBGGRR).
const fn colorref(r: u8, g: u8, b: u8) -> COLORREF {
    COLORREF((r as u32) | ((g as u32) << 8) | ((b as u32) << 16))
}

const BG_COLOR: COLORREF = colorref(24, 24, 27);
const BORDER_COLOR: COLORREF = colorref(60, 60, 66);
// Status dot: red while the mic is capturing audio, orange once the pill
// switches to "Transcribing…"/"Processing…" (audio is being worked on).
const DOT_COLOR_RECORDING: COLORREF = colorref(239, 68, 68);
const DOT_COLOR_BUSY: COLORREF = colorref(249, 115, 22);
const TEXT_COLOR: COLORREF = colorref(228, 228, 231);

// ── Commands sent to the overlay thread ───────────────────────────────────

enum OverlayCmd {
    SetState(String),
    Reposition,
    Hide,
}

/// Sender into the single live overlay thread, if any, paired with the
/// generation of the `show()` that created it. The generation lets teardown
/// tell whether the slot still belongs to it after a faster new `show()`
/// replaced the sender.
static OVERLAY_TX: Mutex<Option<(u64, mpsc::Sender<OverlayCmd>)>> = Mutex::new(None);
static OVERLAY_GEN: AtomicU64 = AtomicU64::new(0);
static CLASS_REGISTERED: Once = Once::new();

// ── Public API ────────────────────────────────────────────────────────────

fn label_for(state: &str) -> &'static str {
    match state {
        "streaming" | "recording" => "Recording\u{2026}",
        "transcribing" => "Transcribing\u{2026}",
        "processing" => "Processing\u{2026}",
        _ => "Recording\u{2026}",
    }
}

/// Whether the pill currently reads "Recording…" — i.e. audio is still being
/// captured. The status dot is drawn red in this state and orange once the
/// label switches to transcribing/processing.
fn is_recording_label(label: &[u16]) -> bool {
    const RECORDING: &[u16] = &[
        'R' as u16, 'e' as u16, 'c' as u16, 'o' as u16, 'r' as u16, 'd' as u16, 'i' as u16,
        'n' as u16, 'g' as u16,
    ];
    label.starts_with(RECORDING)
}

/// Show (or update) the native overlay. Called from the recording start path
/// in place of the WebView `show_*_overlay` helpers on Windows.
pub fn show(app: &AppHandle, state: &str) {
    if settings::get_settings(app).overlay_style == settings::OverlayStyle::None {
        return;
    }

    let (tx, rx) = mpsc::channel::<OverlayCmd>();
    let gen = {
        let Ok(mut guard) = OVERLAY_TX.lock() else {
            return;
        };
        // An overlay is already up: update it in place instead of stacking a
        // second pill. (A sender still parked here while its thread winds
        // down also takes this branch — the SetState is simply dropped when
        // the channel's receiver is gone, and the next show() re-creates.)
        if let Some((_, existing)) = guard.as_ref() {
            let _ = existing.send(OverlayCmd::SetState(state.to_string()));
            return;
        }
        let gen = OVERLAY_GEN.fetch_add(1, Ordering::Relaxed) + 1;
        *guard = Some((gen, tx.clone()));
        gen
    };

    let app = app.clone();
    let initial_state = state.to_string();
    thread::spawn(move || {
        run_overlay_thread(&app, &rx, &initial_state, gen);
    });
}

/// Hide and tear down the native overlay. Releases the overlay window/thread
/// so memory drops to zero between recordings.
pub fn hide() {
    if let Some((_, tx)) = OVERLAY_TX.lock().ok().and_then(|mut g| g.take()) {
        let _ = tx.send(OverlayCmd::Hide);
    }
}

/// Recompute overlay position after settings change.
pub fn reposition(_app: &AppHandle) {
    if let Some((_, tx)) = OVERLAY_TX.lock().ok().and_then(|g| g.clone()) {
        let _ = tx.send(OverlayCmd::Reposition);
    }
}

/// Clear the global sender slot only if it still holds the channel created
/// by this thread's `show()` generation.
///
/// A new `show()` may have won the lock after our `Hide` was queued but
/// before we finished tearing down; blindly nulling the slot then would
/// orphan the new thread (its window would never receive updates or hide).
fn clear_tx_if_current(gen: u64) {
    if let Ok(mut guard) = OVERLAY_TX.lock() {
        if guard.as_ref().is_some_and(|(cur, _)| *cur == gen) {
            *guard = None;
        }
    }
}

// ── Overlay thread ────────────────────────────────────────────────────────

fn run_overlay_thread(
    app: &AppHandle,
    rx: &mpsc::Receiver<OverlayCmd>,
    initial_state: &str,
    gen: u64,
) {
    // GetModuleHandleW returns HMODULE, which is ABI-compatible with
    // HINSTANCE (both are base-address handles in the SDK's model), so the
    // newtype payload is carried over directly.
    let hmodule = unsafe { windows::Win32::System::LibraryLoader::GetModuleHandleW(None) }
        .unwrap_or_default();
    let hinstance: HINSTANCE = HINSTANCE(hmodule.0);

    CLASS_REGISTERED.call_once(|| {
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(overlay_wnd_proc),
            hInstance: hinstance,
            lpszClassName: CLASS_NAME,
            hCursor: unsafe { LoadCursorW(None, IDC_ARROW).unwrap_or_default() },
            ..Default::default()
        };
        unsafe {
            let _ = RegisterClassExW(&wc);
        }
    });
    // Intentionally no UnregisterClassW on teardown — see the module docs.

    // WS_EX_TOOLWINDOW (no taskbar entry) | WS_EX_TOPMOST | WS_EX_NOACTIVATE
    // (never takes focus, mirroring the old webview overlay's focusable(false)).
    let ex_style = WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE;

    let hwnd = unsafe {
        CreateWindowExW(
            ex_style,
            CLASS_NAME,
            w!("Handy"),
            WS_POPUP,
            0,
            0,
            CARD_W as i32,
            CARD_H as i32,
            None,
            None,
            Some(hinstance),
            None,
        )
    };

    let Ok(hwnd) = hwnd else {
        error!("native overlay: CreateWindowExW failed");
        clear_tx_if_current(gen);
        return;
    };

    let mut current_label = label_for(initial_state).to_string();
    unsafe { set_window_text(hwnd, &current_label) };
    place_and_show(app, hwnd);

    unsafe {
        let _ = SetTimer(Some(hwnd), TIMER_ID, TIMER_INTERVAL_MS, None);
    }

    loop {
        // Drain commands (non-blocking); the timer keeps this loop waking up.
        let mut quit = false;
        while let Ok(cmd) = rx.try_recv() {
            match cmd {
                OverlayCmd::Hide => {
                    quit = true;
                    break;
                }
                OverlayCmd::SetState(state) => {
                    let new_label = label_for(&state).to_string();
                    if new_label != current_label {
                        current_label = new_label.clone();
                        unsafe {
                            set_window_text(hwnd, &new_label);
                            // Redraw immediately: the pill is drawn in WM_PAINT
                            // from the window text, so without invalidating the
                            // client area it would keep showing the previous
                            // label (e.g. "Recording…" after stop) until some
                            // unrelated repaint happened.
                            let _ = InvalidateRect(Some(hwnd), None, true);
                        };
                        place_and_show(app, hwnd);
                    }
                }
                OverlayCmd::Reposition => place_and_show(app, hwnd),
            }
        }
        if quit {
            break;
        }

        // Blocking message pump — sleeps when idle. GetMessageW returns BOOL:
        //   0  = WM_QUIT (break)
        //  -1  = error  (break)
        // >0  = message available
        let mut msg = MSG::default();
        let ret = unsafe { GetMessageW(&mut msg, Some(hwnd), 0, 0) };
        if ret.0 <= 0 {
            if ret.0 == -1 {
                error!("native overlay: GetMessageW error");
            }
            break;
        }
        unsafe {
            let _ = TranslateMessage(&msg as *const MSG);
            let _ = DispatchMessageW(&msg as *const MSG);
        }
    }

    unsafe {
        let _ = KillTimer(Some(hwnd), TIMER_ID);
        let _ = DestroyWindow(hwnd);
    }
    clear_tx_if_current(gen);
}

// ── Positioning ──────────────────────────────────────────────────────────

/// Per-monitor DPI scale of the window (1.0 at 96 DPI).
fn dpi_scale(hwnd: HWND) -> f32 {
    let dpi = unsafe { GetDpiForWindow(hwnd) };
    if dpi == 0 {
        1.0
    } else {
        dpi as f32 / 96.0
    }
}

fn place_and_show(app: &AppHandle, hwnd: HWND) {
    let scale = dpi_scale(hwnd);
    let w = (CARD_W * scale).round() as i32;
    let h = (CARD_H * scale).round() as i32;
    let Some((x, y)) = compute_position(app, hwnd, w, h, scale) else {
        return;
    };
    unsafe {
        let _ = SetWindowPos(
            hwnd,
            Some(HWND_TOPMOST),
            x,
            y,
            w,
            h,
            SWP_NOACTIVATE | SWP_SHOWWINDOW,
        );
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
    }
}

fn compute_position(
    app: &AppHandle,
    hwnd: HWND,
    card_w: i32,
    card_h: i32,
    scale: f32,
) -> Option<(i32, i32)> {
    // Try the cursor's monitor via the Tauri monitor list; fall back to the
    // monitor the window currently lives on.
    let (rc_left, rc_top, rc_right, rc_bottom) = match cursor_monitor_rect(app) {
        Some(r) => r,
        None => monitor_rect_from_hwnd(hwnd)?,
    };

    let x = rc_left + ((rc_right - rc_left) - card_w) / 2;
    let settings = settings::get_settings(app);
    let y = match settings.overlay_position {
        OverlayPosition::Top => rc_top + (TOP_OFFSET * scale).round() as i32,
        OverlayPosition::Bottom => rc_bottom - card_h - (BOTTOM_OFFSET * scale).round() as i32,
    };
    Some((x, y))
}

/// Get the physical-pixel rect of the monitor containing the cursor.
fn cursor_monitor_rect(app: &AppHandle) -> Option<(i32, i32, i32, i32)> {
    let cursor = crate::input::get_cursor_position(app)?;
    let monitors = app.available_monitors().ok()?;
    for m in &monitors {
        let pos: PhysicalPosition<i32> = *m.position();
        let size: PhysicalSize<u32> = *m.size();
        let w = size.width as i32;
        let h = size.height as i32;
        if cursor.0 >= pos.x && cursor.0 < pos.x + w && cursor.1 >= pos.y && cursor.1 < pos.y + h {
            return Some((pos.x, pos.y, pos.x + w, pos.y + h));
        }
    }
    None
}

/// Fall back: query the monitor the window is currently on.
fn monitor_rect_from_hwnd(hwnd: HWND) -> Option<(i32, i32, i32, i32)> {
    let hmon = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    let mut mi = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    if !unsafe { GetMonitorInfoW(hmon, &mut mi) }.as_bool() {
        return None;
    }
    let rc = mi.rcMonitor;
    Some((rc.left, rc.top, rc.right, rc.bottom))
}

// ── Window text ───────────────────────────────────────────────────────────

unsafe fn set_window_text(hwnd: HWND, label: &str) {
    let wide: Vec<u16> = label.encode_utf16().chain(std::iter::once(0)).collect();
    let _ = SetWindowTextW(hwnd, PCWSTR(wide.as_ptr()));
}

// ── Window procedure + GDI painting ───────────────────────────────────────

unsafe extern "system" fn overlay_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps as *mut PAINTSTRUCT);
            paint(hdc, hwnd);
            let _ = EndPaint(hwnd, &ps as *const PAINTSTRUCT);
            LRESULT(0)
        }
        // Clicks pass through to whatever is underneath — the pill is a
        // passive indicator.
        WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),
        // We fill the whole client area in WM_PAINT; skipping the stock
        // background erase avoids a flash of the wrong colour.
        WM_ERASEBKGND => LRESULT(1),
        // Crossed into a monitor with different DPI: repaint at the new
        // scale (the next place_and_show also re-derives the geometry).
        WM_DPICHANGED => {
            let _ = InvalidateRect(Some(hwnd), None, true);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

unsafe fn paint(hdc: HDC, hwnd: HWND) {
    let scale = dpi_scale(hwnd);
    let padding = (PADDING * scale).round() as i32;
    let dot = (DOT_DIAMETER * scale).round() as i32;

    let mut rc = RECT::default();
    let _ = GetClientRect(hwnd, &mut rc as *mut RECT);
    let card_h = rc.bottom - rc.top;

    // The pill's current state lives in the window title ("Recording…",
    // "Transcribing…", "Processing…"); derive the status-dot colour from it.
    let mut label_buf = [0u16; 64];
    let len = GetWindowTextW(hwnd, &mut label_buf);
    let mut wide: Vec<u16> = label_buf[..len.max(0) as usize].to_vec();

    let bg_brush = CreateSolidBrush(BG_COLOR);
    let border_pen = CreatePen(PS_SOLID, 1, BORDER_COLOR);
    let dot_brush = if is_recording_label(&wide) {
        CreateSolidBrush(DOT_COLOR_RECORDING)
    } else {
        CreateSolidBrush(DOT_COLOR_BUSY)
    };

    // Select objects into the DC (SelectObject returns the previous HGDIOBJ).
    let old_brush: HGDIOBJ = SelectObject(hdc, bg_brush.into());
    let old_pen: HGDIOBJ = SelectObject(hdc, border_pen.into());
    let _ = SetBkMode(hdc, TRANSPARENT);
    let old_txt = SetTextColor(hdc, TEXT_COLOR);

    // Card background.
    let _ = FillRect(hdc, &rc as *const RECT, bg_brush);

    // Status dot — NULL_PEN so Ellipse draws a filled circle.
    let null_pen: HGDIOBJ = GetStockObject(NULL_PEN);
    let old_dot_pen = SelectObject(hdc, null_pen);
    let old_dot_brush = SelectObject(hdc, dot_brush.into());
    let dot_top = (card_h - dot) / 2;
    let _ = Ellipse(hdc, padding, dot_top, padding + dot, dot_top + dot);
    let _ = SelectObject(hdc, old_dot_pen);
    let _ = SelectObject(hdc, old_dot_brush);

    // Label text, in the UI face the rest of the pill implies.
    let font = CreateFontW(
        -(FONT_H * scale).round() as i32,
        0,
        0,
        0,
        FW_NORMAL.0 as i32,
        0,
        0,
        0,
        DEFAULT_CHARSET,
        OUT_DEFAULT_PRECIS,
        CLIP_DEFAULT_PRECIS,
        CLEARTYPE_QUALITY,
        (DEFAULT_PITCH.0 | FF_DONTCARE.0) as u32,
        w!("Segoe UI"),
    );
    let old_font: HGDIOBJ = SelectObject(hdc, font.into());

    let text_x = padding + dot + (10.0 * scale).round() as i32;
    let text_w = (rc.right - rc.left) - text_x - padding;
    let mut text_rc = RECT {
        left: text_x,
        top: 0,
        right: text_x + text_w,
        bottom: card_h,
    };
    let flags: DRAW_TEXT_FORMAT = DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX;
    let _ = DrawTextW(hdc, &mut wide, &mut text_rc as *mut RECT, flags);

    // Restore DC state and free GDI objects.
    let _ = SelectObject(hdc, old_font);
    let _ = SelectObject(hdc, old_brush);
    let _ = SelectObject(hdc, old_pen);
    let _ = SetTextColor(hdc, old_txt);
    let _ = DeleteObject(font.into());
    let _ = DeleteObject(bg_brush.into());
    let _ = DeleteObject(border_pen.into());
    let _ = DeleteObject(dot_brush.into());
}
