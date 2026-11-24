//! The tray icon, as a StatusNotifierItem. Tauri's tray goes through libappindicator, which
//! on Linux reports no clicks and shows no tooltip; talking SNI directly gives both.
//!
//! `model` follows the engine; the icon, the tooltip and the menu are made from it, and
//! updated at most once a second. A left click opens the panel (`panel`).

mod icon;
mod layer;
mod menu;
mod model;
pub mod panel;
mod place;
pub mod stack;

use ksni::{Handle, MenuItem, ToolTip, TrayMethods};
use std::sync::OnceLock;
use std::time::Duration;
use tauri::{AppHandle, Manager};
use tokio::sync::broadcast::error::RecvError;

use crate::AppState;
pub use model::markup;

static HANDLE: OnceLock<Handle<Tray>> = OnceLock::new();

const EVERY: Duration = Duration::from_secs(1);

pub struct Tray {
    app: AppHandle,
    model: model::Model,
    /// Goes up whenever other downloads are running.
    generation: u64,
    logo: Option<icon::Logo>,
    look: icon::Look,
    icons: Vec<ksni::Icon>,
}

impl Tray {
    fn new(app: AppHandle, model: model::Model) -> Tray {
        let look = icon::Look::of(&model, crate::skins::accent());
        let logo = icon::Logo::load();
        let icons = logo
            .as_ref()
            .map(|l| icon::render(l, &look))
            .unwrap_or_default();
        Tray {
            app,
            model,
            generation: 0,
            logo,
            look,
            icons,
        }
    }

    fn set(&mut self, model: model::Model) {
        if model.ids() != self.model.ids() {
            self.generation += 1;
        }
        let look = icon::Look::of(&model, crate::skins::accent());
        if look != self.look
            && let Some(logo) = &self.logo
        {
            self.icons = icon::render(logo, &look);
        }
        self.look = look;
        self.model = model;
    }
}

/// Opens the main window with the activation token of the click being handled.
fn open_siler(app: &AppHandle) {
    crate::show_main_with(app, ksni::current_xdg_activation_token());
}

impl ksni::Tray for Tray {
    fn id(&self) -> String {
        crate::desktop::APP_ID.into()
    }

    fn title(&self) -> String {
        "Siler".into()
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        self.icons.clone()
    }

    fn tool_tip(&self) -> ToolTip {
        ToolTip {
            title: if self.model.lines.is_empty() {
                "Siler".into()
            } else {
                format!("Siler · {}", model::rate(self.model.speed))
            },
            description: model::tooltip(&self.model),
            ..Default::default()
        }
    }

    fn activate(&mut self, x: i32, y: i32) {
        let opens_panel = self
            .app
            .try_state::<AppState>()
            .is_some_and(|s| s.engine.settings().tray_opens_panel);
        if opens_panel {
            panel::toggle(
                &self.app,
                x,
                y,
                ksni::current_xdg_activation_token(),
                panel::Mode::Downloads,
            );
        } else {
            open_siler(&self.app);
        }
    }

    fn secondary_activate(&mut self, _x: i32, _y: i32) {
        open_siler(&self.app);
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        menu::build(self)
    }
}

/// Starts the tray. Returns false when the desktop has no StatusNotifier host (e.g. GNOME
/// without the AppIndicator extension).
pub async fn spawn(app: AppHandle, engine: siler_core::Engine) -> bool {
    // Subscribed before the list is read, so nothing falls between.
    let events = engine.subscribe();
    let mut tracker = model::Tracker::default();
    tracker.reset(&engine.list());
    let tray = Tray::new(app, tracker.model());
    match tray.spawn().await {
        Ok(handle) => {
            let _ = HANDLE.set(handle.clone());
            tauri::async_runtime::spawn(follow(engine, events, tracker, handle));
            true
        }
        Err(e) => {
            tracing::warn!("no system tray available: {e}");
            false
        }
    }
}

async fn follow(
    engine: siler_core::Engine,
    mut events: tokio::sync::broadcast::Receiver<siler_core::Event>,
    mut tracker: model::Tracker,
    handle: Handle<Tray>,
) {
    let mut shown: Option<model::Model> = None;
    let mut last = tokio::time::Instant::now();
    let mut changed = true;
    loop {
        tokio::select! {
            event = events.recv() => match event {
                Ok(event) => changed |= tracker.apply(&event),
                Err(RecvError::Lagged(_)) => {
                    tracker.reset(&engine.list());
                    changed = true;
                }
                Err(RecvError::Closed) => break,
            },
            _ = tokio::time::sleep_until(last + EVERY), if changed => {
                changed = false;
                let model = tracker.model();
                if shown.as_ref() != Some(&model) {
                    last = tokio::time::Instant::now();
                    shown = Some(model.clone());
                    handle.update(|t| t.set(model)).await;
                    // The progress windows' icon shows some of the same downloads.
                    stack::sync();
                }
            }
        }
    }
}

/// There is a tray to put things away in.
pub fn available(app: &AppHandle) -> bool {
    app.try_state::<AppState>()
        .is_some_and(|s| s.tray_available.load(std::sync::atomic::Ordering::Relaxed))
}

/// Rebuilds the menu and the icon after something they show changed.
pub fn refresh() {
    if let Some(h) = HANDLE.get() {
        let h = h.clone();
        tauri::async_runtime::spawn(async move {
            h.update(|t| t.set(t.model.clone())).await;
        });
    }
}
