//! Logs go to `~/.local/state/siler/logs/siler.<date>.log` (kept for a week), and to
//! stderr when Siler runs in a terminal. When the browser starts Siler there is no
//! terminal, so the file is the only record of what happened.
//!
//! Both are written through [`Redacting`], which takes signed links, logins and cookies
//! out of every line: users paste these logs into public bug reports.

use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;
use tracing::Metadata;
use tracing_appender::rolling::{Builder, Rotation};
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer, fmt};

/// Set to `1` to log addresses and headers in full, for debugging Siler itself.
const KEEP_SECRETS: &str = "SILER_LOG_SECRETS";

/// A writer that passes each log line through [`siler_ipc::redact::text`]. The formatter
/// hands over one whole event per write, so a secret is never split between two writes.
#[derive(Clone)]
pub struct Redacting<M> {
    inner: M,
    on: bool,
}

impl<M> Redacting<M> {
    pub fn new(inner: M) -> Redacting<M> {
        Redacting {
            inner,
            on: std::env::var(KEEP_SECRETS).as_deref() != Ok("1"),
        }
    }
}

impl<'a, M: MakeWriter<'a>> MakeWriter<'a> for Redacting<M> {
    type Writer = RedactingWriter<M::Writer>;

    fn make_writer(&'a self) -> Self::Writer {
        RedactingWriter {
            inner: self.inner.make_writer(),
            on: self.on,
        }
    }

    fn make_writer_for(&'a self, meta: &Metadata<'_>) -> Self::Writer {
        RedactingWriter {
            inner: self.inner.make_writer_for(meta),
            on: self.on,
        }
    }
}

pub struct RedactingWriter<W> {
    inner: W,
    on: bool,
}

impl<W: Write> Write for RedactingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if !self.on {
            return self.inner.write(buf);
        }
        let line = String::from_utf8_lossy(buf);
        self.inner
            .write_all(siler_ipc::redact::text(&line).as_bytes())?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

pub fn dir() -> PathBuf {
    siler_ipc::log_dir()
}

pub fn init() {
    let filter =
        || EnvFilter::try_from_default_env().unwrap_or_else(|_| "info,siler_core=info".into());
    let _ = std::fs::create_dir_all(dir());
    let file = Builder::new()
        .rotation(Rotation::DAILY)
        .filename_prefix("siler")
        .filename_suffix("log")
        .max_log_files(7)
        .build(dir())
        .ok()
        .map(|appender| {
            fmt::layer()
                .with_ansi(false)
                .with_writer(Redacting::new(appender))
                .with_filter(filter())
        });
    let console = std::io::stderr().is_terminal().then(|| {
        fmt::layer()
            .with_writer(Redacting::new(std::io::stderr))
            .with_filter(filter())
    });
    tracing_subscriber::registry()
        .with(file)
        .with(console)
        .init();

    std::panic::set_hook(Box::new(move |info| {
        let backtrace = std::backtrace::Backtrace::force_capture();
        tracing::error!("Siler crashed: {info}\n{backtrace}");
        // There may be no window to show this in (e.g. the web view failed to start), so
        // try a desktop notification too.
        let reason = info
            .payload()
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| info.payload().downcast_ref::<&str>().copied())
            .unwrap_or("unexpected error");
        let _ = siler_core::host::command("notify-send")
            .args(["--app-name=Siler", "--urgency=critical", "Siler crashed"])
            .arg(format!(
                "{}\nDetails are in {}",
                siler_ipc::redact::text(reason),
                dir().display()
            ))
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
        // In place of the default hook's message: when the browser started Siler, stderr
        // is `siler-stderr.log`, and the message may hold an address.
        eprintln!(
            "{}",
            siler_ipc::redact::text(&format!("Siler crashed: {info}"))
        );
    }));
    tracing::info!("Siler {} starting", env!("CARGO_PKG_VERSION"));
    if std::env::var(KEEP_SECRETS).as_deref() == Ok("1") {
        tracing::warn!("{KEEP_SECRETS}=1: logging addresses, logins and cookies in full");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct Buffer(Arc<Mutex<Vec<u8>>>);

    impl Write for Buffer {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn log_lines_lose_their_secrets() {
        let buf = Buffer::default();
        let out = buf.clone();
        for ansi in [false, true] {
            let layer = fmt::layer().with_ansi(ansi).with_writer(Redacting {
                inner: {
                    let b = buf.clone();
                    move || b.clone()
                },
                on: true,
            });
            let subscriber = tracing_subscriber::registry().with(layer);
            tracing::subscriber::with_default(subscriber, || {
                let url = "https://bucket.example/f.iso?X-Amz-Signature=deadbeefcafe0123";
                tracing::info!(url = %url, "mirror disabled");
                tracing::warn!("probe failed: error sending request for url ({url})");
                tracing::debug!(cookie = "sid=s3cretvalue", "sent");
            });
        }
        let text = String::from_utf8(out.0.lock().unwrap().clone()).unwrap();
        assert!(text.contains("X-Amz-Signature=…"), "{text}");
        assert!(!text.contains("deadbeefcafe0123"), "{text}");
        assert!(!text.contains("s3cretvalue"), "{text}");
    }
}
