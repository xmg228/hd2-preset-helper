use std::sync::mpsc::{Receiver, Sender, SyncSender, channel, sync_channel};
use std::thread::JoinHandle;

use anyhow::{Context, Result, anyhow, ensure};
use tray_icon::{
    Icon, TrayIconBuilder,
    menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu},
};
use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetMessageW, KillTimer, MSG, PostThreadMessageW, SetTimer, TranslateMessage,
    WM_APP, WM_QUIT, WM_TIMER,
};

use super::TrayEvent;
use crate::config::AUTO_MONITOR;
use crate::platform::monitors;
use crate::preset_action::PresetActionOptions;

const UPDATE_TRAY: u32 = WM_APP + 1;
const MONITOR_POLL_MS: u32 = 1000;
const MONITOR_ITEM_PREFIX: &str = "overlay_monitor:";

enum TrayUpdate {
    Settings(PresetActionOptions),
    Monitor(String),
}

struct MonitorMenu {
    menu: Submenu,
    selected: String,
    monitors: Vec<monitors::Monitor>,
}

impl MonitorMenu {
    fn rebuild(&self) -> Result<()> {
        while self.menu.remove_at(0).is_some() {}
        self.menu.append(&CheckMenuItem::with_id(
            format!("{MONITOR_ITEM_PREFIX}{AUTO_MONITOR}"),
            "Follow game",
            true,
            self.selected == AUTO_MONITOR,
            None,
        ))?;
        self.menu.append(&PredefinedMenuItem::separator())?;
        let mut found = self.selected == AUTO_MONITOR;
        for monitor in &self.monitors {
            let checked = monitor.id.eq_ignore_ascii_case(&self.selected);
            found |= checked;
            self.menu.append(&CheckMenuItem::with_id(
                format!("{MONITOR_ITEM_PREFIX}{}", monitor.id),
                monitor.label.replace('&', "&&"),
                true,
                checked,
                None,
            ))?;
        }
        if !found {
            self.menu.append(&CheckMenuItem::new(
                "Saved display (Unavailable)",
                false,
                true,
                None,
            ))?;
        }
        Ok(())
    }
}

pub(super) struct WindowsTray {
    updates: Sender<TrayUpdate>,
    thread_id: u32,
    thread: Option<JoinHandle<()>>,
}

impl Drop for WindowsTray {
    fn drop(&mut self) {
        let _ = unsafe { PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0)) };
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl WindowsTray {
    pub(super) fn update_settings(&self, settings: PresetActionOptions) {
        self.update(TrayUpdate::Settings(settings));
    }

    pub(super) fn update_monitor(&self, monitor: String) {
        self.update(TrayUpdate::Monitor(monitor));
    }

    fn update(&self, update: TrayUpdate) {
        let _ = self.updates.send(update);
        let _ = unsafe { PostThreadMessageW(self.thread_id, UPDATE_TRAY, WPARAM(0), LPARAM(0)) };
    }

    pub(super) fn spawn(
        settings: PresetActionOptions,
        monitor: String,
        event_tx: Sender<TrayEvent>,
    ) -> Result<Self> {
        let (ready_tx, ready_rx) = sync_channel(1);
        let (updates_tx, updates_rx) = channel();
        let thread = std::thread::Builder::new()
            .name("hd2-preset-helper-tray".to_string())
            .spawn(move || {
                let tray_thread_id = unsafe { GetCurrentThreadId() };
                let result = run_tray(
                    tray_thread_id,
                    &ready_tx,
                    settings,
                    monitor,
                    event_tx,
                    updates_rx,
                );
                if let Err(error) = result {
                    let _ = ready_tx.send(Err(format!("{error:#}")));
                }
            })
            .context("failed to start tray thread")?;
        let thread_id = ready_rx
            .recv()
            .context("tray thread ended before initialization")?
            .map_err(|error| anyhow!(error))?;

        Ok(Self {
            updates: updates_tx,
            thread_id,
            thread: Some(thread),
        })
    }
}

fn run_tray(
    tray_thread_id: u32,
    ready_tx: &SyncSender<std::result::Result<u32, String>>,
    settings: PresetActionOptions,
    monitor: String,
    event_tx: Sender<TrayEvent>,
    updates_rx: Receiver<TrayUpdate>,
) -> Result<()> {
    let apply_order_item = CheckMenuItem::with_id(
        "apply_in_saved_order",
        "Apply in saved order",
        true,
        settings.apply_in_saved_order,
        None,
    );
    let apply_order_id = apply_order_item.id().clone();
    let auto_ready_item = CheckMenuItem::with_id(
        "auto_ready_up",
        "Auto ready up",
        true,
        settings.auto_ready_up,
        None,
    );
    let auto_ready_id = auto_ready_item.id().clone();
    let save_fallback_when_taken_item = CheckMenuItem::with_id(
        "save_fallback_when_taken",
        "Save fallback for taken Booster",
        true,
        settings.save_fallback_when_taken,
        None,
    );
    let save_fallback_when_taken_id = save_fallback_when_taken_item.id().clone();
    let mut monitor_menu = MonitorMenu {
        menu: Submenu::new("Overlay monitor", true),
        selected: monitor,
        monitors: monitors::available().unwrap_or_default(),
    };
    monitor_menu.rebuild()?;
    let separator = PredefinedMenuItem::separator();
    let close_item = MenuItem::with_id("close", "Exit", true, None);
    let close_id = close_item.id().clone();
    let menu = Menu::with_items(&[
        &apply_order_item,
        &auto_ready_item,
        &save_fallback_when_taken_item,
        &monitor_menu.menu,
        &separator,
        &close_item,
    ])
    .context("failed to create tray menu")?;
    let icon = tray_icon()?;
    let _tray = TrayIconBuilder::new()
        .with_tooltip("HD2 Preset Helper")
        .with_icon(icon)
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
        .build()
        .context("failed to create tray icon")?;

    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        let tray_event = if let Some(monitor) = event.id.as_ref().strip_prefix(MONITOR_ITEM_PREFIX)
        {
            TrayEvent::SetOverlayMonitor(monitor.to_string())
        } else if event.id == apply_order_id {
            TrayEvent::ToggleApplyInSavedOrder
        } else if event.id == auto_ready_id {
            TrayEvent::ToggleAutoReadyUp
        } else if event.id == save_fallback_when_taken_id {
            TrayEvent::ToggleSaveFallbackWhenTaken
        } else if event.id == close_id {
            TrayEvent::ExitRequested
        } else {
            return;
        };
        let _ = event_tx.send(tray_event);
    }));

    let monitor_timer = unsafe { SetTimer(None, 0, MONITOR_POLL_MS, None) };
    ensure!(monitor_timer != 0, "failed to start tray monitor timer");

    ready_tx
        .send(Ok(tray_thread_id))
        .context("failed to signal tray readiness")?;

    let mut message = MSG::default();
    while unsafe { GetMessageW(&mut message, None, 0, 0) }.0 > 0 {
        let poll_monitors = message.message == WM_TIMER
            && message.hwnd.0.is_null()
            && message.wParam.0 == monitor_timer;
        let mut menu_changed = false;
        for update in updates_rx.try_iter() {
            match update {
                TrayUpdate::Settings(settings) => {
                    apply_order_item.set_checked(settings.apply_in_saved_order);
                    auto_ready_item.set_checked(settings.auto_ready_up);
                    save_fallback_when_taken_item.set_checked(settings.save_fallback_when_taken);
                }
                TrayUpdate::Monitor(monitor) => {
                    monitor_menu.selected = monitor;
                    menu_changed = true;
                }
            }
        }
        if poll_monitors {
            match monitors::available() {
                Ok(monitors) if monitors != monitor_menu.monitors => {
                    monitor_menu.monitors = monitors;
                    menu_changed = true;
                }
                Err(error) => tracing::debug!(%error, "monitor list unavailable; keeping menu"),
                _ => {}
            }
        }
        if menu_changed && let Err(error) = monitor_menu.rebuild() {
            tracing::warn!(%error, "failed to refresh overlay monitor menu");
        }
        if message.message == UPDATE_TRAY || poll_monitors {
            continue;
        }
        unsafe {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    let _ = unsafe { KillTimer(None, monitor_timer) };
    MenuEvent::set_event_handler(None::<fn(MenuEvent)>);
    Ok(())
}

fn tray_icon() -> Result<Icon> {
    Icon::from_resource(1, Some((32, 32))).context("failed to load embedded tray icon")
}
