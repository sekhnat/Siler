//! The tray icon, as a StatusNotifierItem. Tauri's tray goes through libappindicator, which
//! on Linux reports no clicks and shows no tooltip; talking SNI directly gives both.

use ksni::menu::{CheckmarkItem, RadioGroup, RadioItem, StandardItem, SubMenu};
use ksni::{Handle, MenuItem, ToolTip, TrayMethods};
use siler_core::{AfterDone, MAIN_QUEUE};
use std::sync::OnceLock;
use tauri::{AppHandle, Manager};

use crate::AppState;
use crate::i18n::tr;

static HANDLE: OnceLock<Handle<Tray>> = OnceLock::new();

const WHEN_DONE: [(AfterDone, &str); 5] = [
    (AfterDone::Nothing, "tray-do-nothing"),
    (AfterDone::Quit, "tray-quit-siler"),
    (AfterDone::Sleep, "tray-sleep"),
    (AfterDone::Hibernate, "tray-hibernate"),
    (AfterDone::PowerOff, "tray-shut-down"),
];

pub struct Tray {
    app: AppHandle,
    icon: Vec<ksni::Icon>,
    status: String,
}

fn icon(png: &[u8]) -> Option<ksni::Icon> {
    let img = tauri::image::Image::from_bytes(png).ok()?;
    // RGBA → ARGB32 in network byte order.
    let data = img
        .rgba()
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|&[r, g, b, a]| [a, r, g, b])
        .collect();
    Some(ksni::Icon {
        width: img.width() as i32,
        height: img.height() as i32,
        data,
    })
}

fn item(label: &str, activate: impl Fn(&AppHandle) + Send + 'static) -> MenuItem<Tray> {
    StandardItem {
        label: label.into(),
        activate: Box::new(move |t: &mut Tray| activate(&t.app)),
        ..Default::default()
    }
    .into()
}

impl ksni::Tray for Tray {
    fn id(&self) -> String {
        crate::desktop::APP_ID.into()
    }

    fn title(&self) -> String {
        "Siler".into()
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        self.icon.clone()
    }

    fn tool_tip(&self) -> ToolTip {
        ToolTip {
            title: "Siler".into(),
            description: self.status.clone(),
            ..Default::default()
        }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        crate::show_main(&self.app);
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let engine = |app: &AppHandle, f: fn(&siler_core::Engine) -> siler_core::Result<()>| {
            if let Some(state) = app.try_state::<AppState>()
                && let Err(e) = f(&state.engine)
            {
                tracing::warn!("tray action failed: {e}");
            }
        };
        let state = self.app.try_state::<AppState>();
        let when_done = state
            .as_ref()
            .map(|s| *s.power.when_done.lock().unwrap())
            .unwrap_or_default();
        let clipboard = state
            .as_ref()
            .is_some_and(|s| s.engine.settings().clipboard_monitor);
        vec![
            item(&tr("tray-show-siler"), crate::show_main),
            MenuItem::Separator,
            item(&tr("tray-pause-all"), move |app| {
                engine(app, |e| e.pause_all())
            }),
            item(&tr("tray-resume-all"), move |app| {
                engine(app, |e| e.resume_all())
            }),
            item(&tr("tray-start-queue"), move |app| {
                engine(app, |e| e.start_queue(MAIN_QUEUE))
            }),
            SubMenu {
                label: tr("tray-when-downloads-finish"),
                submenu: vec![
                    RadioGroup {
                        selected: WHEN_DONE
                            .iter()
                            .position(|(a, _)| *a == when_done)
                            .unwrap_or(0),
                        select: Box::new(|t: &mut Tray, i| {
                            crate::commands::set_when_done_action(&t.app, WHEN_DONE[i].0)
                        }),
                        options: WHEN_DONE
                            .iter()
                            .map(|(_, label)| RadioItem {
                                label: tr(label),
                                ..Default::default()
                            })
                            .collect(),
                    }
                    .into(),
                ],
                ..Default::default()
            }
            .into(),
            CheckmarkItem {
                label: tr("tray-watch-the-clipboard"),
                checked: clipboard,
                activate: Box::new(|t: &mut Tray| {
                    if let Some(state) = t.app.try_state::<AppState>() {
                        let mut s = state.engine.settings();
                        s.clipboard_monitor = !s.clipboard_monitor;
                        match state.engine.set_settings(s) {
                            Ok(s) => {
                                use tauri::Emitter;
                                let _ = t.app.emit("settings-changed", s);
                            }
                            Err(e) => tracing::warn!("couldn't save the setting: {e}"),
                        }
                    }
                }),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            item(&tr("tray-quit"), crate::request_quit),
        ]
    }
}

/// Starts the tray. Returns false when the desktop has no StatusNotifier host (e.g. GNOME
/// without the AppIndicator extension).
pub async fn spawn(app: AppHandle) -> bool {
    let icon = [
        &include_bytes!("../icons/32x32.png")[..],
        &include_bytes!("../icons/64x64.png")[..],
    ]
    .into_iter()
    .filter_map(icon)
    .collect();
    let tray = Tray {
        app,
        icon,
        status: tr("tray-no-active-downloads"),
    };
    match tray.spawn().await {
        Ok(handle) => {
            let _ = HANDLE.set(handle);
            true
        }
        Err(e) => {
            tracing::warn!("no system tray available: {e}");
            false
        }
    }
}

/// Updates the tooltip, e.g. "2 downloading · 5.1 MB/s · 43%".
pub async fn set_status(status: String) {
    if let Some(h) = HANDLE.get() {
        h.update(|t| t.status = status).await;
    }
}

/// Rebuilds the menu after something it shows changed.
pub fn refresh() {
    if let Some(h) = HANDLE.get() {
        let h = h.clone();
        tauri::async_runtime::spawn(async move {
            h.update(|_| {}).await;
        });
    }
}
