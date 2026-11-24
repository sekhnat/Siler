//! What happens when a queue (or everything) has finished: quit Siler, or put the
//! computer to sleep, hibernate it or shut it down through logind, after a countdown the
//! user can cancel.

use serde::Serialize;
use siler_core::AfterDone;
use std::future::Future;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_notification::NotificationExt;
use tokio_util::sync::CancellationToken;

use crate::AppState;
use crate::i18n::{tr, tr_args};

/// Seconds between the announcement and the action.
pub const COUNTDOWN: u64 = 60;

/// The logind method for an action, and the matching `Can…` query.
fn logind_method(action: AfterDone) -> Option<(&'static str, &'static str)> {
    match action {
        AfterDone::Sleep => Some(("Suspend", "CanSuspend")),
        AfterDone::Hibernate => Some(("Hibernate", "CanHibernate")),
        AfterDone::PowerOff => Some(("PowerOff", "CanPowerOff")),
        AfterDone::Nothing | AfterDone::Quit => None,
    }
}

/// Talks to the power manager; replaced by a fake in tests.
pub trait Power {
    /// The `Can…` answer: "yes", "no", "challenge" (asks for a password) or "na".
    fn can(&self, query: &'static str) -> impl Future<Output = String> + Send;
    fn act(&self, method: &'static str) -> impl Future<Output = Result<(), String>> + Send;
}

pub struct Logind;

impl Power for Logind {
    async fn can(&self, query: &'static str) -> String {
        let reply = async {
            let conn = zbus::Connection::system().await?;
            let r = conn
                .call_method(
                    Some("org.freedesktop.login1"),
                    "/org/freedesktop/login1",
                    Some("org.freedesktop.login1.Manager"),
                    query,
                    &(),
                )
                .await?;
            r.body().deserialize::<String>()
        };
        reply.await.unwrap_or_else(|e: zbus::Error| {
            tracing::debug!("{query}: {e}");
            "na".into()
        })
    }

    async fn act(&self, method: &'static str) -> Result<(), String> {
        let conn = zbus::Connection::system()
            .await
            .map_err(|e| e.to_string())?;
        // `true`: let polkit ask for a password if the system wants one.
        conn.call_method(
            Some("org.freedesktop.login1"),
            "/org/freedesktop/login1",
            Some("org.freedesktop.login1.Manager"),
            method,
            &(true,),
        )
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())
    }
}

/// Which actions this computer allows.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct PowerCaps {
    pub sleep: bool,
    pub hibernate: bool,
    pub power_off: bool,
}

pub async fn caps(p: &impl Power) -> PowerCaps {
    let ok = |s: String| s == "yes" || s == "challenge";
    PowerCaps {
        sleep: ok(p.can("CanSuspend").await),
        hibernate: ok(p.can("CanHibernate").await),
        power_off: ok(p.can("CanPowerOff").await),
    }
}

/// Carries out a power action (not Quit or Nothing).
pub async fn perform(p: &impl Power, action: AfterDone) -> Result<(), String> {
    match logind_method(action) {
        Some((method, _)) => p.act(method).await,
        None => Ok(()),
    }
}

/// A countdown in progress.
pub struct PendingPower {
    pub action: AfterDone,
    pub reason: String,
    pub deadline: Instant,
    pub cancel: CancellationToken,
}

#[derive(Debug, Clone, Serialize)]
pub struct PendingPowerView {
    pub action: AfterDone,
    pub reason: String,
    pub seconds_left: u64,
}

#[derive(Default)]
pub struct PowerState {
    pub pending: Mutex<Option<PendingPower>>,
    /// "When all downloads finish": a one-off action, not saved.
    pub when_done: Mutex<AfterDone>,
}

impl PowerState {
    pub fn view(&self) -> Option<PendingPowerView> {
        self.pending
            .lock()
            .unwrap()
            .as_ref()
            .map(|p| PendingPowerView {
                action: p.action,
                reason: p.reason.clone(),
                seconds_left: p
                    .deadline
                    .saturating_duration_since(Instant::now())
                    .as_secs(),
            })
    }

    pub fn cancel(&self) -> bool {
        match self.pending.lock().unwrap().take() {
            Some(p) => {
                p.cancel.cancel();
                true
            }
            None => false,
        }
    }
}

/// The action, as the catalog's messages select on it (and as the log says it).
fn verb(action: AfterDone) -> &'static str {
    match action {
        AfterDone::Sleep => "sleep",
        AfterDone::Hibernate => "hibernate",
        AfterDone::PowerOff => "power_off",
        AfterDone::Quit => "quit",
        AfterDone::Nothing => "nothing",
    }
}

/// On the main thread, for the reason `progress::open` gives.
fn open_countdown_window(app: &AppHandle) {
    let on_main = app.clone();
    let _ = app.run_on_main_thread(move || open_countdown_window_here(&on_main));
}

fn open_countdown_window_here(app: &AppHandle) {
    crate::tray::panel::close(app);
    if let Some(w) = app.get_webview_window("countdown") {
        let _ = w.show();
        let _ = w.set_focus();
        return;
    }
    let built = WebviewWindowBuilder::new(
        app,
        "countdown",
        WebviewUrl::App("index.html#countdown".into()),
    )
    .title("Siler")
    .inner_size(420.0, 210.0)
    .resizable(false)
    .always_on_top(true)
    .center()
    .focused(true)
    .build();
    if let Err(e) = built {
        tracing::warn!("could not open the countdown window: {e}");
        crate::show_main(app);
    }
}

/// Runs `action` because of `reason` ("The queue “Night” has finished"): Quit at once,
/// power actions after a countdown that can be cancelled.
pub fn schedule(app: &AppHandle, action: AfterDone, reason: String) {
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    match action {
        AfterDone::Nothing => return,
        AfterDone::Quit => {
            tracing::info!("{reason}; quitting");
            app.exit(0);
            return;
        }
        _ => {}
    }
    tracing::info!("{reason}; will {} in {COUNTDOWN} s", verb(action));
    let cancel = CancellationToken::new();
    if let Some(old) = state.power.pending.lock().unwrap().replace(PendingPower {
        action,
        reason: reason.clone(),
        deadline: Instant::now() + Duration::from_secs(COUNTDOWN),
        cancel: cancel.clone(),
    }) {
        old.cancel.cancel();
    }
    let _ = app.emit("power-countdown", state.power.view());
    open_countdown_window(app);
    let _ = app
        .notification()
        .builder()
        .title(tr_args(
            "power-will-in-a-minute",
            vec![("action", verb(action).into())],
        ))
        .body(crate::body_text(&tr_args(
            "power-open-siler-to-cancel",
            vec![("reason", reason.as_str().into())],
        )))
        .show();
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(COUNTDOWN)) => {}
            _ = cancel.cancelled() => return,
        }
        run_now(&app).await;
    });
}

/// Carries out the pending action right away ("Do it now", or the countdown ran out).
pub async fn run_now(app: &AppHandle) {
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    let Some(p) = state.power.pending.lock().unwrap().take() else {
        return;
    };
    p.cancel.cancel();
    if let Some(w) = app.get_webview_window("countdown") {
        let _ = w.close();
    }
    tracing::info!("{}: {}", p.reason, verb(p.action));
    if let Err(e) = perform(&Logind, p.action).await {
        tracing::warn!("couldn't {}: {e}", verb(p.action));
        let _ = app
            .notification()
            .builder()
            .title(tr_args(
                "power-couldnt",
                vec![("action", verb(p.action).into())],
            ))
            .body(crate::body_text(&e))
            .show();
    }
}

/// After a download finished or failed: if nothing is left to run and an action was chosen
/// for "when all downloads finish", start it (once).
pub fn check_all_done(app: &AppHandle) {
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    if *state.power.when_done.lock().unwrap() == AfterDone::Nothing {
        return;
    }
    let busy = state
        .engine
        .list()
        .iter()
        .any(|d| d.status.is_running() || d.status == siler_core::Status::Queued);
    if busy {
        return;
    }
    let action = std::mem::take(&mut *state.power.when_done.lock().unwrap());
    let _ = app.emit("when-done-changed", AfterDone::Nothing);
    crate::tray::refresh();
    schedule(app, action, tr("power-all-downloads-finished"));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Fake {
        calls: Mutex<Vec<&'static str>>,
    }

    impl Power for Fake {
        async fn can(&self, query: &'static str) -> String {
            match query {
                "CanSuspend" => "yes",
                "CanHibernate" => "na",
                _ => "challenge",
            }
            .into()
        }
        async fn act(&self, method: &'static str) -> Result<(), String> {
            self.calls.lock().unwrap().push(method);
            Ok(())
        }
    }

    #[tokio::test]
    async fn power_actions() {
        let f = Fake::default();
        assert_eq!(
            caps(&f).await,
            PowerCaps {
                sleep: true,
                hibernate: false,
                power_off: true
            }
        );
        for a in [
            AfterDone::Sleep,
            AfterDone::Nothing,
            AfterDone::Quit,
            AfterDone::Hibernate,
            AfterDone::PowerOff,
        ] {
            perform(&f, a).await.unwrap();
        }
        assert_eq!(
            *f.calls.lock().unwrap(),
            ["Suspend", "Hibernate", "PowerOff"]
        );
    }

    #[test]
    fn cancelling() {
        let s = PowerState::default();
        assert!(!s.cancel());
        let token = CancellationToken::new();
        *s.pending.lock().unwrap() = Some(PendingPower {
            action: AfterDone::PowerOff,
            reason: "done".into(),
            deadline: Instant::now() + Duration::from_secs(30),
            cancel: token.clone(),
        });
        let v = s.view().unwrap();
        assert!(v.seconds_left >= 29 && v.seconds_left <= 30);
        assert!(s.cancel());
        assert!(token.is_cancelled());
        assert!(s.view().is_none());
    }
}
