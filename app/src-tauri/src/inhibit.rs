//! Keeps the computer from going to sleep while downloads run, through a logind
//! "sleep" inhibitor (shown by KDE and GNOME as an app blocking sleep).

use kstar_core::Engine;
use std::time::{Duration, Instant};
use tokio::sync::watch;
use zbus::zvariant::OwnedFd;

#[derive(Debug, PartialEq, Eq)]
enum Action {
    Acquire,
    Release,
    Keep,
}

fn decide(held: bool, active: usize, enabled: bool) -> Action {
    match (held, enabled && active > 0) {
        (false, true) => Action::Acquire,
        (true, false) => Action::Release,
        _ => Action::Keep,
    }
}

async fn inhibit() -> zbus::Result<OwnedFd> {
    let conn = zbus::Connection::system().await?;
    let reply = conn
        .call_method(
            Some("org.freedesktop.login1"),
            "/org/freedesktop/login1",
            Some("org.freedesktop.login1.Manager"),
            "Inhibit",
            &("sleep", "K-Star", "Downloads in progress", "block"),
        )
        .await?;
    reply.body().deserialize::<OwnedFd>()
}

/// Starts the inhibitor task; send it the number of running downloads.
pub fn spawn(engine: Engine) -> watch::Sender<usize> {
    let (tx, mut rx) = watch::channel(0usize);
    tauri::async_runtime::spawn(async move {
        let mut held: Option<OwnedFd> = None;
        let mut retry_at: Option<Instant> = None;
        let mut warned = false;
        let mut tick = tokio::time::interval(Duration::from_secs(5));
        loop {
            tokio::select! {
                r = rx.changed() => if r.is_err() { break },
                _ = tick.tick() => {} // picks up a changed setting
            }
            let active = *rx.borrow_and_update();
            match decide(held.is_some(), active, engine.settings().prevent_sleep) {
                Action::Acquire if retry_at.is_none_or(|t| Instant::now() >= t) => {
                    match inhibit().await {
                        Ok(fd) => {
                            tracing::info!("preventing sleep while downloading");
                            held = Some(fd);
                            retry_at = None;
                        }
                        Err(e) => {
                            if !warned {
                                tracing::warn!("can't prevent sleep: {e}");
                                warned = true;
                            }
                            retry_at = Some(Instant::now() + Duration::from_secs(60));
                        }
                    }
                }
                Action::Release => {
                    held = None; // closing the fd ends the inhibitor
                    tracing::info!("sleep allowed again");
                }
                _ => {}
            }
        }
    });
    tx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decisions() {
        assert_eq!(decide(false, 0, true), Action::Keep);
        assert_eq!(decide(false, 2, true), Action::Acquire);
        assert_eq!(decide(true, 2, true), Action::Keep);
        assert_eq!(decide(true, 0, true), Action::Release);
        assert_eq!(decide(true, 3, false), Action::Release);
        assert_eq!(decide(false, 3, false), Action::Keep);
    }
}
