//! The tray icon, as a StatusNotifierItem. Tauri's tray goes through libappindicator, which
//! on Linux reports no clicks and shows no tooltip; talking SNI directly gives both.

use ksni::menu::StandardItem;
use ksni::{Handle, MenuItem, ToolTip, TrayMethods};
use std::sync::OnceLock;
use tauri::{AppHandle, Manager};

use crate::AppState;

static HANDLE: OnceLock<Handle<Tray>> = OnceLock::new();

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
        "K-Star".into()
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        self.icon.clone()
    }

    fn tool_tip(&self) -> ToolTip {
        ToolTip {
            title: "K-Star".into(),
            description: self.status.clone(),
            ..Default::default()
        }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        crate::show_main(&self.app);
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let engine = |app: &AppHandle, f: fn(&kstar_core::Engine) -> kstar_core::Result<()>| {
            if let Some(state) = app.try_state::<AppState>()
                && let Err(e) = f(&state.engine)
            {
                tracing::warn!("tray action failed: {e}");
            }
        };
        vec![
            item("Show K-Star", crate::show_main),
            MenuItem::Separator,
            item("Pause all", move |app| engine(app, |e| e.pause_all())),
            item("Resume all", move |app| engine(app, |e| e.resume_all())),
            item("Start queue", move |app| engine(app, |e| e.start_queue())),
            MenuItem::Separator,
            item("Quit", crate::request_quit),
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
        status: "No active downloads".into(),
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
