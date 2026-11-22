//! Recording RTMP and MMS streams. These aren't HTTP, so FFmpeg does the fetching: it
//! copies the stream into a Matroska file until the user presses Finish (FFmpeg is asked to
//! stop with `q`, so the file is closed properly) or the stream ends. The recording is then
//! remuxed into the chosen container and published like any media download.
//!
//! Siler looks the server's name up itself and gives FFmpeg its address: the bundled FFmpeg
//! is a static program that can't use the system's name service. The address is checked
//! like any media address (no private addresses unless allowed). The connection is direct:
//! a proxy in Siler's settings isn't used for these streams.

use reqwest::Url;
use siler_media::select::Output;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::Ordering::Relaxed;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

use super::workdir;
use crate::error::MediaError;
use crate::manager::Inner;
use crate::task::{Outcome, Running, STOP_REMOVE};
use crate::{Error, Result};

/// Schemes recorded through FFmpeg.
pub const STREAM_SCHEMES: &[&str] = &["rtmp", "rtmpt", "mms", "mmsh", "mmst"];

/// An RTMP or MMS address.
pub fn is_stream_url(url: &str) -> bool {
    Url::parse(url.trim()).is_ok_and(|u| {
        STREAM_SCHEMES.contains(&u.scheme()) || matches!(u.scheme(), "rtmps" | "rtmpe")
    })
}

/// Checks a stream address: a scheme FFmpeg can record, and a host.
pub(crate) fn parse_stream_url(url: &str) -> Result<Url> {
    let u = Url::parse(url.trim()).map_err(|e| Error::InvalidUrl(e.to_string()))?;
    match u.scheme() {
        "rtmps" | "rtmpe" | "rtmpte" | "rtmpts" => Err(Error::Media(MediaError::Unsupported(
            "encrypted RTMP (rtmps, rtmpe)".into(),
        ))),
        s if STREAM_SCHEMES.contains(&s) && u.host_str().is_some() => Ok(u),
        s => Err(Error::InvalidUrl(format!("unsupported scheme “{s}”"))),
    }
}

/// The default port of each scheme, for the name lookup.
fn port_of(u: &Url) -> u16 {
    u.port().unwrap_or(match u.scheme() {
        "rtmp" => 1935,
        "rtmpt" | "mmsh" => 80,
        _ => 1755,
    })
}

/// Looks the host up and returns the address FFmpeg should connect to, refusing private
/// addresses unless allowed.
async fn address_of(u: &Url, allow_private: bool) -> Result<String> {
    let host = u
        .host_str()
        .ok_or_else(|| Error::InvalidUrl("no server name".into()))?
        .trim_matches(['[', ']'])
        .to_string();
    let ip = match host.parse::<std::net::IpAddr>() {
        Ok(ip) => ip,
        Err(_) => tokio::net::lookup_host((host.as_str(), port_of(u)))
            .await
            .map_err(|e| Error::Network(format!("{host} can't be found: {e}")))?
            .map(|a| a.ip())
            .find(|ip| ip.is_ipv4())
            .ok_or_else(|| Error::Network(format!("{host} has no IPv4 address")))?,
    };
    if !allow_private && siler_media::policy::is_private(ip) {
        return Err(Error::Media(MediaError::UrlForbidden(format!(
            "{host} is on this computer or the local network"
        ))));
    }
    Ok(match ip {
        std::net::IpAddr::V6(v6) => format!("[{v6}]"),
        v4 => v4.to_string(),
    })
}

/// The URLs FFmpeg tries, in order: `mms://` is MMS over HTTP first, then over TCP.
fn attempts(u: &Url) -> Vec<Url> {
    if u.scheme() != "mms" {
        return vec![u.clone()];
    }
    ["mmsh", "mmst"]
        .iter()
        .filter_map(|s| Url::parse(&format!("{s}{}", &u.as_str()[3..])).ok())
        .collect()
}

/// The FFmpeg command line that records `url` (already pointing at an address) into `out`.
pub(crate) fn record_args(url: &Url, original: &Url, out: &Path) -> Vec<String> {
    let mut a: Vec<String> = [
        "-hide_banner",
        "-nostats",
        "-loglevel",
        "error",
        "-progress",
        "pipe:1",
        "-protocol_whitelist",
        "tcp,http,rtmp,rtmpt,mmsh,mmst,file",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    if url.scheme().starts_with("rtmp") {
        // The server sees the name it was given, not the address FFmpeg connects to.
        a.push("-rtmp_tcurl".into());
        let mut tc = original.clone();
        tc.set_query(None);
        a.push(tc.to_string());
        a.push("-rtmp_live".into());
        a.push("any".into());
    }
    a.extend([
        "-i".into(),
        url.to_string(),
        "-map".into(),
        "0".into(),
        "-c".into(),
        "copy".into(),
        "-f".into(),
        "matroska".into(),
        "-y".into(),
        format!("file:{}", out.display()),
    ]);
    a
}

/// Replaces the host of `u` with `addr`.
fn with_address(u: &Url, addr: &str) -> Url {
    let mut v = u.clone();
    let _ = v.set_host(Some(addr.trim_matches(['[', ']'])));
    v
}

/// Runs FFmpeg until the stream ends, Finish, or a stop. Returns FFmpeg's last error output
/// when it failed.
async fn capture(
    ff: &Path,
    args: &[String],
    run: &Arc<Running>,
) -> Result<std::result::Result<(), String>> {
    let mut child = tokio::process::Command::from(crate::host::command(ff))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| Error::Media(MediaError::FfmpegFailed(e.to_string())))?;
    let mut stdin = child.stdin.take();
    let stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    let r = run.clone();
    let reader = tokio::spawn(async move {
        let Some(out) = stdout else { return };
        let mut lines = BufReader::new(out).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if let Some(us) = line
                .strip_prefix("out_time_us=")
                .and_then(|v| v.trim().parse::<i64>().ok())
            {
                let secs = us.max(0) as f64 / 1e6;
                super::run::set_media(&r, |m| m.recorded_secs = Some(secs));
            } else if let Some(n) = line
                .strip_prefix("total_size=")
                .and_then(|v| v.trim().parse::<u64>().ok())
            {
                r.downloaded.store(n, Relaxed);
            }
        }
    });
    let err_reader = tokio::spawn(async move {
        let mut buf = Vec::new();
        if let Some(e) = stderr.as_mut() {
            let _ = e.read_to_end(&mut buf).await;
        }
        let text = String::from_utf8_lossy(&buf).to_string();
        let keep = text.len().saturating_sub(4096);
        text[text.ceil_char_boundary(keep)..].trim().to_string()
    });
    let stopped = tokio::select! {
        s = child.wait() => {
            s.map_err(|e| Error::Media(MediaError::FfmpegFailed(e.to_string())))?;
            false
        }
        _ = run.finish.cancelled() => true,
        _ = run.cancel.cancelled() => true,
    };
    let status = if stopped {
        if run.stop_reason.load(Relaxed) == STOP_REMOVE {
            let _ = child.kill().await;
            reader.abort();
            return Ok(Ok(()));
        }
        // Ask FFmpeg to stop: it closes the file properly.
        if let Some(i) = stdin.as_mut() {
            let _ = i.write_all(b"q").await;
            let _ = i.flush().await;
        }
        drop(stdin.take());
        match tokio::time::timeout(Duration::from_secs(15), child.wait()).await {
            Ok(s) => s.ok(),
            Err(_) => {
                let _ = child.kill().await;
                None
            }
        }
    } else {
        child.wait().await.ok()
    };
    let _ = reader.await;
    let err = err_reader.await.unwrap_or_default();
    Ok(match status {
        Some(s) if s.success() => Ok(()),
        // Stopped on request: FFmpeg's exit code after `q` isn't always 0.
        _ if stopped => Ok(()),
        _ => Err(if err.is_empty() {
            "FFmpeg stopped".into()
        } else {
            err
        }),
    })
}

/// Records an RTMP or MMS stream, then rebuilds and publishes it.
pub(crate) async fn drive(inner: &Arc<Inner>, id: i64, run: &Arc<Running>) -> Result<Outcome> {
    let rec = inner
        .state()
        .records
        .get(&id)
        .cloned()
        .ok_or(Error::NotFound)?;
    let mr = rec
        .media
        .clone()
        .ok_or_else(|| Error::Protocol("This download has no stream details.".into()))?;
    let url = parse_stream_url(&rec.url)?;
    let dir = rec
        .dir
        .clone()
        .ok_or_else(|| Error::Protocol("This download has no folder.".into()))?;
    let name = rec
        .filename
        .clone()
        .ok_or_else(|| Error::Protocol("This download has no name.".into()))?;
    let work = workdir(&dir, &name);
    std::fs::create_dir_all(&work)?;
    *run.part.lock().expect("part lock") = Some(work.clone());
    *run.media.lock().expect("media lock") = Some(super::MediaProgress {
        phase: "recording",
        recorded_secs: Some(0.0),
        ..Default::default()
    });
    let ff = inner
        .ffmpeg()
        .ok_or(Error::Media(MediaError::FfmpegMissing))?;
    if !inner.ffmpeg_status.lock().expect("status lock").network {
        return Err(Error::Media(MediaError::Unsupported(
            "RTMP and MMS recording with this FFmpeg (it has no network support)".into(),
        )));
    }
    let capture_file = work.join("rec.mkv");
    // A recording can't continue where it stopped: one that has data was finished already.
    let have = std::fs::metadata(&capture_file)
        .map(|m| m.len())
        .unwrap_or(0);
    if have == 0 && !mr.finish {
        {
            let mut st = inner.state();
            if let Some(r) = st.records.get_mut(&id) {
                r.status = crate::Status::Downloading;
                r.error = None;
                inner.persist_and_emit(r, Some(run))?;
            }
        }
        let s = inner.settings();
        let addr = address_of(&url, s.allow_private_media_addresses).await?;
        let mut last_err = String::new();
        for target in attempts(&url) {
            let args = record_args(&with_address(&target, &addr), &target, &capture_file);
            match capture(&ff, &args, run).await? {
                Ok(()) => {
                    last_err.clear();
                    break;
                }
                Err(e) => {
                    tracing::info!("recording {target} failed: {e}");
                    last_err = e;
                }
            }
        }
        if run.stop_reason.load(Relaxed) == STOP_REMOVE {
            return Ok(Outcome::Stopped);
        }
        let got = std::fs::metadata(&capture_file)
            .map(|m| m.len())
            .unwrap_or(0);
        if got == 0 {
            let _ = std::fs::remove_file(&capture_file);
            return Err(Error::Media(MediaError::FfmpegFailed(
                if last_err.is_empty() {
                    "nothing was recorded".into()
                } else {
                    last_err
                },
            )));
        }
    }
    {
        let mut st = inner.state();
        if let Some(r) = st.records.get_mut(&id)
            && let Some(m) = r.media.as_mut()
            && !m.finish
        {
            m.finish = true;
            inner.persist_and_emit(r, Some(run))?;
        }
    }
    // Into the chosen container. Matroska takes anything; MP4 may refuse old codecs (WMV), in
    // which case the Matroska recording is kept.
    super::run::set_media(run, |m| m.phase = "rebuilding");
    let out = match mr.output {
        Output::Mp4 => {
            let mp4 = work.join("out.mp4");
            let args: Vec<String> = [
                "-nostdin",
                "-hide_banner",
                "-nostats",
                "-loglevel",
                "error",
                "-progress",
                "pipe:1",
                "-protocol_whitelist",
                "file",
            ]
            .iter()
            .map(|s| s.to_string())
            .chain([
                "-i".to_string(),
                format!("file:{}", capture_file.display()),
                "-map".into(),
                "0".into(),
                "-c".into(),
                "copy".into(),
                "-movflags".into(),
                "+faststart".into(),
                "-f".into(),
                "mp4".into(),
                "-y".into(),
                format!("file:{}", mp4.display()),
            ])
            .collect();
            let done = tokio_util::sync::CancellationToken::new();
            match super::ffmpeg::run(&ff, &args, &done, 0, |_| {}).await {
                Ok(true) => mp4,
                other => {
                    tracing::info!("the recording stays Matroska: {other:?}");
                    let _ = std::fs::remove_file(&mp4);
                    capture_file.clone()
                }
            }
        }
        _ => capture_file.clone(),
    };
    super::run::publish(inner, id, run, out, Vec::new(), &dir, &name, &work)
}

/// Where a stream download keeps its recording (for tests).
#[allow(dead_code)]
pub(crate) fn capture_path(work: &Path) -> PathBuf {
    work.join("rec.mkv")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_and_arguments() {
        assert!(is_stream_url("rtmp://live.example/app/key"));
        assert!(is_stream_url("mms://wm.example/radio"));
        assert!(!is_stream_url("https://a.example/x.mp4"));
        assert!(parse_stream_url("rtmps://x/app").is_err());
        let u = Url::parse("mms://wm.example/radio").unwrap();
        let a: Vec<String> = attempts(&u).iter().map(|u| u.to_string()).collect();
        assert_eq!(a, ["mmsh://wm.example/radio", "mmst://wm.example/radio"]);
        let r = Url::parse("rtmp://live.example/app/key?t=1").unwrap();
        let args = record_args(
            &with_address(&r, "203.0.113.5"),
            &r,
            Path::new("/w/rec.mkv"),
        );
        let i = args.iter().position(|a| a == "-i").unwrap();
        assert_eq!(args[i + 1], "rtmp://203.0.113.5/app/key?t=1");
        let t = args.iter().position(|a| a == "-rtmp_tcurl").unwrap();
        assert_eq!(args[t + 1], "rtmp://live.example/app/key");
        assert_eq!(args.last().unwrap(), "file:/w/rec.mkv");
    }
}
