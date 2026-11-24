//! HTTP/2 slots against real CDNs, on a real connection: how fast each setting is. Needs
//! the network and downloads about a gigabyte per round, so it is ignored by default:
//!
//! ```sh
//! SILER_REAL_H2=1 cargo test --release -p siler-core --test real_h2 -- --ignored --nocapture
//! ```
//!
//! `SILER_REAL_H2_ONLY=kernel` runs one file, `SILER_REAL_H2_ROUNDS=2` repeats the matrix.
//! Each run is a fresh engine (cold connections) and its file is deleted afterwards. The
//! settings are tried in turn within a round, so a change in the line's speed over time
//! touches them alike. No user data is sent; requests carry Siler's default User-Agent.

use siler_core::{Engine, NewDownload, Settings, Status};
use std::time::{Duration, Instant};

const FILES: &[(&str, &str)] = &[
    (
        "kernel.org (Fastly)",
        "https://cdn.kernel.org/pub/linux/kernel/v6.x/linux-6.12.tar.xz",
    ),
    (
        "dl.google.com",
        "https://dl.google.com/linux/direct/google-chrome-stable_current_amd64.deb",
    ),
    (
        "download.blender.org",
        "https://download.blender.org/release/Blender4.2/blender-4.2.0-linux-x64.tar.xz",
    ),
];

/// (connections, HTTP/2 connections per server): HTTP/1.1 as today, then 1, 4 and 16 slots,
/// and a lone stream with slots on (it stays on HTTP/1.1 by design; the check that it does).
const SETTINGS: &[(u8, u8)] = &[(8, 0), (8, 1), (8, 4), (8, 16), (1, 4)];

struct Run {
    file: &'static str,
    connections: u8,
    slots: u8,
    outcome: Result<Measured, String>,
}

struct Measured {
    mb_s: f64,
    protocol: Option<u8>,
    retries: u32,
    wasted: u64,
}

async fn one(url: &str, connections: u8, slots: u8) -> Result<Measured, String> {
    let tmp = tempfile::tempdir().map_err(|e| e.to_string())?;
    let e = Engine::open(tmp.path().join("data")).map_err(|e| e.to_string())?;
    e.set_settings(Settings {
        download_dir: tmp.path().join("dl"),
        use_category_folders: false,
        connections,
        http2_connections_per_server: slots,
        ..e.settings()
    })
    .map_err(|e| e.to_string())?;
    let id = e
        .add(NewDownload {
            url: url.to_string(),
            ..Default::default()
        })
        .map_err(|e| e.to_string())?;
    let started = Instant::now();
    let deadline = started + Duration::from_secs(600);
    let mut protocol = None;
    loop {
        let d = e.get(id).ok_or("gone")?;
        protocol = protocol.or(d.protocol);
        match d.status {
            Status::Completed => {
                let secs = started.elapsed().as_secs_f64();
                let traffic = d.traffic.unwrap_or_default();
                return Ok(Measured {
                    mb_s: d.downloaded as f64 / 1e6 / secs,
                    protocol,
                    retries: traffic.retries,
                    wasted: traffic.wasted,
                });
            }
            Status::Failed => return Err(d.error.unwrap_or_default()),
            _ => {}
        }
        if Instant::now() > deadline {
            return Err("over 10 minutes".into());
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the network: SILER_REAL_H2=1 … -- --ignored"]
async fn http2_slots_on_real_cdns() {
    if std::env::var("SILER_REAL_H2").as_deref() != Ok("1") {
        eprintln!("skipped: set SILER_REAL_H2=1");
        return;
    }
    let only = std::env::var("SILER_REAL_H2_ONLY").ok();
    let rounds: usize = std::env::var("SILER_REAL_H2_ROUNDS")
        .ok()
        .and_then(|r| r.parse().ok())
        .unwrap_or(1);
    let mut runs = Vec::new();
    for round in 1..=rounds {
        for (file, url) in FILES {
            if only
                .as_deref()
                .is_some_and(|o| !file.contains(o) && !url.contains(o))
            {
                continue;
            }
            for &(connections, slots) in SETTINGS {
                let outcome = one(url, connections, slots).await;
                let run = Run {
                    file,
                    connections,
                    slots,
                    outcome,
                };
                eprintln!("round {round}: {}", row(&run));
                runs.push(run);
            }
        }
    }
    eprintln!(
        "\n| File | Connections | HTTP/2 slots | MB/s | Protocol | Retries | Fetched twice |"
    );
    eprintln!("|---|---|---|---|---|---|---|");
    for run in &runs {
        eprintln!("{}", row(run));
    }
}

fn row(r: &Run) -> String {
    let slots = match r.slots {
        0 => "off".to_string(),
        n => n.to_string(),
    };
    match &r.outcome {
        Ok(m) => format!(
            "| {} | {} | {} | {:.1} | {} | {} | {:.1} MB |",
            r.file,
            r.connections,
            slots,
            m.mb_s,
            match m.protocol {
                Some(2) => "HTTP/2",
                Some(_) => "HTTP/1.1",
                None => "?",
            },
            m.retries,
            m.wasted as f64 / 1e6
        ),
        Err(e) => format!(
            "| {} | {} | {} | failed: {e} | | | |",
            r.file, r.connections, slots
        ),
    }
}
