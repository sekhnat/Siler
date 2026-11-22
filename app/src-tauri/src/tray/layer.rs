//! gtk-layer-shell, loaded when the program runs: it is optional, and it only works where
//! the compositor has the layer-shell protocol (KWin, wlroots; not GNOME).

use gtk::glib::translate::ToGlibPtr;
use libloading::Library;
use std::ffi::c_int;
use std::sync::OnceLock;

use super::place::Edge;

/// GTK_LAYER_SHELL_LAYER_OVERLAY
const OVERLAY: c_int = 3;
pub const EDGES: [Edge; 4] = [Edge::Left, Edge::Right, Edge::Top, Edge::Bottom];

type Window = *mut gtk::ffi::GtkWindow;
type Monitor = *mut gtk::gdk::ffi::GdkMonitor;

pub struct Layer {
    is_supported: unsafe extern "C" fn() -> c_int,
    init_for_window: unsafe extern "C" fn(Window),
    set_namespace: unsafe extern "C" fn(Window, *const std::ffi::c_char),
    set_layer: unsafe extern "C" fn(Window, c_int),
    set_monitor: unsafe extern "C" fn(Window, Monitor),
    set_anchor: unsafe extern "C" fn(Window, c_int, c_int),
    set_margin: unsafe extern "C" fn(Window, c_int, c_int),
    set_exclusive_zone: unsafe extern "C" fn(Window, c_int),
    set_keyboard_interactivity: unsafe extern "C" fn(Window, c_int),
    _lib: Library,
}

static LAYER: OnceLock<Option<Layer>> = OnceLock::new();

fn code(edge: Edge) -> c_int {
    match edge {
        Edge::Left => 0,
        Edge::Right => 1,
        Edge::Top => 2,
        Edge::Bottom => 3,
    }
}

fn load() -> Result<Layer, libloading::Error> {
    // SAFETY: the library's initialisers have no preconditions, and each symbol's type is
    // the one in gtk-layer-shell.h (gboolean and the enums are C ints). The fields can't
    // outlive `_lib`, which lives in a static.
    unsafe {
        let lib = Library::new("libgtk-layer-shell.so.0")?;
        Ok(Layer {
            is_supported: *lib.get(b"gtk_layer_is_supported\0")?,
            init_for_window: *lib.get(b"gtk_layer_init_for_window\0")?,
            set_namespace: *lib.get(b"gtk_layer_set_namespace\0")?,
            set_layer: *lib.get(b"gtk_layer_set_layer\0")?,
            set_monitor: *lib.get(b"gtk_layer_set_monitor\0")?,
            set_anchor: *lib.get(b"gtk_layer_set_anchor\0")?,
            set_margin: *lib.get(b"gtk_layer_set_margin\0")?,
            set_exclusive_zone: *lib.get(b"gtk_layer_set_exclusive_zone\0")?,
            set_keyboard_interactivity: *lib.get(b"gtk_layer_set_keyboard_interactivity\0")?,
            _lib: lib,
        })
    }
}

/// Layer-shell, when it works here. GTK's main thread only.
pub fn get() -> Option<&'static Layer> {
    LAYER
        .get_or_init(|| {
            let layer = load()
                .inspect_err(|e| tracing::info!("no gtk-layer-shell, so no tray panel: {e}"))
                .ok()?;
            // SAFETY: GTK is initialised by the time the tray can be clicked.
            if unsafe { (layer.is_supported)() } == 0 {
                tracing::info!("the compositor has no layer-shell, so no tray panel");
                return None;
            }
            Some(layer)
        })
        .as_ref()
}

// Every method is called on GTK's main thread with a live window, which the borrow ensures.
impl Layer {
    /// Makes a window that isn't shown yet a surface above everything else.
    pub fn init(&self, w: &gtk::Window, namespace: &std::ffi::CStr, keyboard: bool) {
        let p: Window = w.to_glib_none().0;
        // SAFETY: see the note on this impl; the namespace is copied.
        unsafe {
            (self.init_for_window)(p);
            (self.set_namespace)(p, namespace.as_ptr());
            (self.set_layer)(p, OVERLAY);
            // Exclusive: Escape works without clicking the panel first.
            (self.set_keyboard_interactivity)(p, c_int::from(keyboard));
        }
    }

    /// The whole screen, also under its panels.
    pub fn cover(&self, w: &gtk::Window, monitor: &gtk::gdk::Monitor) {
        let p: Window = w.to_glib_none().0;
        // SAFETY: see the note on this impl.
        unsafe {
            (self.set_monitor)(p, monitor.to_glib_none().0);
            for e in EDGES {
                (self.set_anchor)(p, code(e), 1);
            }
            (self.set_exclusive_zone)(p, -1);
        }
    }

    /// At `edge` of the screen, `along` pixels from its start, clear of the desktop's panels.
    pub fn anchor(&self, w: &gtk::Window, monitor: &gtk::gdk::Monitor, edge: Edge, along: i32) {
        let p: Window = w.to_glib_none().0;
        let side = match edge {
            Edge::Bottom | Edge::Top => Edge::Left,
            Edge::Left | Edge::Right => Edge::Top,
        };
        // SAFETY: see the note on this impl.
        unsafe {
            (self.set_monitor)(p, monitor.to_glib_none().0);
            for e in EDGES {
                let margin = if e == edge {
                    super::place::GAP
                } else if e == side {
                    along
                } else {
                    0
                };
                (self.set_anchor)(p, code(e), c_int::from(e == edge || e == side));
                (self.set_margin)(p, code(e), margin);
            }
            (self.set_exclusive_zone)(p, 0);
        }
    }
}
