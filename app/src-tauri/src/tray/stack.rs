//! The tray icon of the progress windows put away in the tray: one icon for all of them,
//! there only while there are some. A left click lists them in the panel; the menu brings
//! back one or all.
//!
//! One task owns the icon and makes it match [`crate::progress::in_tray`] whenever it is
//! poked, so spawning, updating and removing it never race.

use ksni::menu::StandardItem;
use ksni::{Handle, MenuItem, ToolTip, TrayMethods};
use siler_core::Status;
use std::sync::OnceLock;
use tauri::{AppHandle, Manager};
use tokio::sync::Notify;

use super::{icon, model, panel};
use crate::AppState;
use crate::i18n::{tr, tr_args};

static POKE: OnceLock<Notify> = OnceLock::new();

fn poke() -> &'static Notify {
    POKE.get_or_init(Notify::new)
}

/// What the icon shows of one window's download.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub id: i64,
    pub name: String,
    pub percent: Option<u8>,
    pub speed: u64,
    pub status: Status,
}

impl Entry {
    fn of(d: &siler_core::DownloadInfo) -> Entry {
        let name = d
            .filename
            .clone()
            .or_else(|| siler_core::filename::name_from_url(&d.url))
            .unwrap_or_else(|| tr("notify-a-download"));
        let percent = match (d.status, d.total_size) {
            (Status::Completed, _) => Some(100),
            (_, Some(total)) if total > 0 => Some((d.downloaded.min(total) * 100 / total) as u8),
            _ => None,
        };
        Entry {
            id: d.id,
            name,
            percent,
            speed: d.speed,
            status: d.status,
        }
    }

    fn line(&self) -> String {
        let pct = self
            .percent
            .map_or_else(|| "…".to_string(), |p| format!("{p}%"));
        let mut text = format!("{pct}  {}", model::short(&self.name, 36));
        if self.speed > 0 {
            text.push_str(&format!("  ·  {}", model::rate(self.speed)));
        } else if self.status == Status::Paused {
            text.push_str(&format!("  ·  {}", tr("tray-stack-paused")));
        }
        text
    }
}

/// The percent of everything in the stack whose size is known.
fn combined(entries: &[Entry]) -> icon::Ring {
    let known: Vec<u8> = entries.iter().filter_map(|e| e.percent).collect();
    if known.is_empty() {
        icon::Ring::Arc
    } else {
        let avg = known.iter().map(|&p| u32::from(p)).sum::<u32>() / known.len() as u32;
        icon::Ring::At((avg as u8) / 2 * 2)
    }
}

pub struct Stack {
    app: AppHandle,
    entries: Vec<Entry>,
    /// Goes up whenever the set of windows changes (see `menu::guard`).
    generation: u64,
    look: icon::StackLook,
    icons: Vec<ksni::Icon>,
}

impl Stack {
    fn new(app: AppHandle, entries: Vec<Entry>) -> Stack {
        let look = icon::StackLook::of(combined(&entries), entries.len(), crate::skins::accent());
        Stack {
            app,
            icons: icon::render_stack(&look),
            entries,
            generation: 0,
            look,
        }
    }

    fn set(&mut self, entries: Vec<Entry>) {
        let ids = |e: &[Entry]| e.iter().map(|e| e.id).collect::<Vec<_>>();
        if ids(&entries) != ids(&self.entries) {
            self.generation += 1;
        }
        let look = icon::StackLook::of(combined(&entries), entries.len(), crate::skins::accent());
        if look != self.look {
            self.icons = icon::render_stack(&look);
            self.look = look;
        }
        self.entries = entries;
    }
}

impl ksni::Tray for Stack {
    fn id(&self) -> String {
        format!("{}.Progress", crate::desktop::APP_ID)
    }

    fn title(&self) -> String {
        tr("tray-stack-title")
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        self.icons.clone()
    }

    fn tool_tip(&self) -> ToolTip {
        ToolTip {
            title: tr_args(
                "tray-stack-tooltip",
                vec![("count", self.entries.len().into())],
            ),
            description: model::markup(
                &self
                    .entries
                    .iter()
                    .map(Entry::line)
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            ..Default::default()
        }
    }

    fn activate(&mut self, x: i32, y: i32) {
        let token = ksni::current_xdg_activation_token();
        if let [only] = self.entries.as_slice() {
            crate::progress::restore(&self.app, only.id, token);
        } else {
            panel::toggle(&self.app, x, y, token, panel::Mode::Stack);
        }
    }

    fn secondary_activate(&mut self, _x: i32, _y: i32) {
        crate::progress::restore_all(&self.app, ksni::current_xdg_activation_token());
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let item = |label: String, f: Box<dyn Fn(&mut Stack) + Send>| -> MenuItem<Stack> {
            StandardItem {
                label: label.replace('_', "__"),
                activate: f,
                ..Default::default()
            }
            .into()
        };
        let mut items = vec![guard(self.generation)];
        for e in &self.entries {
            let id = e.id;
            items.push(item(
                e.line(),
                Box::new(move |s: &mut Stack| {
                    crate::progress::restore(&s.app, id, ksni::current_xdg_activation_token())
                }),
            ));
        }
        items.push(MenuItem::Separator);
        items.push(item(
            tr("tray-stack-restore-all"),
            Box::new(|s: &mut Stack| {
                crate::progress::restore_all(&s.app, ksni::current_xdg_activation_token())
            }),
        ));
        items.push(item(
            tr("tray-show-siler"),
            Box::new(|s: &mut Stack| {
                crate::show_main_with(&s.app, ksni::current_xdg_activation_token())
            }),
        ));
        items
    }
}

/// See `menu::guard`: a menu changes layout whenever the set of windows does.
fn guard(generation: u64) -> MenuItem<Stack> {
    let hidden = || {
        StandardItem {
            visible: false,
            ..Default::default()
        }
        .into()
    };
    ksni::menu::SubMenu {
        visible: false,
        submenu: (0..=generation % 2).map(|_| hidden()).collect(),
        ..Default::default()
    }
    .into()
}

/// The windows in the tray changed, or their downloads did: the icon follows.
pub fn sync() {
    poke().notify_one();
}

/// Starts the task that owns the icon; once, with the tray.
pub fn start(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let mut handle: Option<Handle<Stack>> = None;
        loop {
            poke().notified().await;
            let entries: Vec<Entry> = match app.try_state::<AppState>() {
                Some(state) => crate::progress::in_tray()
                    .into_iter()
                    .filter_map(|id| state.engine.get(id))
                    .map(|d| Entry::of(&d))
                    .collect(),
                None => Vec::new(),
            };
            match (&handle, entries.is_empty()) {
                (Some(h), true) => {
                    h.shutdown().await;
                    handle = None;
                }
                (Some(h), false) => {
                    h.update(|s| s.set(entries)).await;
                }
                (None, false) => match Stack::new(app.clone(), entries).spawn().await {
                    Ok(h) => handle = Some(h),
                    Err(e) => tracing::warn!("no tray icon for the progress windows: {e}"),
                },
                (None, true) => {}
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: i64, percent: Option<u8>) -> Entry {
        Entry {
            id,
            name: "a.iso".into(),
            percent,
            speed: 0,
            status: Status::Downloading,
        }
    }

    #[test]
    fn the_ring_averages_what_is_known() {
        assert_eq!(combined(&[entry(1, None)]), icon::Ring::Arc);
        assert_eq!(
            combined(&[entry(1, Some(20)), entry(2, Some(61)), entry(3, None)]),
            icon::Ring::At(40)
        );
    }

    #[test]
    fn the_guard_changes_shape_with_every_generation() {
        let children = |g| match guard(g) {
            MenuItem::SubMenu(s) => s.submenu.len(),
            _ => panic!("not a submenu"),
        };
        assert_ne!(children(0), children(1));
    }
}
