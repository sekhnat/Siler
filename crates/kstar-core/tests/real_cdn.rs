//! Media downloads from real CDNs: public test streams, so parsers and the engine meet what
//! packagers and servers actually send. Needs the network, so it is ignored by default:
//!
//! ```sh
//! KSTAR_REAL_CDN=1 KSTAR_FFMPEG=/path/to/ffmpeg cargo test -p kstar-core --test real_cdn -- --ignored --nocapture
//! ```
//!
//! For each stream: probe, choose the lowest video quality with its default audio and one
//! subtitle when there is one, download to MKV, and check the file with `ffprobe` (stream
//! kinds and duration). Streams over the size budget are probed only. A stream that can't be
//! reached is reported as down, not failed. No user data is sent; requests carry K-Star's
//! default User-Agent and nothing else.

use kstar_core::{
    ContainerChoice, Engine, FfmpegStatus, MediaKind, MediaProbe, NewMediaDownload, RequestMeta,
    Selection, Settings, Status, SubtitleMode,
};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

/// Downloads larger than this (by the manifest's estimate) are probed only.
const BUDGET: u64 = 60 * 1024 * 1024;

/// The streams, and what each covers.
const STREAMS: &[(&str, &str)] = &[
    (
        "HLS fMP4, separate audio renditions, WebVTT subtitles (Apple)",
        "https://devstreaming-cdn.apple.com/videos/streaming/examples/img_bipbop_adv_example_fmp4/master.m3u8",
    ),
    (
        "HLS MPEG-TS, separate audio, WebVTT subtitles (Apple)",
        "https://devstreaming-cdn.apple.com/videos/streaming/examples/img_bipbop_adv_example_ts/master.m3u8",
    ),
    (
        "HLS MPEG-TS, muxed audio, signed-style query on every URI, 30 s clip (Unified Streaming)",
        "https://demo.unified-streaming.com/k8s/features/stable/video/tears-of-steel/tears-of-steel.ism/.m3u8?vbegin=0&vend=30",
    ),
    (
        "DASH SegmentTimeline with $Time$, relative BaseURL, 30 s clip (Unified Streaming)",
        "https://demo.unified-streaming.com/k8s/features/stable/video/tears-of-steel/tears-of-steel.ism/.mpd?vbegin=0&vend=30",
    ),
    (
        "DASH, TTML subtitles inside MP4 (stpp), a clip from 60 s with a presentation offset (Unified Streaming)",
        "https://demo.unified-streaming.com/k8s/features/stable/video/tears-of-steel/tears-of-steel-ttml.ism/.mpd?vbegin=60&vend=90",
    ),
    (
        "DASH, WebVTT subtitles inside MP4 (wvtt), a clip from 60 s (Unified Streaming)",
        "https://demo.unified-streaming.com/k8s/features/stable/video/tears-of-steel/tears-of-steel-wvtt.ism/.mpd?vbegin=60&vend=90",
    ),
    (
        "HLS MPEG-TS, WebVTT segments whose X-TIMESTAMP-MAP isn't the video's start, a clip from 60 s (Unified Streaming)",
        "https://demo.unified-streaming.com/k8s/features/stable/video/tears-of-steel/tears-of-steel-wvtt.ism/.m3u8?vbegin=60&vend=90",
    ),
    (
        "HLS MPEG-TS with AES-128 keys (JW Player test stream)",
        "https://playertest.longtailvideo.com/adaptive/oceans_aes/oceans_aes.m3u8",
    ),
    (
        "DASH SegmentTemplate with $Number$ (Akamai)",
        "https://dash.akamaized.net/akamai/bbb_30fps/bbb_30fps.mpd",
    ),
    (
        "DASH, several audio languages, WebVTT inside MP4 as whole files (Shaka demo assets)",
        "https://storage.googleapis.com/shaka-demo-assets/angel-one/dash.mpd",
    ),
    (
        "HLS, several audio languages and subtitles (Shaka demo assets)",
        "https://storage.googleapis.com/shaka-demo-assets/angel-one-hls/hls.m3u8",
    ),
    (
        "HLS MPEG-TS, muxed audio (Mux test streams)",
        "https://test-streams.mux.dev/x36xhzz/x36xhzz.m3u8",
    ),
];

fn ffmpeg() -> PathBuf {
    std::env::var_os("KSTAR_FFMPEG")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("ffmpeg"))
}

fn ffprobe(ff: &Path) -> PathBuf {
    if ff.parent().is_some_and(|p| !p.as_os_str().is_empty()) {
        ff.with_file_name("ffprobe")
    } else {
        PathBuf::from("ffprobe")
    }
}

/// The kind of each stream and its duration in seconds (Matroska keeps one per stream; the
/// file's own duration is the longest of them, a subtitle cue past the end included).
fn inspect(ff: &Path, file: &Path) -> Vec<(String, f64)> {
    let out = Command::new(ffprobe(ff))
        .args([
            "-v",
            "error",
            "-show_entries",
            "stream=codec_type:stream_tags=DURATION",
        ])
        .args(["-of", "csv=p=0"])
        .arg(file)
        .output()
        .expect("ffprobe runs");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            let (kind, dur) = l.split_once(',').unwrap_or((l, ""));
            let secs = dur
                .split(':')
                .filter_map(|x| x.parse::<f64>().ok())
                .fold(0.0, |acc, x| acc * 60.0 + x);
            (!kind.is_empty()).then(|| (kind.to_string(), secs))
        })
        .collect()
}

/// The lowest video quality, its default (or first) audio, and its first subtitle.
fn smallest(p: &MediaProbe) -> (Selection, Option<u64>) {
    let Some(v) = p.variants.iter().min_by_key(|v| {
        (
            v.video.bandwidth.unwrap_or(u64::MAX),
            v.video.resolution.map_or(u32::MAX, |r| r.0 * r.1),
        )
    }) else {
        // Audio only.
        let a = p
            .audio
            .iter()
            .min_by_key(|a| a.bandwidth.unwrap_or(u64::MAX));
        return (
            Selection {
                video: None,
                audio: a.map(|a| vec![a.id.clone()]).unwrap_or_default(),
                subtitles: Vec::new(),
            },
            a.and_then(|a| a.estimated_bytes),
        );
    };
    let audio: Vec<_> = p.audio.iter().filter(|a| v.audio.contains(&a.id)).collect();
    let a = audio
        .iter()
        .find(|a| a.is_default)
        .or_else(|| audio.iter().min_by_key(|a| a.bandwidth.unwrap_or(u64::MAX)))
        .copied();
    let s = p.subtitles.iter().find(|s| v.subtitles.contains(&s.id));
    let sizes = [v.video.estimated_bytes, a.and_then(|a| a.estimated_bytes)];
    let estimate = sizes.iter().flatten().sum::<u64>();
    (
        Selection {
            video: Some(v.video.id.clone()),
            audio: a.map(|a| vec![a.id.clone()]).unwrap_or_default(),
            subtitles: s.map(|s| vec![s.id.clone()]).unwrap_or_default(),
        },
        (estimate > 0).then_some(estimate),
    )
}

enum Verdict {
    Ok(String),
    ProbedOnly(String),
    Down(String),
    Failed(String),
}

impl std::fmt::Display for Verdict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Verdict::Ok(m) => write!(f, "ok: {m}"),
            Verdict::ProbedOnly(m) => write!(f, "probed only: {m}"),
            Verdict::Down(m) => write!(f, "down: {m}"),
            Verdict::Failed(m) => write!(f, "FAILED: {m}"),
        }
    }
}

async fn one(e: &Engine, ff: &Path, out: &Path, name: &str, url: &str) -> Verdict {
    let p = match e.probe_media(url, &RequestMeta::default()).await {
        Ok(p) => p,
        Err(err) if err.is_transient() => return Verdict::Down(err.to_string()),
        Err(err) => return Verdict::Failed(format!("probe: {err}")),
    };
    println!(
        "  probe: {:?}, {:.1} s, {} variants, {} audio, {} subtitles, warnings {:?}",
        p.kind,
        p.duration.unwrap_or(0.0),
        p.variants.len(),
        p.audio.len(),
        p.subtitles.len(),
        p.warnings
    );
    if !matches!(p.kind, MediaKind::Hls | MediaKind::Dash) {
        return Verdict::Failed(format!("probe: {:?} {:?}", p.kind, p.message));
    }
    let (sel, estimate) = smallest(&p);
    let mb = |b: u64| b as f64 / 1048576.0;
    if estimate.is_some_and(|b| b > BUDGET) {
        return Verdict::ProbedOnly(format!(
            "about {:.0} MB at the lowest quality",
            mb(estimate.unwrap_or(0))
        ));
    }
    println!(
        "  downloading: video {}, {} audio, {} subtitles, about {:.1} MB",
        sel.video.is_some(),
        sel.audio.len(),
        sel.subtitles.len(),
        mb(estimate.unwrap_or(0))
    );
    let want_sub = !sel.subtitles.is_empty();
    let want_video = sel.video.is_some();
    let id = match e
        .add_media(NewMediaDownload {
            url: url.to_string(),
            dir: Some(out.to_path_buf()),
            filename: Some(name.to_string()),
            selection: Some(sel),
            container: Some(ContainerChoice::Mkv),
            subtitles: Some(SubtitleMode::Embed),
            ..Default::default()
        })
        .await
    {
        Ok(id) => id,
        Err(err) => return Verdict::Failed(format!("add: {err}")),
    };
    let started = Instant::now();
    let d = loop {
        let d = e.get(id).expect("exists");
        if matches!(
            d.status,
            Status::Completed | Status::Failed | Status::Paused
        ) {
            break d;
        }
        if started.elapsed() > Duration::from_secs(900) {
            return Verdict::Failed(format!("timed out at {:?}", d.status));
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    };
    if d.status != Status::Completed {
        return Verdict::Failed(format!("{:?}: {:?}", d.status, d.error));
    }
    let file = d
        .dir
        .clone()
        .expect("dir")
        .join(d.filename.clone().expect("name"));
    let size = std::fs::metadata(&file).map(|m| m.len()).unwrap_or(0);
    let streams = inspect(ff, &file);
    let kinds: Vec<&str> = streams.iter().map(|(k, _)| k.as_str()).collect();
    // The video's length (or the audio's, for an audio-only stream) against the manifest.
    let main = if want_video { "video" } else { "audio" };
    let dur = streams
        .iter()
        .filter(|(k, _)| k == main)
        .map(|(_, d)| *d)
        .fold(0.0, f64::max);
    let mut problems = Vec::new();
    if want_video && !kinds.contains(&"video") {
        problems.push("no video stream".to_string());
    }
    if !kinds.contains(&"audio") {
        problems.push("no audio stream".to_string());
    }
    if want_sub && !kinds.contains(&"subtitle") {
        problems.push("no subtitle stream".to_string());
    }
    // No stream (a subtitle cue past the end) makes the file longer than its picture and
    // sound.
    let media_end = streams
        .iter()
        .filter(|(k, _)| k == "video" || k == "audio")
        .map(|(_, d)| *d)
        .fold(0.0, f64::max);
    let file_end = streams.iter().map(|(_, d)| *d).fold(0.0, f64::max);
    if file_end > media_end + 0.1 {
        problems.push(format!(
            "a stream runs to {file_end:.2} s, past the picture and sound ({media_end:.2} s)"
        ));
    }
    if want_sub && !streams.iter().any(|(k, d)| k == "subtitle" && *d > 0.0) {
        problems.push("the subtitle stream is empty".to_string());
    }
    if let Some(expected) = p.duration
        && (dur - expected).abs() > (expected * 0.02).max(1.0)
    {
        problems.push(format!(
            "duration {dur:.2} s, manifest says {expected:.2} s"
        ));
    }
    let summary = format!(
        "{} in {:.0} s: {:?}, {dur:.1} s, {:.1} MB",
        file.file_name().unwrap_or_default().to_string_lossy(),
        started.elapsed().as_secs_f64(),
        kinds,
        mb(size)
    );
    if problems.is_empty() {
        Verdict::Ok(summary)
    } else {
        Verdict::Failed(format!("{summary}; {}", problems.join("; ")))
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the network: KSTAR_REAL_CDN=1 … -- --ignored"]
async fn real_cdn_streams_download_and_play() {
    if std::env::var("KSTAR_REAL_CDN").as_deref() != Ok("1") {
        eprintln!("skipped: set KSTAR_REAL_CDN=1");
        return;
    }
    let ff = ffmpeg();
    let tmp = tempfile::tempdir().expect("tempdir");
    let e = Engine::open(tmp.path()).expect("open");
    let status: FfmpegStatus = e.set_ffmpeg_path(Some(ff.clone()));
    assert!(status.ok, "FFmpeg check failed: {:?}", status.missing);
    e.set_settings(Settings {
        download_dir: tmp.path().join("downloads"),
        ..e.settings()
    })
    .expect("settings");
    let only = std::env::var("KSTAR_REAL_CDN_ONLY").ok();
    let mut results = Vec::new();
    for (i, (what, url)) in STREAMS.iter().enumerate() {
        if only
            .as_deref()
            .is_some_and(|o| !what.contains(o) && !url.contains(o))
        {
            continue;
        }
        println!("[{}] {what}\n  {url}", i + 1);
        let v = one(
            &e,
            &ff,
            &tmp.path().join("out"),
            &format!("cdn{}", i + 1),
            url,
        )
        .await;
        println!("  => {v}");
        results.push((what, v));
    }
    e.shutdown().await;
    println!("\nSummary:");
    for (what, v) in &results {
        println!("  {v} — {what}");
    }
    let failed: Vec<_> = results
        .iter()
        .filter(|(_, v)| matches!(v, Verdict::Failed(_)))
        .collect();
    assert!(failed.is_empty(), "{} stream(s) failed", failed.len());
}
