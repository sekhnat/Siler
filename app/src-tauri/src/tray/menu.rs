//! The right-click menu: the running downloads, live, then Siler's actions.

use ksni::MenuItem;
use ksni::menu::{CheckmarkItem, RadioGroup, RadioItem, StandardItem, SubMenu};
use siler_core::{AfterDone, MAIN_QUEUE};
use tauri::{AppHandle, Manager};

use super::Tray;
use super::model::{self, Line};
use crate::AppState;
use crate::i18n::{tr, tr_args};

/// How many downloads the menu lists.
const IN_MENU: usize = 5;

const WHEN_DONE: [(AfterDone, &str); 5] = [
    (AfterDone::Nothing, "tray-do-nothing"),
    (AfterDone::Quit, "tray-quit-siler"),
    (AfterDone::Sleep, "tray-sleep"),
    (AfterDone::Hibernate, "tray-hibernate"),
    (AfterDone::PowerOff, "tray-shut-down"),
];

/// dbusmenu reads `_` as "underline the next letter".
fn label(text: &str) -> String {
    text.replace('_', "__")
}

fn item(text: &str, activate: impl Fn(&AppHandle) + Send + 'static) -> MenuItem<Tray> {
    StandardItem {
        label: label(text),
        activate: Box::new(move |t: &mut Tray| activate(&t.app)),
        ..Default::default()
    }
    .into()
}

fn note(text: &str) -> MenuItem<Tray> {
    StandardItem {
        label: label(text),
        enabled: false,
        ..Default::default()
    }
    .into()
}

fn engine(app: &AppHandle, f: impl FnOnce(&siler_core::Engine) -> siler_core::Result<()>) {
    if let Some(state) = app.try_state::<AppState>()
        && let Err(e) = f(&state.engine)
    {
        tracing::warn!("tray action failed: {e}");
    }
}

/// An invisible submenu whose number of children follows `generation`.
///
/// ksni gives items new ids, so a click on an outdated menu hits nothing, only when the
/// menu's layout changes. One download replacing another keeps the layout, and the click
/// would act on the newcomer. Changing this item's children makes it a new layout every time
/// the set of downloads changes.
fn guard(generation: u64) -> MenuItem<Tray> {
    let hidden = || {
        StandardItem {
            visible: false,
            ..Default::default()
        }
        .into()
    };
    SubMenu {
        visible: false,
        submenu: (0..=generation % 2).map(|_| hidden()).collect(),
        ..Default::default()
    }
    .into()
}

fn download(line: &Line) -> MenuItem<Tray> {
    let id = line.id;
    let mut actions = Vec::new();
    if line.can_finish {
        actions.push(item(&tr("tray-finish"), move |app| {
            engine(app, |e| e.finish(id))
        }));
    }
    actions.push(item(&tr("tray-pause"), move |app| {
        engine(app, |e| e.pause(id))
    }));
    actions.push(item(&tr("tray-open-folder"), move |app| {
        let app = app.clone();
        // Asking the file manager waits for D-Bus, and this runs on the tray's task.
        std::thread::spawn(move || {
            if let Err(e) = crate::commands::reveal(&app, id) {
                tracing::warn!("couldn't show the folder: {e}");
            }
        });
    }));
    SubMenu {
        label: label(&format!(
            "{}   {}",
            model::short(&line.name, 30),
            line.detail(true)
        )),
        submenu: actions,
        ..Default::default()
    }
    .into()
}

pub(super) fn build(tray: &Tray) -> Vec<MenuItem<Tray>> {
    let m = &tray.model;
    let mut items = vec![guard(tray.generation), note(&model::summary(m))];
    if !m.lines.is_empty() {
        items.push(MenuItem::Separator);
        items.extend(m.lines.iter().take(IN_MENU).map(download));
        if m.lines.len() > IN_MENU {
            items.push(note(&tr_args(
                "tray-and-more",
                vec![("count", (m.lines.len() - IN_MENU).into())],
            )));
        }
    }
    items.push(MenuItem::Separator);

    let state = tray.app.try_state::<AppState>();
    let when_done = state
        .as_ref()
        .map(|s| *s.power.when_done.lock().unwrap())
        .unwrap_or_default();
    let clipboard = state
        .as_ref()
        .is_some_and(|s| s.engine.settings().clipboard_monitor);
    items.extend([
        item(&tr("tray-show-siler"), super::open_siler),
        MenuItem::Separator,
        item(&tr("tray-pause-all"), |app| engine(app, |e| e.pause_all())),
        item(&tr("tray-resume-all"), |app| {
            engine(app, |e| e.resume_all())
        }),
        item(&tr("tray-start-queue"), |app| {
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
    ]);
    items
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn underscores_are_shown() {
        assert_eq!(label("my_file__2.iso"), "my__file____2.iso");
    }

    #[test]
    fn the_guard_changes_shape_with_every_generation() {
        let children = |g| match guard(g) {
            MenuItem::SubMenu(s) => {
                assert!(!s.visible);
                s.submenu.len()
            }
            _ => panic!("not a submenu"),
        };
        for g in 0..5 {
            assert_ne!(children(g), children(g + 1));
        }
    }
}
