//! The FFmpeg K-Star runs to combine tracks: finding what it can do, and running it on an
//! argument vector (never a shell). It is a separate program; K-Star only talks to it
//! through its command line and its `-progress` output.

use serde::Serialize;
use std::collections::HashSet;
use std::path::Path;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio_util::sync::CancellationToken;

use crate::error::MediaError;
use crate::{Error, Result};

/// What K-Star found out about its FFmpeg.
#[derive(Debug, Clone, Serialize, Default, PartialEq, Eq)]
pub struct FfmpegStatus {
    pub path: Option<String>,
    pub version: Option<String>,
    /// Everything K-Star needs is there.
    pub ok: bool,
    /// What is missing, for the Settings screen.
    pub missing: Vec<String>,
}

const MUXERS: &[&str] = &["mp4", "matroska", "mpegts", "webvtt"];
const DEMUXERS: &[&str] = &[
    "mpegts", "mov", "matroska", "aac", "mp3", "webvtt", "concat",
];
const PROTOCOLS: &[&str] = &["file", "pipe"];
const BSFS: &[&str] = &["aac_adtstoasc"];
/// MP4 holds subtitles as `mov_text`, which is an encoder even under stream copy.
const ENCODERS: &[&str] = &["mov_text"];

/// Runs `ffmpeg -version` and each capability list, and checks them against what K-Star
/// needs. FFmpeg answers one list option per run (the others are ignored), so each is asked
/// for separately.
pub(crate) fn probe(path: &Path) -> FfmpegStatus {
    let shown = Some(path.display().to_string());
    let mut text = String::new();
    for option in [
        "-version",
        "-muxers",
        "-demuxers",
        "-protocols",
        "-bsfs",
        "-encoders",
    ] {
        let out = crate::host::command(path)
            .args(["-hide_banner", option])
            .stdin(Stdio::null())
            .output();
        match out {
            Ok(o) if o.status.success() => {
                text.push_str(&String::from_utf8_lossy(&o.stdout));
                text.push('\n');
            }
            _ => {
                return FfmpegStatus {
                    path: shown,
                    version: None,
                    ok: false,
                    missing: vec!["it didn't run".into()],
                };
            }
        }
    }
    parse(&text, shown)
}

/// Reads the output of the list options. Muxer and demuxer lines carry flag columns (`D` for
/// demuxing at column 1, `E` for muxing at column 2); encoder lines start with six flag
/// characters; protocols and filters are plain names.
pub(crate) fn parse(text: &str, path: Option<String>) -> FfmpegStatus {
    let version = text
        .lines()
        .next()
        .and_then(|l| l.strip_prefix("ffmpeg version "))
        .and_then(|v| v.split_whitespace().next())
        .map(str::to_string);
    let mut muxers = HashSet::new();
    let mut demuxers = HashSet::new();
    let mut protocols = HashSet::new();
    let mut bsfs = HashSet::new();
    let mut encoders = HashSet::new();
    let mut section = "";
    for line in text.lines() {
        let t = line.trim();
        if t == "Formats:" || t.starts_with("File formats:") {
            section = "formats";
            continue;
        }
        if t.starts_with("Supported file protocols:") {
            section = "protocols";
            continue;
        }
        if t.starts_with("Bitstream filters:") {
            section = "bsfs";
            continue;
        }
        if t == "Encoders:" {
            section = "encoders";
            continue;
        }
        if t.is_empty() || t.ends_with(':') && !t.contains(' ') {
            continue;
        }
        match section {
            "formats" => {
                // Columns: a space, then D (demuxing) and E (muxing), then the name at 4.
                let flags: Vec<char> = line.chars().take(3).collect();
                let Some(names) = line
                    .get(4..)
                    .and_then(|rest| rest.split_whitespace().next())
                else {
                    continue;
                };
                for name in names.split(',') {
                    if flags.get(1) == Some(&'D') {
                        demuxers.insert(name.to_string());
                    }
                    if flags.get(2) == Some(&'E') {
                        muxers.insert(name.to_string());
                    }
                }
            }
            "protocols" => {
                if t != "Input:" && t != "Output:" {
                    protocols.extend(t.split_whitespace().map(str::to_string));
                }
            }
            "bsfs" => bsfs.extend(t.split_whitespace().map(str::to_string)),
            "encoders" => {
                let mut cols = t.split_whitespace();
                if let (Some(flags), Some(name)) = (cols.next(), cols.next())
                    && flags.len() == 6
                    && !flags.contains('=')
                {
                    encoders.insert(name.to_string());
                }
            }
            _ => {}
        }
    }
    let mut missing = Vec::new();
    for (names, have, what) in [
        (MUXERS, &muxers, "muxer"),
        (DEMUXERS, &demuxers, "demuxer"),
        (PROTOCOLS, &protocols, "protocol"),
        (BSFS, &bsfs, "bitstream filter"),
        (ENCODERS, &encoders, "encoder"),
    ] {
        for n in names {
            if !have.contains(*n) {
                missing.push(format!("{what} {n}"));
            }
        }
    }
    FfmpegStatus {
        path,
        version,
        ok: missing.is_empty(),
        missing,
    }
}

/// Runs FFmpeg with `args` and reports progress as a permille of `total_us`. Returns
/// `Ok(false)` when stopped through `cancel` (the process is killed).
pub(crate) async fn run(
    path: &Path,
    args: &[String],
    cancel: &CancellationToken,
    total_us: u64,
    progress: impl Fn(u32) + Send + 'static,
) -> Result<bool> {
    let mut child = tokio::process::Command::from(crate::host::command(path))
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| Error::Media(MediaError::FfmpegFailed(e.to_string())))?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let reader = tokio::spawn(async move {
        let Some(out) = stdout else { return };
        let mut lines = BufReader::new(out).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if let Some(us) = line
                .strip_prefix("out_time_us=")
                .and_then(|v| v.trim().parse::<u64>().ok())
                && let Some(permille) = (us.min(total_us) * 1000).checked_div(total_us)
            {
                progress(permille as u32);
            }
        }
    });
    // Keeps the last 64 KiB of the error output, reading all of it so FFmpeg never blocks
    // on a full pipe.
    let err_reader = tokio::spawn(async move {
        let mut kept = Vec::new();
        let Some(mut e) = stderr else { return kept };
        let mut chunk = [0u8; 8192];
        while let Ok(n) = e.read(&mut chunk).await {
            if n == 0 {
                break;
            }
            kept.extend_from_slice(&chunk[..n]);
            if kept.len() > 64 * 1024 {
                kept.drain(..kept.len() - 64 * 1024);
            }
        }
        kept
    });
    let status = tokio::select! {
        s = child.wait() => s.map_err(|e| Error::Media(MediaError::FfmpegFailed(e.to_string())))?,
        _ = cancel.cancelled() => {
            let _ = child.start_kill();
            let _ = child.wait().await;
            reader.abort();
            return Ok(false);
        }
    };
    let _ = reader.await;
    let stderr_text = err_reader.await.unwrap_or_default();
    if status.success() {
        Ok(true)
    } else {
        let text = String::from_utf8_lossy(&stderr_text);
        let last = text
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("it exited with an error");
        Err(Error::Media(MediaError::FfmpegFailed(
            last.trim().chars().take(300).collect(),
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "ffmpeg version 9.0.2 Copyright (c) 2000-2026\n\
        File formats:\n\
         D. = Demuxing supported\n\
         .E = Muxing supported\n\
        \x20 E  mp4             MP4 (MPEG-4 Part 14)\n\
        \x20 E  matroska        Matroska\n\
        \x20 E  mpegts          MPEG-TS\n\
        \x20 E  webvtt          WebVTT subtitle\n\
        \x20D  mov,mp4,m4a,3gp MOV\n\
        \x20D  matroska,webm   Matroska\n\
        \x20D  mpegts          MPEG-TS\n\
        \x20D  webvtt          WebVTT subtitle\n\
        \x20D  aac             raw ADTS AAC\n\
        \x20D  mp3             MP2/3\n\
        \x20D  concat          Virtual concatenation\n\
        Supported file protocols:\n\
        Input:\n\
          file\n\
          pipe\n\
        Output:\n\
          file\n\
        Bitstream filters:\n\
        aac_adtstoasc\n\
        noise\n\
        Encoders:\n\
        \x20V..... = Video\n\
        \x20------\n\
        \x20S..... mov_text             3GPP Timed Text subtitle\n\
        \x20S..... webvtt               WebVTT subtitle\n";

    #[test]
    fn capabilities_are_read_from_the_lists() {
        let s = parse(SAMPLE, Some("/x/ffmpeg".into()));
        assert_eq!(s.version.as_deref(), Some("9.0.2"));
        assert!(s.ok, "{:?}", s.missing);
        assert!(s.missing.is_empty());
    }

    #[test]
    fn a_missing_piece_is_named() {
        let without_pipe = SAMPLE.replace("pipe\n", "");
        let s = parse(&without_pipe, None);
        assert!(!s.ok);
        assert_eq!(s.missing, vec!["protocol pipe".to_string()]);
        let no_bsf = SAMPLE.replace("aac_adtstoasc\n", "");
        assert!(
            parse(&no_bsf, None)
                .missing
                .iter()
                .any(|m| m.contains("aac_adtstoasc"))
        );
        let no_movtext = SAMPLE.replace("mov_text", "other");
        assert_eq!(
            parse(&no_movtext, None).missing,
            vec!["encoder mov_text".to_string()]
        );
    }

    #[tokio::test]
    async fn progress_and_failure_are_reported() {
        let cancel = CancellationToken::new();
        let dir = std::env::temp_dir().join(format!("kstar-ff-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let script = dir.join("fake-ffmpeg.sh");
        std::fs::write(
            &script,
            "#!/bin/sh\necho out_time_us=500000\necho out_time_us=1000000\necho progress=end\necho boom >&2\nexit 3\n",
        )
        .expect("write script");
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let s2 = seen.clone();
        let res = run(&script, &[], &cancel, 1_000_000, move |p| {
            s2.lock().expect("lock").push(p)
        })
        .await;
        assert!(
            matches!(res, Err(Error::Media(MediaError::FfmpegFailed(ref m))) if m == "boom"),
            "{res:?}"
        );
        assert_eq!(*seen.lock().expect("lock"), vec![500, 1000]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
