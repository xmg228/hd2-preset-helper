use std::cell::RefCell;
use std::collections::HashMap;
use std::mem::{MaybeUninit, size_of};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, ensure};
use tracing::warn;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    ANTIALIASED_QUALITY, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BeginPaint, CLIP_DEFAULT_PRECIS,
    CreateFontW, DC_BRUSH, DEFAULT_CHARSET, DEFAULT_PITCH, DIB_RGB_COLORS, DT_CALCRECT, DT_CENTER,
    DT_END_ELLIPSIS, DT_LEFT, DT_NOPREFIX, DT_SINGLELINE, DT_VCENTER, DT_WORDBREAK, DeleteObject,
    DrawTextW, EndPaint, FW_NORMAL, FW_SEMIBOLD, FillRect, GetDC, GetStockObject, HBRUSH, HDC,
    HFONT, InvalidateRect, OUT_DEFAULT_PRECIS, PAINTSTRUCT, ReleaseDC, SelectObject, SetBkMode,
    SetDCBrushColor, SetDIBitsToDevice, SetTextColor, TRANSPARENT,
};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::{
    GetRawInputData, HRAWINPUT, RAWINPUT, RAWINPUTDEVICE, RAWINPUTHEADER, RID_INPUT,
    RIDEV_INPUTSINK, RIM_TYPEKEYBOARD, RegisterRawInputDevices,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CS_HREDRAW, CS_VREDRAW, CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW,
    GetSystemMetrics, HWND_TOPMOST, KillTimer, LWA_ALPHA, MSG, PostThreadMessageW, RegisterClassW,
    SM_CXSCREEN, SW_HIDE, SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_SHOWWINDOW,
    SetLayeredWindowAttributes, SetTimer, SetWindowPos, ShowWindow, TranslateMessage, WM_APP,
    WM_INPUT, WM_PAINT, WM_QUIT, WM_TIMER, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP, WS_VISIBLE,
};
use windows::core::PCWSTR;

use super::{OverlayEventPolicy, OverlayModel, OverlayTone, compact_error};
use crate::app_events::{AppEvent, AppEventSink, OverlayPreset, OverlayPresetStatus};
use crate::assets::{self, IconCatalog};
use crate::input::HotkeyModifiers;

const OVERLAY_CLASS: &str = "hd2-preset-helper-overlay";
const APP_EVENT_MESSAGE: u32 = WM_APP + 1;
const BASE_SCREEN_MARGIN: i32 = 24;
const BASE_WINDOW_W: i32 = 224;
const BASE_PADDING: i32 = 16;
const BASE_TITLE_Y: i32 = 11;
const BASE_TITLE_H: i32 = 20;
const BASE_TITLE_GAP: i32 = 9;
const BASE_ROW_H: i32 = 28;
const BASE_ROW_GAP: i32 = 5;
const BASE_STATUS_GAP: i32 = 6;
const BASE_STATUS_SEPARATOR_GAP: i32 = 2;
const BASE_STATUS_H: i32 = 24;
const BASE_BOTTOM_PADDING: i32 = 16;
const BASE_ICON_SIZE: i32 = 28;
const BASE_ICON_GAP: i32 = 2;
const BASE_KEY_W: i32 = 32;
const BASE_KEY_H: i32 = 18;
const BASE_KEY_ICON_GAP: i32 = 9;
const BASE_LABEL_ICON_GAP: i32 = 8;
const BASE_ACCENT_W: i32 = 3;
const MAX_PRESET_ICONS: i32 = 5;
const HOLD_KEY_RELEASE_HIDE_DELAY: Duration = Duration::from_millis(120);
const OVERLAY_ALPHA: u8 = 220;
const FADE_DURATION: Duration = Duration::from_millis(180);
const DEFAULT_OVERLAY_ROWS: usize = 4;
const MAX_OVERLAY_ROWS: usize = 12;
const TIMER_HIDE: usize = 1;
const TIMER_FADE: usize = 2;
const FADE_TICK_MS: u32 = 16;

struct OverlayState {
    model: OverlayModel,
    icons: HashMap<String, Option<IconBitmap>>,
    metrics: OverlayMetrics,
}

#[derive(Clone, Copy)]
struct OverlayMetrics {
    scale: f32,
    screen_margin: i32,
    window_w: i32,
    window_h: i32,
    padding: i32,
    row_w: i32,
    row_y: i32,
    row_h: i32,
    row_gap: i32,
    status_y: i32,
    status_h: i32,
    icon_size: u32,
    icon_gap: i32,
    key_x: i32,
    key_w: i32,
    key_h: i32,
    label_x: i32,
    label_w: i32,
    icon_x: i32,
    title_y: i32,
    title_h: i32,
    text_pad_x: i32,
    text_pad_y: i32,
    accent_w: i32,
    title_font: i32,
    body_font: i32,
    small_font: i32,
}

struct IconBitmap {
    size: u32,
    bgra: Vec<u8>,
}

struct OverlayFonts {
    title: HFONT,
    body: HFONT,
    small: HFONT,
}

struct OverlayRenderer {
    title_font: i32,
    body_font: i32,
    small_font: i32,
    fonts: OverlayFonts,
    dc_brush: HBRUSH,
}

struct OverlayContext {
    state: OverlayState,
    renderer: OverlayRenderer,
}

thread_local! {
    static OVERLAY: RefCell<Option<OverlayContext>> = const { RefCell::new(None) };
}

impl OverlayFonts {
    fn create(metrics: OverlayMetrics) -> Self {
        Self {
            title: create_font(metrics.title_font, FW_SEMIBOLD.0 as i32, "Bahnschrift"),
            body: create_font(metrics.body_font, FW_NORMAL.0 as i32, "Segoe UI"),
            small: create_font(metrics.small_font, FW_SEMIBOLD.0 as i32, "Bahnschrift"),
        }
    }
}

impl Drop for OverlayFonts {
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteObject(self.title.into());
            let _ = DeleteObject(self.body.into());
            let _ = DeleteObject(self.small.into());
        }
    }
}

impl OverlayRenderer {
    fn create(metrics: OverlayMetrics) -> Self {
        Self {
            title_font: metrics.title_font,
            body_font: metrics.body_font,
            small_font: metrics.small_font,
            fonts: OverlayFonts::create(metrics),
            // DC_BRUSH is a process-independent stock object. Its color is set per HDC,
            // and it must not be deleted by the application.
            dc_brush: unsafe { HBRUSH(GetStockObject(DC_BRUSH).0) },
        }
    }

    fn sync(&mut self, metrics: OverlayMetrics) {
        if self.title_font != metrics.title_font
            || self.body_font != metrics.body_font
            || self.small_font != metrics.small_font
        {
            self.fonts = OverlayFonts::create(metrics);
            self.title_font = metrics.title_font;
            self.body_font = metrics.body_font;
            self.small_font = metrics.small_font;
        }
    }

    fn fill_rect(&self, hdc: HDC, rect: &RECT, color: COLORREF) {
        unsafe {
            SetDCBrushColor(hdc, color);
            FillRect(hdc, rect, self.dc_brush);
        }
    }
}

fn create_font(height: i32, weight: i32, face: &str) -> HFONT {
    let face = wide_null(face);
    unsafe {
        CreateFontW(
            -height,
            0,
            0,
            0,
            weight,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            ANTIALIASED_QUALITY,
            DEFAULT_PITCH.0 as u32,
            PCWSTR(face.as_ptr()),
        )
    }
}

impl OverlayMetrics {
    fn from_dpi(dpi: u32) -> Self {
        let scale = (dpi as f32 / 96.0).clamp(0.75, 3.0);
        Self::from_scale(scale, DEFAULT_OVERLAY_ROWS)
    }

    fn with_preset_count(self, preset_count: usize) -> Self {
        Self::from_scale_and_label_width(self.scale, overlay_row_count(preset_count), self.label_w)
    }

    fn from_scale(scale: f32, preset_count: usize) -> Self {
        Self::from_scale_and_label_width(scale, preset_count, 0)
    }

    fn with_label_width(self, preset_count: usize, label_w: i32) -> Self {
        Self::from_scale_and_label_width(self.scale, overlay_row_count(preset_count), label_w)
    }

    fn from_scale_and_label_width(scale: f32, preset_count: usize, label_w: i32) -> Self {
        let preset_count = overlay_row_count(preset_count) as i32;
        let padding = scaled_i32(BASE_PADDING, scale);
        let label_w = label_w.max(0);
        let label_icon_gap = if label_w > 0 {
            scaled_i32(BASE_LABEL_ICON_GAP, scale)
        } else {
            0
        };
        let window_w = scaled_i32(BASE_WINDOW_W, scale) + label_w + label_icon_gap;
        let title_y = scaled_i32(BASE_TITLE_Y, scale);
        let title_h = scaled_i32(BASE_TITLE_H, scale);
        let row_y = title_y + title_h + scaled_i32(BASE_TITLE_GAP, scale);
        let row_h = scaled_i32(BASE_ROW_H, scale);
        let row_gap = scaled_i32(BASE_ROW_GAP, scale);
        let status_y = row_y
            + preset_count * row_h
            + (preset_count - 1).max(0) * row_gap
            + scaled_i32(BASE_STATUS_GAP, scale);
        let status_h = scaled_i32(BASE_STATUS_H, scale);
        let window_h = status_y + status_h + scaled_i32(BASE_BOTTOM_PADDING, scale);
        let key_w = scaled_i32(BASE_KEY_W, scale);
        let key_icon_gap = scaled_i32(BASE_KEY_ICON_GAP, scale);
        let icon_size = scaled_i32(BASE_ICON_SIZE, scale);
        let icon_gap = scaled_i32(BASE_ICON_GAP, scale);
        let icons_w = MAX_PRESET_ICONS * icon_size + (MAX_PRESET_ICONS - 1) * icon_gap;
        let content_w = key_w + key_icon_gap + icons_w;
        let content_w = content_w + label_w + label_icon_gap;
        let key_x = (window_w - content_w).max(0) / 2;
        let label_x = key_x + key_w + key_icon_gap;
        let icon_x = label_x + label_w + label_icon_gap;
        Self {
            scale,
            screen_margin: scaled_i32(BASE_SCREEN_MARGIN, scale),
            window_w,
            window_h,
            padding,
            row_w: window_w - padding * 2,
            row_y,
            row_h,
            row_gap,
            status_y,
            status_h,
            icon_size: icon_size as u32,
            icon_gap,
            key_x,
            key_w,
            key_h: scaled_i32(BASE_KEY_H, scale),
            label_x,
            label_w,
            icon_x,
            title_y,
            title_h,
            text_pad_x: scaled_i32(7, scale),
            text_pad_y: scaled_i32(2, scale),
            accent_w: scaled_i32(BASE_ACCENT_W, scale),
            title_font: scaled_i32(14, scale),
            body_font: scaled_i32(13, scale),
            small_font: scaled_i32(12, scale),
        }
    }

    fn with_status_height(mut self, status_h: i32) -> Self {
        self.status_h = status_h.max(scaled_i32(BASE_STATUS_H, self.scale));
        self.window_h = self.status_y + self.status_h + scaled_i32(BASE_BOTTOM_PADDING, self.scale);
        self
    }
}

fn overlay_row_count(preset_count: usize) -> usize {
    preset_count.clamp(1, MAX_OVERLAY_ROWS)
}

impl Default for OverlayMetrics {
    fn default() -> Self {
        Self::from_dpi(96)
    }
}

fn scaled_i32(value: i32, scale: f32) -> i32 {
    ((value as f32 * scale).round() as i32).max(1)
}

fn centered_y(top: i32, outer_h: i32, inner_h: i32) -> i32 {
    top + (outer_h - inner_h).max(0) / 2
}

impl OverlayState {
    fn new(catalog: Arc<IconCatalog>) -> Self {
        Self {
            model: OverlayModel::new(catalog),
            icons: HashMap::new(),
            metrics: OverlayMetrics::default(),
        }
    }

    fn set_metrics(&mut self, metrics: OverlayMetrics) {
        let icon_size_changed = self.metrics.icon_size != metrics.icon_size;
        self.metrics = metrics;
        if icon_size_changed {
            self.icons.clear();
            warm_preset_icons(self);
        }
    }
}

impl OverlayContext {
    fn new(catalog: Arc<IconCatalog>) -> Self {
        let state = OverlayState::new(catalog);
        let renderer = OverlayRenderer::create(state.metrics);
        Self { state, renderer }
    }
}

pub(super) fn start(modifiers: HotkeyModifiers, catalog: Arc<IconCatalog>) -> Result<AppEventSink> {
    let (sender, receiver) = mpsc::channel();
    let wake_thread = Arc::new(AtomicU32::new(0));
    let overlay_wake_thread = Arc::clone(&wake_thread);

    thread::Builder::new()
        .name("hd2-preset-helper-overlay".to_string())
        .spawn(move || {
            if let Err(error) = run_overlay(receiver, modifiers, catalog, &overlay_wake_thread) {
                warn!(error = %format!("{error:#}"), "overlay thread stopped");
            }
            overlay_wake_thread.store(0, Ordering::Release);
        })
        .context("failed to start overlay thread")?;

    Ok(AppEventSink::new(move |event| {
        let _ = sender.send(event);
        let thread_id = wake_thread.load(Ordering::Acquire);
        if thread_id != 0 {
            unsafe {
                let _ = PostThreadMessageW(thread_id, APP_EVENT_MESSAGE, WPARAM(0), LPARAM(0));
            }
        }
    }))
}

fn run_overlay(
    receiver: Receiver<AppEvent>,
    modifiers: HotkeyModifiers,
    catalog: Arc<IconCatalog>,
    wake_thread: &AtomicU32,
) -> Result<()> {
    OVERLAY.with(|overlay| *overlay.borrow_mut() = Some(OverlayContext::new(catalog)));
    let initial_metrics = OverlayMetrics::default();
    let initial_position = overlay_position(initial_metrics);

    let class_name = wide_null(OVERLAY_CLASS);
    let window_title = wide_null("HD2 Preset Helper");

    unsafe {
        let window_class = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(window_proc),
            lpszClassName: PCWSTR(class_name.as_ptr()),
            ..Default::default()
        };

        RegisterClassW(&window_class);

        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_LAYERED | WS_EX_TRANSPARENT,
            PCWSTR(class_name.as_ptr()),
            PCWSTR(window_title.as_ptr()),
            WS_POPUP | WS_VISIBLE,
            initial_position.0,
            initial_position.1,
            initial_metrics.window_w,
            initial_metrics.window_h,
            None,
            None,
            None,
            None,
        )
        .context("failed to create overlay window")?;

        register_raw_keyboard(hwnd)?;
        SetLayeredWindowAttributes(hwnd, COLORREF(0), OVERLAY_ALPHA, LWA_ALPHA)
            .context("failed to configure overlay transparency")?;
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        let metrics = OverlayMetrics::from_dpi(GetDpiForWindow(hwnd));
        let position = overlay_position(metrics);
        OVERLAY.with(|overlay| {
            if let Some(overlay) = overlay.borrow_mut().as_mut() {
                overlay.state.set_metrics(metrics);
            }
        });
        SetWindowPos(
            hwnd,
            Some(HWND_TOPMOST),
            position.0,
            position.1,
            metrics.window_w,
            metrics.window_h,
            SWP_NOACTIVATE | SWP_SHOWWINDOW,
        )
        .context("failed to position overlay window")?;

        wake_thread.store(GetCurrentThreadId(), Ordering::Release);
        run_overlay_loop(hwnd, receiver, modifiers);
        OVERLAY.with(|overlay| overlay.borrow_mut().take());
    }

    Ok(())
}

fn register_raw_keyboard(hwnd: HWND) -> Result<()> {
    let device = RAWINPUTDEVICE {
        usUsagePage: 0x01,
        usUsage: 0x06,
        dwFlags: RIDEV_INPUTSINK,
        hwndTarget: hwnd,
    };
    unsafe {
        RegisterRawInputDevices(&[device], size_of::<RAWINPUTDEVICE>() as u32)
            .context("failed to register raw keyboard input")?;
    }
    Ok(())
}

fn overlay_position(metrics: OverlayMetrics) -> (i32, i32) {
    let screen_w = unsafe { GetSystemMetrics(SM_CXSCREEN) }.max(metrics.window_w);
    (
        screen_w - metrics.window_w - metrics.screen_margin,
        metrics.screen_margin,
    )
}

fn run_overlay_loop(hwnd: HWND, receiver: Receiver<AppEvent>, modifiers: HotkeyModifiers) {
    let mut fade_started = None;
    let mut hold_visible = false;
    let mut hide_deadline: Option<Instant> = None;
    let mut modifier_down = modifiers.is_down();

    if !drain_app_events(
        hwnd,
        &receiver,
        modifier_down,
        &mut hold_visible,
        &mut hide_deadline,
        &mut fade_started,
    ) {
        return;
    }

    loop {
        let mut message = MSG::default();
        let get_result = unsafe { GetMessageW(&mut message, None, 0, 0) };
        if get_result.0 <= 0 || message.message == WM_QUIT {
            return;
        }

        match message.message {
            APP_EVENT_MESSAGE => {
                if !drain_app_events(
                    hwnd,
                    &receiver,
                    modifier_down,
                    &mut hold_visible,
                    &mut hide_deadline,
                    &mut fade_started,
                ) {
                    return;
                }
            }
            WM_INPUT => {
                let previous = modifier_down;
                if read_raw_keyboard(message.lParam) {
                    modifier_down = modifiers.is_down();
                }
                if modifier_down != previous {
                    handle_modifier_change(
                        hwnd,
                        modifier_down,
                        hold_visible,
                        &mut hide_deadline,
                        &mut fade_started,
                    );
                }
                unsafe {
                    let _ = DefWindowProcW(hwnd, message.message, message.wParam, message.lParam);
                }
            }
            WM_TIMER => match message.wParam.0 {
                TIMER_HIDE => {
                    handle_hide_timer(
                        hwnd,
                        modifier_down,
                        hold_visible,
                        &mut hide_deadline,
                        &mut fade_started,
                    );
                }
                TIMER_FADE => {
                    update_fade(hwnd, &mut fade_started);
                }
                _ => unsafe {
                    let _ = TranslateMessage(&message);
                    DispatchMessageW(&message);
                },
            },
            _ => unsafe {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            },
        }
    }
}

fn drain_app_events(
    hwnd: HWND,
    receiver: &Receiver<AppEvent>,
    modifier_down: bool,
    hold_visible: &mut bool,
    hide_deadline: &mut Option<Instant>,
    fade_started: &mut Option<Instant>,
) -> bool {
    let mut policy: Option<OverlayEventPolicy> = None;
    loop {
        match receiver.try_recv() {
            Ok(event) => {
                // Apply every queued state update; the latest event controls visibility.
                if let Some(next) = apply_event(event) {
                    policy = Some(next);
                }
            }
            Err(TryRecvError::Empty) => break,
            Err(TryRecvError::Disconnected) => return false,
        }
    }

    let Some(policy) = policy else {
        return true;
    };

    *hold_visible = matches!(policy, OverlayEventPolicy::Hold);
    cancel_hide_timer(hwnd, hide_deadline);
    cancel_timer(hwnd, TIMER_FADE);
    *fade_started = None;
    resize_overlay(hwnd);
    show_overlay(hwnd);

    if !modifier_down && let OverlayEventPolicy::HideAfter(delay) = policy {
        schedule_hide_timer(hwnd, hide_deadline, delay);
    }

    true
}

fn handle_modifier_change(
    hwnd: HWND,
    modifier_down: bool,
    hold_visible: bool,
    hide_deadline: &mut Option<Instant>,
    fade_started: &mut Option<Instant>,
) {
    if modifier_down {
        cancel_hide_timer(hwnd, hide_deadline);
        cancel_timer(hwnd, TIMER_FADE);
        *fade_started = None;
        resize_overlay(hwnd);
        show_overlay(hwnd);
    } else if !hold_visible {
        schedule_hide_timer(hwnd, hide_deadline, HOLD_KEY_RELEASE_HIDE_DELAY);
    }
}

fn schedule_hide_timer(hwnd: HWND, hide_deadline: &mut Option<Instant>, delay: Duration) {
    *hide_deadline = Some(Instant::now() + delay);
    schedule_timer(hwnd, TIMER_HIDE, delay);
}

fn cancel_hide_timer(hwnd: HWND, hide_deadline: &mut Option<Instant>) {
    *hide_deadline = None;
    cancel_timer(hwnd, TIMER_HIDE);
}

fn handle_hide_timer(
    hwnd: HWND,
    modifier_down: bool,
    hold_visible: bool,
    hide_deadline: &mut Option<Instant>,
    fade_started: &mut Option<Instant>,
) {
    // KillTimer stops future timer generation, but a WM_TIMER already queued before
    // cancellation can still be delivered. Validate the absolute deadline so a stale
    // hide message cannot start fading a newly shown Ready/Done/Failed state.
    cancel_timer(hwnd, TIMER_HIDE);

    let Some(deadline) = *hide_deadline else {
        return;
    };

    let now = Instant::now();
    if now < deadline {
        schedule_timer(hwnd, TIMER_HIDE, deadline.saturating_duration_since(now));
        return;
    }

    *hide_deadline = None;
    if !modifier_down && !hold_visible {
        begin_fade(hwnd, fade_started);
    }
}

fn schedule_timer(hwnd: HWND, timer_id: usize, delay: Duration) {
    cancel_timer(hwnd, timer_id);
    let milliseconds = delay.as_millis().clamp(1, u32::MAX as u128) as u32;
    unsafe {
        let _ = SetTimer(Some(hwnd), timer_id, milliseconds, None);
    }
}

fn cancel_timer(hwnd: HWND, timer_id: usize) {
    unsafe {
        let _ = KillTimer(Some(hwnd), timer_id);
    }
}

fn begin_fade(hwnd: HWND, fade_started: &mut Option<Instant>) {
    *fade_started = Some(Instant::now());
    schedule_timer(hwnd, TIMER_FADE, Duration::from_millis(FADE_TICK_MS as u64));
}

fn update_fade(hwnd: HWND, fade_started: &mut Option<Instant>) {
    let Some(started_at) = *fade_started else {
        cancel_timer(hwnd, TIMER_FADE);
        return;
    };

    let elapsed = started_at.elapsed();
    if elapsed >= FADE_DURATION {
        cancel_timer(hwnd, TIMER_FADE);
        *fade_started = None;
        unsafe {
            let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 0, LWA_ALPHA);
            let _ = ShowWindow(hwnd, SW_HIDE);
        }
        OVERLAY.with(|overlay| {
            if let Some(overlay) = overlay.borrow_mut().as_mut() {
                overlay.state.model.set_ready();
            }
        });
        resize_overlay(hwnd);
        return;
    }

    let progress = (elapsed.as_secs_f32() / FADE_DURATION.as_secs_f32()).clamp(0.0, 1.0);
    let eased = progress * progress * (3.0 - 2.0 * progress);
    let alpha = (OVERLAY_ALPHA as f32 * (1.0 - eased)).round() as u8;
    unsafe {
        let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), alpha, LWA_ALPHA);
    }
}

fn read_raw_keyboard(lparam: LPARAM) -> bool {
    let mut raw = MaybeUninit::<RAWINPUT>::zeroed();
    let mut size = size_of::<RAWINPUT>() as u32;
    let copied = unsafe {
        GetRawInputData(
            HRAWINPUT(lparam.0 as *mut _),
            RID_INPUT,
            Some(raw.as_mut_ptr().cast()),
            &mut size,
            size_of::<RAWINPUTHEADER>() as u32,
        )
    };
    if copied == u32::MAX || copied < size_of::<RAWINPUTHEADER>() as u32 {
        return false;
    }

    let raw = unsafe { raw.assume_init() };
    raw.header.dwType == RIM_TYPEKEYBOARD.0
}

fn show_overlay(hwnd: HWND) {
    unsafe {
        let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), OVERLAY_ALPHA, LWA_ALPHA);
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        let _ = InvalidateRect(Some(hwnd), None, false);
    }
}

fn resize_overlay(hwnd: HWND) {
    let Some(metrics) = refresh_status_metrics(hwnd) else {
        return;
    };
    let position = overlay_position(metrics);

    unsafe {
        let _ = SetWindowPos(
            hwnd,
            Some(HWND_TOPMOST),
            position.0,
            position.1,
            metrics.window_w,
            metrics.window_h,
            SWP_NOACTIVATE | SWP_SHOWWINDOW,
        );
    }
}

fn refresh_status_metrics(hwnd: HWND) -> Option<OverlayMetrics> {
    OVERLAY.with(|overlay| {
        let mut overlay = overlay.borrow_mut();
        let overlay = overlay.as_mut()?;
        overlay.renderer.sync(overlay.state.metrics);

        let metrics = overlay.state.metrics;
        let hdc = unsafe { GetDC(Some(hwnd)) };
        if hdc.is_invalid() {
            return Some(metrics);
        }

        let measured_label_w = overlay
            .state
            .model
            .presets
            .iter()
            .filter_map(|preset| preset.label.as_deref())
            .map(|label| measure_text_line_width(hdc, overlay.renderer.fonts.body, label))
            .max()
            .unwrap_or(0);
        let base_window_w = scaled_i32(BASE_WINDOW_W, metrics.scale);
        let label_icon_gap = scaled_i32(BASE_LABEL_ICON_GAP, metrics.scale);
        let screen_w = unsafe { GetSystemMetrics(SM_CXSCREEN) }.max(base_window_w);
        let max_window_w = (screen_w - metrics.screen_margin * 2).max(base_window_w);
        let max_label_w = (max_window_w - base_window_w - label_icon_gap).max(0);
        let label_w = if measured_label_w > 0 {
            (measured_label_w + scaled_i32(2, metrics.scale)).min(max_label_w)
        } else {
            0
        };
        let metrics = metrics.with_label_width(overlay.state.model.presets.len(), label_w);
        let text_w = (metrics.row_w - metrics.text_pad_x * 2).max(1);
        let text_h = measure_wrapped_text_height(
            hdc,
            overlay.renderer.fonts.body,
            &overlay.state.model.status,
            text_w,
        );
        unsafe {
            let _ = ReleaseDC(Some(hwnd), hdc);
        }

        let status_h = text_h + metrics.text_pad_y * 2;
        let metrics = metrics.with_status_height(status_h);
        overlay.state.metrics = metrics;
        Some(metrics)
    })
}

fn measure_text_line_width(hdc: HDC, font: HFONT, text: &str) -> i32 {
    let mut text = wide_null(text);
    let mut rect = RECT::default();

    unsafe {
        let _ = SelectObject(hdc, font.into());
        let len = text.len().saturating_sub(1);
        DrawTextW(
            hdc,
            &mut text[..len],
            &mut rect,
            DT_LEFT | DT_SINGLELINE | DT_CALCRECT | DT_NOPREFIX,
        );
    }

    (rect.right - rect.left).max(0)
}

fn measure_wrapped_text_height(hdc: HDC, font: HFONT, text: &str, width: i32) -> i32 {
    let mut text = wide_null(text);
    let mut rect = RECT {
        left: 0,
        top: 0,
        right: width.max(1),
        bottom: 0,
    };

    unsafe {
        let _ = SelectObject(hdc, font.into());
        let len = text.len().saturating_sub(1);
        DrawTextW(
            hdc,
            &mut text[..len],
            &mut rect,
            DT_LEFT | DT_WORDBREAK | DT_CALCRECT | DT_NOPREFIX,
        );
    }

    (rect.bottom - rect.top).max(0)
}

fn apply_event(event: AppEvent) -> Option<OverlayEventPolicy> {
    OVERLAY.with(|overlay| {
        let mut overlay = overlay.borrow_mut();
        let state = &mut overlay.as_mut()?.state;

        let update = state.model.apply(event);
        if update.presets_changed {
            state.metrics = state.metrics.with_preset_count(state.model.presets.len());
            warm_preset_icons(state);
        }
        Some(update.policy)
    })
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_PAINT => {
            paint_overlay(hwnd);
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

fn paint_overlay(hwnd: HWND) {
    OVERLAY.with(|overlay| {
        let mut overlay = overlay.borrow_mut();
        let Some(overlay) = overlay.as_mut() else {
            return;
        };
        let OverlayContext { state, renderer } = overlay;
        renderer.sync(state.metrics);

        unsafe {
            let mut paint = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut paint);

            let background_rect = RECT {
                left: 0,
                top: 0,
                right: state.metrics.window_w,
                bottom: state.metrics.window_h,
            };
            let accent_rect = RECT {
                left: 0,
                top: 0,
                right: state.metrics.accent_w,
                bottom: state.metrics.window_h,
            };
            renderer.fill_rect(hdc, &background_rect, rgb(18, 22, 26));
            renderer.fill_rect(hdc, &accent_rect, tone_color(state.model.tone));

            SetBkMode(hdc, TRANSPARENT);
            let _ = SelectObject(hdc, renderer.fonts.title.into());
            SetTextColor(hdc, rgb(236, 242, 248));
            draw_text_line(
                hdc,
                "HD2 Preset Helper",
                state.metrics.padding,
                state.metrics.title_y,
                state.metrics.window_w - state.metrics.padding * 2,
                state.metrics.title_h,
            );
            draw_presets(hdc, state, renderer);
            draw_status(hdc, state, renderer);

            let _ = EndPaint(hwnd, &paint);
        }
    });
}

fn draw_presets(hdc: HDC, state: &OverlayState, renderer: &OverlayRenderer) {
    unsafe {
        let visible_count = state.model.presets.len().min(MAX_OVERLAY_ROWS);
        for (index, preset) in state.model.presets.iter().take(visible_count).enumerate() {
            let metrics = state.metrics;
            let y = metrics.row_y + index as i32 * (metrics.row_h + metrics.row_gap);
            let is_active = state.model.active_preset.as_deref() == Some(preset.name.as_str());

            if is_active {
                let row_rect = RECT {
                    left: metrics.padding,
                    top: y,
                    right: metrics.padding + metrics.row_w,
                    bottom: y + metrics.row_h,
                };
                renderer.fill_rect(hdc, &row_rect, rgb(37, 43, 48));
            }

            draw_key_label(
                hdc,
                metrics,
                renderer,
                preset.key_label,
                metrics.key_x,
                centered_y(y, metrics.row_h, metrics.key_h),
            );

            if let Some(label) = preset.label.as_deref() {
                let label_h = scaled_i32(18, metrics.scale);
                let _ = SelectObject(hdc, renderer.fonts.body.into());
                SetTextColor(hdc, rgb(206, 216, 226));
                draw_text_line(
                    hdc,
                    label,
                    metrics.label_x,
                    centered_y(y, metrics.row_h, label_h),
                    metrics.label_w,
                    label_h,
                );
            }

            match &preset.status {
                OverlayPresetStatus::Ready => {
                    draw_preset_icons(
                        hdc,
                        state,
                        renderer,
                        preset,
                        metrics.icon_x,
                        centered_y(y, metrics.row_h, metrics.icon_size as i32),
                    );
                }
                OverlayPresetStatus::NotSaved => {
                    let _ = SelectObject(hdc, renderer.fonts.body.into());
                    SetTextColor(hdc, rgb(128, 138, 148));
                    draw_text_line(
                        hdc,
                        "not saved",
                        metrics.icon_x,
                        centered_y(y, metrics.row_h, scaled_i32(18, metrics.scale)),
                        scaled_i32(170, metrics.scale),
                        scaled_i32(18, metrics.scale),
                    );
                }
                OverlayPresetStatus::Invalid(error) => {
                    let _ = SelectObject(hdc, renderer.fonts.body.into());
                    SetTextColor(hdc, rgb(255, 150, 150));
                    draw_text_line(
                        hdc,
                        &format!("invalid: {}", compact_error(error)),
                        metrics.icon_x,
                        centered_y(y, metrics.row_h, scaled_i32(18, metrics.scale)),
                        metrics.window_w - metrics.icon_x - scaled_i32(18, metrics.scale),
                        scaled_i32(18, metrics.scale),
                    );
                }
            }

            if index + 1 < visible_count {
                let separator_y = y + metrics.row_h + metrics.row_gap / 2;
                let separator = RECT {
                    left: metrics.padding,
                    top: separator_y,
                    right: metrics.window_w - metrics.padding,
                    bottom: separator_y + 1,
                };
                renderer.fill_rect(hdc, &separator, rgb(48, 55, 62));
            }
        }
    }
}

fn draw_key_label(
    hdc: HDC,
    metrics: OverlayMetrics,
    renderer: &OverlayRenderer,
    text: &str,
    x: i32,
    y: i32,
) {
    let mut text = wide_null(text);
    let mut rect = RECT {
        left: x,
        top: y,
        right: x + metrics.key_w,
        bottom: y + metrics.key_h,
    };

    unsafe {
        let _ = SelectObject(hdc, renderer.fonts.small.into());
        SetTextColor(hdc, rgb(150, 196, 230));
        let len = text.len().saturating_sub(1);
        DrawTextW(
            hdc,
            &mut text[..len],
            &mut rect,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
        );
    }
}

fn draw_preset_icons(
    hdc: HDC,
    state: &OverlayState,
    renderer: &OverlayRenderer,
    preset: &OverlayPreset,
    x: i32,
    y: i32,
) {
    let icon_size = state.metrics.icon_size as i32;

    for (index, item_id) in preset.stratagems.iter().take(4).enumerate() {
        let icon_x = x + index as i32 * (icon_size + state.metrics.icon_gap);
        draw_preset_icon(hdc, state, renderer, item_id, icon_x, y);
    }

    if let Some(item_id) = preset.booster.as_ref() {
        let icon_x = x + 4 * (icon_size + state.metrics.icon_gap);
        draw_preset_icon(hdc, state, renderer, item_id, icon_x, y);
    }
}

fn draw_preset_icon(
    hdc: HDC,
    state: &OverlayState,
    renderer: &OverlayRenderer,
    item_id: &str,
    x: i32,
    y: i32,
) {
    match state.icons.get(item_id).and_then(|icon| icon.as_ref()) {
        Some(icon) => draw_icon_bitmap(hdc, icon, x, y),
        None => draw_icon_placeholder(hdc, renderer, x, y, state.metrics.icon_size),
    }
}

fn draw_icon_bitmap(hdc: HDC, icon: &IconBitmap, x: i32, y: i32) {
    let bitmap_info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: icon.size as i32,
            biHeight: -(icon.size as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };

    unsafe {
        SetDIBitsToDevice(
            hdc,
            x,
            y,
            icon.size,
            icon.size,
            0,
            0,
            0,
            icon.size,
            icon.bgra.as_ptr().cast(),
            &bitmap_info,
            DIB_RGB_COLORS,
        );
    }
}

fn draw_icon_placeholder(hdc: HDC, renderer: &OverlayRenderer, x: i32, y: i32, size: u32) {
    let rect = RECT {
        left: x,
        top: y,
        right: x + size as i32,
        bottom: y + size as i32,
    };
    renderer.fill_rect(hdc, &rect, rgb(52, 58, 64));
}

fn draw_status(hdc: HDC, state: &OverlayState, renderer: &OverlayRenderer) {
    unsafe {
        let metrics = state.metrics;
        let separator_y = metrics.status_y
            - scaled_i32(BASE_STATUS_GAP - BASE_STATUS_SEPARATOR_GAP, metrics.scale);
        let separator = RECT {
            left: metrics.padding,
            top: separator_y,
            right: metrics.window_w - metrics.padding,
            bottom: separator_y + 1,
        };
        renderer.fill_rect(hdc, &separator, rgb(48, 55, 62));

        let _ = SelectObject(hdc, renderer.fonts.body.into());
        SetTextColor(hdc, rgb(206, 216, 226));
        draw_wrapped_text(
            hdc,
            &state.model.status,
            metrics.padding + metrics.text_pad_x,
            metrics.status_y + metrics.text_pad_y,
            metrics.row_w - metrics.text_pad_x * 2,
            metrics.status_h - metrics.text_pad_y * 2,
        );
    }
}

fn draw_wrapped_text(hdc: HDC, text: &str, x: i32, y: i32, w: i32, h: i32) {
    let mut text = wide_null(text);
    let mut rect = RECT {
        left: x,
        top: y,
        right: x + w.max(1),
        bottom: y + h.max(1),
    };

    unsafe {
        let len = text.len().saturating_sub(1);
        DrawTextW(
            hdc,
            &mut text[..len],
            &mut rect,
            DT_LEFT | DT_WORDBREAK | DT_NOPREFIX,
        );
    }
}

fn draw_text_line(hdc: HDC, text: &str, x: i32, y: i32, w: i32, h: i32) {
    let mut text = wide_null(text);
    let mut rect = RECT {
        left: x,
        top: y,
        right: x + w,
        bottom: y + h,
    };

    unsafe {
        let len = text.len().saturating_sub(1);
        DrawTextW(
            hdc,
            &mut text[..len],
            &mut rect,
            DT_LEFT | DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX,
        );
    }
}

fn warm_preset_icons(state: &mut OverlayState) {
    let item_ids = state
        .model
        .presets
        .iter()
        .flat_map(|preset| preset.stratagems.iter().chain(preset.booster.iter()))
        .cloned()
        .collect::<Vec<_>>();

    for item_id in item_ids {
        if state.icons.contains_key(&item_id) {
            continue;
        }

        let icon = match render_item_icon(&state.model.catalog, &item_id, state.metrics.icon_size) {
            Ok(icon) => Some(icon),
            Err(error) => {
                warn!(item_id, error = %format!("{error:#}"), "failed to render overlay icon");
                None
            }
        };
        state.icons.insert(item_id, icon);
    }
}

fn render_item_icon(catalog: &IconCatalog, item_id: &str, icon_size: u32) -> Result<IconBitmap> {
    ensure!(icon_size > 0, "overlay icon size must be non-zero");
    let entry = catalog
        .get(item_id)
        .with_context(|| format!("unknown icon item ID {item_id}"))?;
    let source = assets::icon_image(&entry.path)?;

    let (dst_w, dst_h) = fit_inside(source.width(), source.height(), icon_size);
    let resized = assets::resize_rgba_box(&source, dst_w, dst_h)?;
    let offset_x = (icon_size - dst_w) / 2;
    let offset_y = (icon_size - dst_h) / 2;
    let mut bgra = vec![0u8; (icon_size * icon_size * 4) as usize];

    for y in 0..icon_size {
        for x in 0..icon_size {
            let out_index = ((y * icon_size + x) * 4) as usize;
            let (r, g, b, a) =
                if x >= offset_x && x < offset_x + dst_w && y >= offset_y && y < offset_y + dst_h {
                    let pixel = resized.get_pixel(x - offset_x, y - offset_y).0;
                    (pixel[0], pixel[1], pixel[2], pixel[3])
                } else {
                    (0, 0, 0, 0)
                };

            // The embedded PNG uses straight alpha. Composite after alpha-aware
            // resizing so the dark tile color does not bleed into antialiased edges.
            let alpha = a as u16;
            let inv_alpha = 255 - alpha;
            let out_r = (r as u16 * alpha + 30u16 * inv_alpha + 127) / 255;
            let out_g = (g as u16 * alpha + 36u16 * inv_alpha + 127) / 255;
            let out_b = (b as u16 * alpha + 42u16 * inv_alpha + 127) / 255;
            bgra[out_index..out_index + 4].copy_from_slice(&[
                out_b as u8,
                out_g as u8,
                out_r as u8,
                255,
            ]);
        }
    }

    Ok(IconBitmap {
        size: icon_size,
        bgra,
    })
}

fn fit_inside(source_w: u32, source_h: u32, bounds: u32) -> (u32, u32) {
    if source_w == 0 || source_h == 0 {
        return (bounds, bounds);
    }
    if source_w >= source_h {
        let height =
            ((source_h as u64 * bounds as u64 + source_w as u64 / 2) / source_w as u64) as u32;
        (bounds, height.max(1))
    } else {
        let width =
            ((source_w as u64 * bounds as u64 + source_h as u64 / 2) / source_h as u64) as u32;
        (width.max(1), bounds)
    }
}

const fn tone_color(tone: OverlayTone) -> COLORREF {
    match tone {
        OverlayTone::Info => rgb(118, 210, 255),
        OverlayTone::Working => rgb(255, 220, 96),
        OverlayTone::Success => rgb(120, 230, 150),
        OverlayTone::Warning => rgb(255, 180, 100),
        OverlayTone::Error => rgb(255, 120, 120),
    }
}

const fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    COLORREF(r as u32 | ((g as u32) << 8) | ((b as u32) << 16))
}

fn wide_null(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}
