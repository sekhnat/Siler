//! Logs go to `~/.local/state/kstar/logs/kstar.<date>.log` (kept for a week), and to
//! stderr when K-Star runs in a terminal. When the browser starts K-Star there is no
//! terminal, so the file is the only record of what happened.

use std::io::IsTerminal;
use std::path::PathBuf;
use tracing_appender::rolling::{Builder, Rotation};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer, fmt};

pub fn dir() -> PathBuf {
    kstar_ipc::log_dir()
}

pub fn init() {
    let filter =
        || EnvFilter::try_from_default_env().unwrap_or_else(|_| "info,kstar_core=info".into());
    let _ = std::fs::create_dir_all(dir());
    let file = Builder::new()
        .rotation(Rotation::DAILY)
        .filename_prefix("kstar")
        .filename_suffix("log")
        .max_log_files(7)
        .build(dir())
        .ok()
        .map(|appender| {
            fmt::layer()
                .with_ansi(false)
                .with_writer(appender)
                .with_filter(filter())
        });
    let console = std::io::stderr().is_terminal().then(|| {
        fmt::layer()
            .with_writer(std::io::stderr)
            .with_filter(filter())
    });
    tracing_subscriber::registry()
        .with(file)
        .with(console)
        .init();

    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let backtrace = std::backtrace::Backtrace::force_capture();
        tracing::error!("K-Star crashed: {info}\n{backtrace}");
        // There may be no window to show this in (e.g. the web view failed to start), so
        // try a desktop notification too.
        let reason = info
            .payload()
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| info.payload().downcast_ref::<&str>().copied())
            .unwrap_or("unexpected error");
        let _ = kstar_core::host::command("notify-send")
            .args(["--app-name=K-Star", "--urgency=critical", "K-Star crashed"])
            .arg(format!("{reason}\nDetails are in {}", dir().display()))
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
        default_hook(info);
    }));
    tracing::info!("K-Star {} starting", env!("CARGO_PKG_VERSION"));
}
