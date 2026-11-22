//! The panel a left click on the tray icon opens, beside the icon.
//!
//! On Wayland it is a layer-shell surface with the keyboard, over transparent "catchers"
//! that cover each screen: a click anywhere else closes it. (Waiting for the focus to
//! leave isn't enough there: a click on the empty desktop doesn't move it.) On X11 it is
//! an undecorated window that grabs the pointer as a menu does, for the same reason, and
//! also closes when it loses the focus.

use gtk::glib;
use gtk::prelude::*;
use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::SeqCst};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder};

use super::layer;
use super::place::{self, Anchor, Rect, Screen};

pub const LABEL: &str = "tray";
const WIDTH: i32 = 380;
const FIRST_HEIGHT: i32 = 240;
const MAX_HEIGHT: i32 = 600;
/// How long a hidden panel is kept before its web view is given back.
const KEEP_HIDDEN: Duration = Duration::from_secs(60);
/// X11: the click on the icon that took the focus away mustn't open it again.
const REOPEN_AFTER: Duration = Duration::from_millis(300);

static OPEN: AtomicBool = AtomicBool::new(false);
/// Counts openings and closings, so an outdated "destroy it" timer does nothing.
static TURN: AtomicU64 = AtomicU64::new(0);

#[derive(Default)]
struct Ui {
    window: Option<gtk::Window>,
    catchers: Vec<gtk::Window>,
    shown: Option<Shown>,
    closed_at: Option<Instant>,
    height: i32,
}

#[derive(Clone)]
struct Shown {
    anchor: Anchor,
    monitor: gtk::gdk::Monitor,
    area: Rect,
    work: Rect,
}

thread_local! {
    // GTK's objects stay on its thread.
    static UI: RefCell<Ui> = RefCell::new(Ui { height: FIRST_HEIGHT, ..Default::default() });
}

/// The panel is on screen, so it needs the progress events.
pub fn is_open() -> bool {
    OPEN.load(SeqCst)
}

/// A left click on the icon, at the coordinates the tray host gave.
pub fn toggle(app: &AppHandle, x: i32, y: i32, token: Option<String>) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let scales = if is_open() {
            Vec::new()
        } else {
            kwin_scales().await
        };
        let on_main = app.clone();
        let _ = app.run_on_main_thread(move || {
            if is_open() {
                close_now(&on_main);
            } else if UI.with_borrow(|ui| ui.closed_at.is_some_and(|t| t.elapsed() < REOPEN_AFTER))
            {
                // That click already closed it.
            } else if let Err(e) = open(&on_main, (x, y), &scales) {
                tracing::info!("no tray panel ({e}), opening Siler instead");
                crate::show_main_with(&on_main, token);
            }
        });
    });
}

/// Closes the panel, from any thread.
pub fn close(app: &AppHandle) {
    if !is_open() {
        return;
    }
    let on_main = app.clone();
    let _ = app.run_on_main_thread(move || close_now(&on_main));
}

/// The page's height changed.
pub fn resize(app: &AppHandle, height: i32) {
    let _ = app.run_on_main_thread(move || {
        let (window, shown, height) = UI.with_borrow_mut(|ui| {
            ui.height = height.clamp(80, MAX_HEIGHT);
            (ui.window.clone(), ui.shown.clone(), ui.height)
        });
        if let Some(w) = window {
            w.set_size_request(WIDTH, height);
            w.resize(WIDTH, height);
            if let Some(shown) = shown {
                put(&w, &shown, height);
            }
        }
    });
}

/// KWin's scale of each screen; empty elsewhere.
async fn kwin_scales() -> Vec<((i32, i32), f64)> {
    let kde = std::env::var("XDG_CURRENT_DESKTOP").is_ok_and(|d| d.contains("KDE"));
    if !kde {
        return Vec::new();
    }
    let ask = async {
        let conn = zbus::Connection::session().await?;
        let reply = conn
            .call_method(
                Some("org.kde.KWin"),
                "/KWin",
                Some("org.kde.KWin"),
                "supportInformation",
                &(),
            )
            .await?;
        reply.body().deserialize::<String>()
    };
    match tokio::time::timeout(Duration::from_millis(300), ask).await {
        Ok(Ok(info)) => place::kwin_scales(&info),
        _ => Vec::new(),
    }
}

fn rect(r: &gtk::gdk::Rectangle) -> Rect {
    Rect {
        x: r.x(),
        y: r.y(),
        w: r.width(),
        h: r.height(),
    }
}

fn open(app: &AppHandle, click: (i32, i32), scales: &[((i32, i32), f64)]) -> Result<(), String> {
    let display = gtk::gdk::Display::default().ok_or("no display")?;
    let wayland = display.backend().is_wayland();
    let layer = if wayland {
        Some(layer::get().ok_or("no layer-shell")?)
    } else {
        None
    };

    let monitors: Vec<gtk::gdk::Monitor> = (0..display.n_monitors())
        .filter_map(|i| display.monitor(i))
        .collect();
    let screens: Vec<Screen> = monitors
        .iter()
        .map(|m| {
            let area = rect(&m.geometry());
            let scale = scales
                .iter()
                .find(|(corner, _)| *corner == (area.x, area.y))
                .map_or(f64::from(m.scale_factor().max(1)), |(_, s)| *s);
            Screen { area, scale }
        })
        .collect();
    let anchor = place::locate(&screens, click).ok_or("no screen")?;
    let monitor = monitors[anchor.screen].clone();
    let shown = Shown {
        area: screens[anchor.screen].area,
        work: rect(&monitor.workarea()),
        monitor,
        anchor,
    };

    let window = match UI.with_borrow(|ui| ui.window.clone()) {
        Some(w) => w,
        None => build(app, layer)?,
    };
    if let Some(layer) = layer {
        // Shown first, so the panel ends up above them.
        let catchers: Vec<gtk::Window> = monitors.iter().map(|m| catcher(app, layer, m)).collect();
        UI.with_borrow_mut(|ui| ui.catchers = catchers);
    }

    let height = UI.with_borrow_mut(|ui| {
        ui.shown = Some(shown.clone());
        ui.height
    });
    tracing::debug!(?click, edge = ?shown.anchor.edge, "tray panel opens");
    OPEN.store(true, SeqCst);
    TURN.fetch_add(1, SeqCst);
    // Progress was skipped while it was closed.
    let _ = app.emit_to(LABEL, "engine-resync", ());
    let _ = app.emit_to(LABEL, "tray-opened", ());
    window.set_size_request(WIDTH, height);
    put(&window, &shown, height);
    window.show_all();
    if layer.is_none() {
        // X11 may ignore a position given before the window is mapped.
        put(&window, &shown, height);
        window.present_with_time(gtk::current_event_time());
    }
    Ok(())
}

fn put(window: &gtk::Window, shown: &Shown, height: i32) {
    let size = (WIDTH, height);
    if window.display().backend().is_wayland() {
        if let Some(layer) = layer::get() {
            let along = place::along(&shown.area, &shown.anchor, size);
            layer.anchor(window, &shown.monitor, shown.anchor.edge, along);
        }
    } else {
        let (x, y) = place::corner(&shown.work, &shown.anchor, size);
        window.move_(x, y);
    }
}

fn build(app: &AppHandle, layer: Option<&layer::Layer>) -> Result<gtk::Window, String> {
    let page = WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App("index.html#tray".into()))
        .title(crate::i18n::tr("window-tray-panel"))
        .inner_size(f64::from(WIDTH), f64::from(FIRST_HEIGHT))
        .resizable(false)
        // Layer-shell draws no decorations; asking for none on Wayland would add GTK's own.
        .decorations(layer.is_some())
        .transparent(true)
        .skip_taskbar(true)
        .always_on_top(true)
        .visible(false)
        .disable_drag_drop_handler()
        .build()
        .map_err(|e| e.to_string())?;
    let window: gtk::Window = page.gtk_window().map_err(|e| e.to_string())?.upcast();
    if let Some(layer) = layer {
        layer.init(&window, c"siler-tray", true);
    }
    let handle = app.clone();
    window.connect_key_press_event(move |_, key| {
        if key.keyval() == gtk::gdk::keys::constants::Escape {
            close_now(&handle);
            return glib::Propagation::Stop;
        }
        glib::Propagation::Proceed
    });
    if layer.is_none() {
        close_on_outside(app, &window);
    }
    window.connect_destroy(|_| {
        OPEN.store(false, SeqCst);
        UI.with_borrow_mut(|ui| {
            ui.window = None;
            ui.shown = None;
        });
    });
    UI.with_borrow_mut(|ui| ui.window = Some(window.clone()));
    Ok(window)
}

/// X11: the focus leaving closes the panel, and so does a click outside it, which the grab
/// brings here.
fn close_on_outside(app: &AppHandle, window: &gtk::Window) {
    let handle = app.clone();
    window.connect_focus_out_event(move |_, _| {
        close_now(&handle);
        glib::Propagation::Proceed
    });
    window.connect_map_event(|w, _| {
        let w = w.clone();
        // Once it's viewable: a window that isn't can't take the grab.
        glib::idle_add_local_once(move || grab(&w));
        glib::Propagation::Proceed
    });
    let outside = std::rc::Rc::new(std::cell::Cell::new(false));
    let pressed = outside.clone();
    window.add_events(
        gtk::gdk::EventMask::BUTTON_PRESS_MASK | gtk::gdk::EventMask::BUTTON_RELEASE_MASK,
    );
    window.connect_button_press_event(move |w, press| {
        let (x, y) = press.root();
        let ((left, top), (width, height)) = (w.position(), w.size());
        pressed.set(
            x < f64::from(left)
                || y < f64::from(top)
                || x >= f64::from(left + width)
                || y >= f64::from(top + height),
        );
        glib::Propagation::Proceed
    });
    let handle = app.clone();
    window.connect_button_release_event(move |_, _| {
        if outside.replace(false) {
            let handle = handle.clone();
            glib::idle_add_local_once(move || close_now(&handle));
        }
        glib::Propagation::Proceed
    });
}

fn grab(window: &gtk::Window) {
    if !is_open() {
        return;
    }
    let (Some(surface), Some(seat)) = (window.window(), window.display().default_seat()) else {
        return;
    };
    // Owner events: clicks inside reach the page as usual.
    let status = seat.grab(
        &surface,
        gtk::gdk::SeatCapabilities::ALL_POINTING,
        true,
        None,
        None,
        None,
    );
    if status != gtk::gdk::GrabStatus::Success {
        tracing::debug!(?status, "the tray panel couldn't grab the pointer");
    }
}

/// A transparent surface over a whole screen: a click on it closes the panel.
fn catcher(app: &AppHandle, layer: &layer::Layer, monitor: &gtk::gdk::Monitor) -> gtk::Window {
    let w = gtk::Window::new(gtk::WindowType::Toplevel);
    w.set_app_paintable(true);
    if let Some(visual) = gtk::prelude::GtkWindowExt::screen(&w).and_then(|s| s.rgba_visual()) {
        w.set_visual(Some(&visual));
    }
    layer.init(&w, c"siler-tray-catcher", false);
    layer.cover(&w, monitor);
    w.connect_draw(|_, cr| {
        cr.set_operator(gtk::cairo::Operator::Source);
        // Not fully clear, or some compositors don't map it.
        cr.set_source_rgba(0.0, 0.0, 0.0, 0.004);
        if let Err(e) = cr.paint() {
            tracing::debug!("the tray panel's catcher didn't paint: {e}");
        }
        glib::Propagation::Stop
    });
    w.add_events(gtk::gdk::EventMask::BUTTON_PRESS_MASK | gtk::gdk::EventMask::BUTTON_RELEASE_MASK);
    let app = app.clone();
    // On the release: a window destroyed between press and release leaves GTK waiting for
    // that release, and it swallows the next click.
    w.connect_button_release_event(move |_, _| {
        tracing::debug!("a click outside the tray panel");
        let app = app.clone();
        // Not from inside the event of a window that closing destroys.
        glib::idle_add_local_once(move || close_now(&app));
        glib::Propagation::Stop
    });
    w.show_all();
    w
}

fn close_now(app: &AppHandle) {
    if !OPEN.swap(false, SeqCst) {
        return;
    }
    tracing::debug!("tray panel closes");
    // Taken out first: hiding emits signals that come back here.
    let (catchers, window) = UI.with_borrow_mut(|ui| {
        ui.shown = None;
        ui.closed_at = Some(Instant::now());
        (std::mem::take(&mut ui.catchers), ui.window.clone())
    });
    for c in catchers {
        // SAFETY: the list held the last reference we use; nothing touches it afterwards.
        unsafe { c.destroy() };
    }
    if let Some(w) = window {
        let display = w.display();
        if !display.backend().is_wayland()
            && let Some(seat) = display.default_seat()
        {
            seat.ungrab();
        }
        w.hide();
    }
    let turn = TURN.fetch_add(1, SeqCst) + 1;
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(KEEP_HIDDEN).await;
        if TURN.load(SeqCst) == turn
            && let Some(w) = app.get_webview_window(LABEL)
        {
            let _ = w.destroy();
        }
    });
}
