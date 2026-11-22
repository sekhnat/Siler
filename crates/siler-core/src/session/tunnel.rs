//! A private tunnel through a proxy for reqwest: a socket without a file (Linux's abstract
//! namespace) that takes connections from this process only, each of which is dialled to
//! one server through one [`Exit`]. reqwest does TLS over it end to end, and so shows the
//! server's certificate.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use tokio::net::{UnixListener, UnixStream};

use crate::transport::dial::Exit;
use crate::{Error, Result};

pub(crate) struct Tunnel {
    path: PathBuf,
    made: Arc<AtomicU64>,
    task: tokio::task::JoinHandle<()>,
}

/// Whether the other end is this process. Anyone can connect to a socket without a file,
/// and a tunnel lends out the proxy's login.
fn is_ours(stream: &UnixStream) -> bool {
    stream
        .peer_cred()
        .ok()
        .and_then(|cred| cred.pid())
        .and_then(|pid| u32::try_from(pid).ok())
        == Some(std::process::id())
}

impl Tunnel {
    pub fn open(exit: Exit, host: String, port: u16) -> Result<Tunnel> {
        let name = u64::from_le_bytes(super::windows::random()?);
        // The leading NUL makes it abstract.
        let path = PathBuf::from(format!("\0siler-tunnel-{name:016x}"));
        let listener = UnixListener::bind(&path).map_err(Error::Io)?;
        let made = Arc::new(AtomicU64::new(0));
        let count = made.clone();
        let task = tokio::spawn(async move {
            loop {
                let mut local = match listener.accept().await {
                    Ok((local, _)) => local,
                    Err(e) => {
                        // Out of file descriptors, say: not to be spun on.
                        tracing::warn!("tunnel: {e}");
                        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                        continue;
                    }
                };
                if !is_ours(&local) {
                    tracing::warn!("another program tried one of Siler's tunnels");
                    continue;
                }
                count.fetch_add(1, Relaxed);
                let (exit, host) = (exit.clone(), host.clone());
                tokio::spawn(async move {
                    // Dropping `local` is the failure; the caller finds out why itself.
                    let mut remote = match exit.dial(&host, port).await {
                        Ok(remote) => remote,
                        Err(why) => {
                            tracing::debug!(?why, "tunnel to {host}:{port} failed");
                            return;
                        }
                    };
                    if let Err(e) = tokio::io::copy_bidirectional(&mut local, &mut remote).await {
                        tracing::debug!("tunnel to {host}:{port} ended: {e}");
                    }
                });
            }
        });
        Ok(Tunnel { path, made, task })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Connections made so far: the same number before and after means the same connection.
    pub fn made(&self) -> u64 {
        self.made.load(Relaxed)
    }
}

impl Drop for Tunnel {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OTHER_PROCESS: &str = "SILER_TEST_CONNECT_TO";

    #[tokio::test]
    async fn only_this_process_may_use_a_tunnel() {
        // The second process is this test again, which only connects.
        if let Ok(name) = std::env::var(OTHER_PROCESS) {
            let stream = UnixStream::connect(format!("\0{name}")).await.unwrap();
            stream.readable().await.unwrap();
            return;
        }
        let name = format!("siler-test-{:016x}", fastrand::u64(..));
        let path = format!("\0{name}");
        let listener = UnixListener::bind(&path).unwrap();

        let _mine = UnixStream::connect(&path).await.unwrap();
        let (accepted, _) = listener.accept().await.unwrap();
        assert!(is_ours(&accepted));

        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "session::tunnel::tests::only_this_process_may_use_a_tunnel",
            ])
            .env(OTHER_PROCESS, &name)
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let wait = std::time::Duration::from_secs(20);
        let (accepted, _) = tokio::time::timeout(wait, listener.accept())
            .await
            .expect("the other process connects")
            .unwrap();
        assert!(!is_ours(&accepted));
        drop(accepted);
        child.wait().unwrap();
    }
}
