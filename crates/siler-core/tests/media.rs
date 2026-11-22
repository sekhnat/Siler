//! End-to-end media tests: a local server serves the committed fixtures (see
//! scripts/gen_media_fixtures.sh), with fault injection, and the engine downloads them. The
//! muxing tests need an FFmpeg: `SILER_FFMPEG` if set, else `ffmpeg` on PATH; they skip
//! without one unless `SILER_REQUIRE_FFMPEG=1`.

use axum::Router;
use axum::body::Body;
use axum::extract::{Path as AxPath, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::Response;
use axum::routing::get;
use siler_core::{
    ContainerChoice, DownloadInfo, Engine, Event, FfmpegStatus, HintSubtitle, InlineManifest,
    MediaError, MediaKind, MediaProbe, NewMediaDownload, RequestMeta, Selection, Settings,
    SourceHint, Status, SubtitleMode,
};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/media")
}

// ------------------------------------------------------------------ server

#[derive(Clone, Copy)]
enum Fault {
    /// Answer 200 with an HTML page (an expired link that returns a login page).
    Html,
    /// Answer 403 (an expired signed URL).
    Forbidden,
    /// Answer 200 with these bytes (a wrong key).
    Bytes(&'static [u8]),
}

#[derive(Default)]
struct Shared {
    hits: Mutex<HashMap<String, usize>>,
    /// Faults for a path: the first `n` requests get the fault.
    faults: Mutex<HashMap<String, (usize, Fault)>>,
    /// A delay before every segment answer.
    delay: Mutex<Option<Duration>>,
    /// Files made by a test, served under `/extra/`.
    extra: Mutex<Option<PathBuf>>,
    /// The `Range` headers asked for, by path.
    ranges: Mutex<Vec<(String, String)>>,
    /// When each path was first asked for.
    first: Mutex<HashMap<String, Instant>>,
}

impl Shared {
    fn hits(&self, path: &str) -> usize {
        self.hits.lock().unwrap().get(path).copied().unwrap_or(0)
    }
}

fn content_type(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("") {
        "m3u8" => "application/vnd.apple.mpegurl",
        "mpd" => "application/dash+xml",
        "ts" => "video/mp2t",
        "m4s" | "mp4" => "video/mp4",
        "vtt" => "text/vtt",
        "html" => "text/html",
        _ => "application/octet-stream",
    }
}

async fn serve(
    State(shared): State<Arc<Shared>>,
    AxPath(path): AxPath<String>,
    headers: axum::http::HeaderMap,
) -> Response {
    let key = format!("/{path}");
    *shared.hits.lock().unwrap().entry(key.clone()).or_default() += 1;
    shared
        .first
        .lock()
        .unwrap()
        .entry(key.clone())
        .or_insert_with(Instant::now);
    let range = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    if let Some(r) = &range {
        shared.ranges.lock().unwrap().push((key.clone(), r.clone()));
    }
    if let Some(rest) = path.strip_prefix("redir/") {
        // A redirect to the same path without the prefix (relative references then resolve
        // against where it led).
        return Response::builder()
            .status(StatusCode::FOUND)
            .header(header::LOCATION, format!("/{rest}"))
            .body(Body::empty())
            .unwrap_or_default();
    }
    if path.ends_with(".ts")
        || path.ends_with(".m4s")
        || (path.starts_with("extra/") && range.is_some())
    {
        let d = *shared.delay.lock().unwrap();
        if let Some(d) = d {
            tokio::time::sleep(d).await;
        }
    }
    {
        let mut faults = shared.faults.lock().unwrap();
        if let Some((n, fault)) = faults.get_mut(&key)
            && *n > 0
        {
            {
                *n -= 1;
                let fault = *fault;
                drop(faults);
                return match fault {
                    Fault::Html => Response::builder()
                        .header(header::CONTENT_TYPE, "text/html")
                        .body(Body::from(
                            "<!DOCTYPE html><html><body>Session expired</body></html>",
                        ))
                        .unwrap_or_default(),
                    Fault::Forbidden => Response::builder()
                        .status(StatusCode::FORBIDDEN)
                        .body(Body::empty())
                        .unwrap_or_default(),
                    Fault::Bytes(b) => Response::new(Body::from(b)),
                };
            }
        }
    }
    let file = match (
        path.strip_prefix("extra/"),
        shared.extra.lock().unwrap().clone(),
    ) {
        (Some(rest), Some(dir)) => dir.join(rest),
        _ => fixtures().join(&path),
    };
    match tokio::fs::read(file).await {
        Ok(bytes) => {
            if let Some((a, b)) = range.as_deref().and_then(parse_range) {
                let len = bytes.len() as u64;
                if a >= len {
                    return Response::builder()
                        .status(StatusCode::RANGE_NOT_SATISFIABLE)
                        .header(header::CONTENT_RANGE, format!("bytes */{len}"))
                        .body(Body::empty())
                        .unwrap_or_default();
                }
                let b = b.unwrap_or(len - 1).min(len - 1);
                return Response::builder()
                    .status(StatusCode::PARTIAL_CONTENT)
                    .header(header::CONTENT_TYPE, content_type(&path))
                    .header(header::CONTENT_RANGE, format!("bytes {a}-{b}/{len}"))
                    .body(Body::from(bytes[a as usize..=b as usize].to_vec()))
                    .unwrap_or_default();
            }
            let mut resp = Response::new(Body::from(bytes));
            resp.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static(content_type(&path)),
            );
            resp
        }
        Err(_) => Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::empty())
            .unwrap_or_default(),
    }
}

/// `bytes=a-b` or `bytes=a-`.
fn parse_range(r: &str) -> Option<(u64, Option<u64>)> {
    let (a, b) = r.strip_prefix("bytes=")?.split_once('-')?;
    Some((a.parse().ok()?, b.parse().ok()))
}

async fn spawn_server() -> (SocketAddr, Arc<Shared>) {
    let shared = Arc::new(Shared::default());
    let app = Router::new()
        .route("/{*path}", get(serve))
        .with_state(shared.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (addr, shared)
}

// ------------------------------------------------------------------ helpers

/// The FFmpeg to use, if there is one.
fn ffmpeg() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("SILER_FFMPEG") {
        return Some(PathBuf::from(p));
    }
    let out = Command::new("ffmpeg").arg("-version").output().ok()?;
    out.status.success().then(|| PathBuf::from("ffmpeg"))
}

/// Skips the test when there is no FFmpeg (or fails when it is required).
macro_rules! need_ffmpeg {
    () => {
        match ffmpeg() {
            Some(p) => p,
            None => {
                if std::env::var("SILER_REQUIRE_FFMPEG").as_deref() == Ok("1") {
                    panic!("SILER_REQUIRE_FFMPEG=1 but no FFmpeg was found");
                }
                eprintln!("skipped: no FFmpeg");
                return;
            }
        }
    };
}

/// An engine without FFmpeg (probes, and single-track outputs).
async fn engine_plain(dir: &Path) -> Engine {
    let e = Engine::open(dir).expect("open");
    e.set_settings(Settings {
        download_dir: dir.join("downloads"),
        ..e.settings()
    })
    .expect("settings");
    e
}

async fn engine(dir: &Path, ff: &Path) -> Engine {
    let e = Engine::open(dir).expect("open");
    let status: FfmpegStatus = e.set_ffmpeg_path(Some(ff.to_path_buf()));
    assert!(status.ok, "FFmpeg check failed: {:?}", status.missing);
    e.set_settings(Settings {
        download_dir: dir.join("downloads"),
        ..e.settings()
    })
    .expect("settings");
    e
}

fn url(addr: SocketAddr, path: &str) -> String {
    format!("http://{addr}/{path}")
}

async fn wait_done(e: &Engine, id: i64) -> DownloadInfo {
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        let d = e.get(id).expect("exists");
        if matches!(
            d.status,
            Status::Completed | Status::Failed | Status::Paused
        ) {
            return d;
        }
        assert!(
            Instant::now() < deadline,
            "timed out: {:?} {:?}",
            d.status,
            d.error
        );
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
}

fn output_of(d: &DownloadInfo) -> PathBuf {
    d.dir
        .clone()
        .expect("dir")
        .join(d.filename.clone().expect("name"))
}

fn ffprobe_of(ff: &Path) -> PathBuf {
    if ff.as_os_str() == "ffmpeg" {
        PathBuf::from("ffprobe")
    } else {
        ff.with_file_name("ffprobe")
    }
}

/// The streams of a file: (codec_type, codec_name, language).
fn streams(ff: &Path, file: &Path) -> Vec<(String, String, Option<String>)> {
    let out = Command::new(ffprobe_of(ff))
        .args([
            "-v",
            "error",
            "-show_entries",
            "stream=codec_type,codec_name:stream_tags=language",
            "-of",
            "json",
        ])
        .arg(file)
        .output()
        .expect("ffprobe runs");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
    v["streams"]
        .as_array()
        .expect("streams")
        .iter()
        .map(|s| {
            (
                s["codec_type"].as_str().unwrap_or("").to_string(),
                s["codec_name"].as_str().unwrap_or("").to_string(),
                s["tags"]["language"].as_str().map(str::to_string),
            )
        })
        .collect()
}

fn duration_of(ff: &Path, file: &Path) -> f64 {
    let out = Command::new(ffprobe_of(ff))
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "csv=p=0",
        ])
        .arg(file)
        .output()
        .expect("ffprobe runs");
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .unwrap_or(0.0)
}

async fn probe(e: &Engine, u: &str) -> MediaProbe {
    e.probe_media(u, &RequestMeta::default(), None)
        .await
        .expect("probe")
}

fn dir() -> tempfile::TempDir {
    tempfile::tempdir().expect("tempdir")
}

// ------------------------------------------------------------------ probing

#[tokio::test]
async fn probe_reads_hls_and_dash_presentations() {
    let (addr, _s) = spawn_server().await;
    let t = dir();
    let e = engine_plain(t.path()).await;

    let p = probe(&e, &url(addr, "split/master.m3u8")).await;
    assert_eq!(p.kind, MediaKind::Hls);
    assert_eq!(p.variants.len(), 1);
    assert_eq!(p.audio.len(), 2);
    assert_eq!(p.subtitles.len(), 1);
    assert!(p.audio[0].is_default, "English is the default audio");
    assert_eq!(p.default.audio, vec![p.audio[0].id.clone()]);
    assert_eq!(p.default.video, Some(p.variants[0].video.id.clone()));
    assert!(p.default.subtitles.is_empty(), "no subtitles by default");
    assert!(!p.ffmpeg, "the probe reports the FFmpeg it was given");

    let d = probe(&e, &url(addr, "dash/manifest.mpd")).await;
    assert_eq!(d.kind, MediaKind::Dash);
    assert_eq!(d.variants.len(), 1);
    assert_eq!(d.audio.len(), 1);
    assert!(d.duration.is_some_and(|x| (x - 6.0).abs() < 0.5));

    let m = probe(&e, &url(addr, "muxed/index.m3u8")).await;
    assert_eq!(m.kind, MediaKind::Hls);
    assert!(
        m.variants[0].muxed_audio,
        "a playlist of muxed segments carries its audio inside"
    );
    e.shutdown().await;
}

#[tokio::test]
async fn drm_html_and_plain_files_are_told_apart() {
    let (addr, _s) = spawn_server().await;
    let t = dir();
    let e = engine_plain(t.path()).await;

    let drm = probe(&e, &url(addr, "drm/manifest.mpd")).await;
    assert_eq!(drm.kind, MediaKind::Drm);
    assert!(drm.message.as_deref().unwrap_or("").contains("DRM"));
    assert!(
        drm.dropped_drm >= 2,
        "every track is protected: {}",
        drm.dropped_drm
    );

    let page = probe(&e, &url(addr, "drm/expired.html")).await;
    assert_eq!(page.kind, MediaKind::NotMedia);
    assert!(page.message.is_some());

    let err = e
        .add_media(NewMediaDownload {
            url: url(addr, "drm/manifest.mpd"),
            dir: Some(t.path().join("out")),
            ..Default::default()
        })
        .await
        .expect_err("DRM is refused");
    assert!(
        matches!(err, siler_core::Error::Media(MediaError::Drm)),
        "{err:?}"
    );
    e.shutdown().await;
}

#[tokio::test]
async fn a_stream_on_this_computer_may_use_this_computer_and_redirects_are_followed() {
    // The fixture server is on 127.0.0.1: a private source may point at private addresses
    // (refusals from public sources are covered by the guard's unit tests). The manifest is
    // reached through a redirect, and its relative references resolve against where it led.
    let (addr, s) = spawn_server().await;
    let t = dir();
    let e = engine_plain(t.path()).await;
    let p = probe(&e, &url(addr, "redir/muxed/index.m3u8")).await;
    assert_eq!(p.kind, MediaKind::Hls);
    assert_eq!(s.hits("/redir/muxed/index.m3u8"), 1);
    assert_eq!(
        s.hits("/muxed/index.m3u8"),
        1,
        "a small manifest is read once, not sniffed and read again"
    );
    let id = e
        .add_media(NewMediaDownload {
            url: url(addr, "redir/aes/index.m3u8"),
            dir: Some(t.path().join("out")),
            container: Some(ContainerChoice::Original),
            filename: Some("moved".into()),
            ..Default::default()
        })
        .await
        .expect("adds");
    let d = wait_done(&e, id).await;
    assert_eq!(d.status, Status::Completed, "{:?}", d.error);
    assert!(
        s.hits("/aes/seg2.ts") >= 1,
        "segments resolve against the redirect"
    );
    e.shutdown().await;
}

// ------------------------------------------------------------------ downloads

#[tokio::test]
async fn muxed_hls_becomes_one_mkv() {
    let ff = need_ffmpeg!();
    let (addr, _s) = spawn_server().await;
    let t = dir();
    let e = engine(t.path(), &ff).await;
    let id = e
        .add_media(NewMediaDownload {
            url: url(addr, "muxed/index.m3u8"),
            dir: Some(t.path().join("out")),
            container: Some(ContainerChoice::Mkv),
            filename: Some("clip".into()),
            ..Default::default()
        })
        .await
        .expect("adds");
    let d = wait_done(&e, id).await;
    assert_eq!(d.status, Status::Completed, "{:?}", d.error);
    let file = output_of(&d);
    assert_eq!(file.extension().and_then(|x| x.to_str()), Some("mkv"));
    let s = streams(&ff, &file);
    assert!(s.iter().any(|x| x.0 == "video"), "{s:?}");
    assert!(s.iter().any(|x| x.0 == "audio"), "{s:?}");
    let len = duration_of(&ff, &file);
    assert!((len - 6.0).abs() < 0.6, "duration {len}");
    // The Work Directory is gone once the output is published.
    assert!(!siler_core_workdir(&d).exists());
    e.shutdown().await;
}

fn siler_core_workdir(d: &DownloadInfo) -> PathBuf {
    let dir = d.dir.clone().expect("dir");
    dir.join(format!(
        ".{}.siler.media",
        d.filename.clone().unwrap_or_default()
    ))
}

#[tokio::test]
async fn aes_128_segments_decrypt_to_the_plain_bytes() {
    let ff = need_ffmpeg!();
    let (addr, _s) = spawn_server().await;
    let t = dir();
    let e = engine(t.path(), &ff).await;
    // Single muxed track, so the original container is available: the output is the joined,
    // decrypted segments, which must equal the plain segments byte for byte.
    let id = e
        .add_media(NewMediaDownload {
            url: url(addr, "aes/index.m3u8"),
            dir: Some(t.path().join("out")),
            container: Some(ContainerChoice::Original),
            filename: Some("secret".into()),
            ..Default::default()
        })
        .await
        .expect("adds");
    let d = wait_done(&e, id).await;
    assert_eq!(d.status, Status::Completed, "{:?}", d.error);
    let got = std::fs::read(output_of(&d)).expect("output");
    let mut want = Vec::new();
    for i in 0..3 {
        want.extend(std::fs::read(fixtures().join(format!("muxed/seg{i}.ts"))).expect("fixture"));
    }
    assert_eq!(got.len(), want.len());
    assert!(
        got == want,
        "decrypted output differs from the plain segments"
    );
    assert_eq!(_s.hits("/aes/key.bin"), 1, "the key is fetched once");
    e.shutdown().await;
}

#[tokio::test]
async fn an_encrypted_init_segment_is_decrypted_with_the_segments() {
    let (addr, s) = spawn_server().await;
    let t = dir();
    let e = engine_plain(t.path()).await;
    // No IV on the key line: the init takes the next segment's sequence number. The original
    // container is the joined, decrypted init and fragments, byte for byte.
    let id = e
        .add_media(NewMediaDownload {
            url: url(addr, "aes-fmp4/seq/index.m3u8"),
            dir: Some(t.path().join("out")),
            container: Some(ContainerChoice::Original),
            filename: Some("seq".into()),
            ..Default::default()
        })
        .await
        .expect("adds");
    let d = wait_done(&e, id).await;
    assert_eq!(d.status, Status::Completed, "{:?}", d.error);
    let mut want = std::fs::read(fixtures().join("fmp4/init.mp4")).expect("fixture");
    for i in 0..3 {
        want.extend(std::fs::read(fixtures().join(format!("fmp4/seg{i}.m4s"))).expect("fixture"));
    }
    assert!(
        std::fs::read(output_of(&d)).expect("output") == want,
        "decrypted output differs from the plain init and fragments"
    );
    assert_eq!(s.hits("/aes/key.bin"), 1, "the key is fetched once");
    e.shutdown().await;
}

#[tokio::test]
async fn an_encrypted_fmp4_stream_with_a_stated_iv_becomes_an_mp4() {
    let ff = need_ffmpeg!();
    let (addr, _s) = spawn_server().await;
    let t = dir();
    let e = engine(t.path(), &ff).await;
    let id = e
        .add_media(NewMediaDownload {
            url: url(addr, "aes-fmp4/iv/index.m3u8"),
            dir: Some(t.path().join("out")),
            container: Some(ContainerChoice::Mp4),
            filename: Some("iv".into()),
            ..Default::default()
        })
        .await
        .expect("adds");
    let d = wait_done(&e, id).await;
    assert_eq!(d.status, Status::Completed, "{:?}", d.error);
    let file = output_of(&d);
    let st = streams(&ff, &file);
    assert!(st.iter().any(|x| x.0 == "video" && x.1 == "h264"), "{st:?}");
    assert!(st.iter().any(|x| x.0 == "audio" && x.1 == "aac"), "{st:?}");
    let len = duration_of(&ff, &file);
    assert!((len - 6.0).abs() < 0.6, "duration {len}");
    e.shutdown().await;
}

#[tokio::test]
async fn a_wrong_key_fails_with_a_decryption_error() {
    let (addr, s) = spawn_server().await;
    s.faults.lock().unwrap().insert(
        "/aes/key.bin".into(),
        (usize::MAX, Fault::Bytes(b"0123456789abcdef")),
    );
    let t = dir();
    let e = engine_plain(t.path()).await;
    let id = e
        .add_media(NewMediaDownload {
            url: url(addr, "aes-fmp4/iv/index.m3u8"),
            dir: Some(t.path().join("out")),
            container: Some(ContainerChoice::Original),
            filename: Some("wrong".into()),
            ..Default::default()
        })
        .await
        .expect("adds");
    let d = wait_done(&e, id).await;
    assert_eq!(d.status, Status::Failed);
    assert!(
        d.error.as_deref().is_some_and(|m| m.contains("decrypted")),
        "{:?}",
        d.error
    );
    e.shutdown().await;
}

#[tokio::test]
async fn split_tracks_with_subtitles_become_one_mp4() {
    let ff = need_ffmpeg!();
    let (addr, _s) = spawn_server().await;
    let t = dir();
    let e = engine(t.path(), &ff).await;
    let p = probe(&e, &url(addr, "split/master.m3u8")).await;
    let de = p.audio[1].id.clone();
    let en = p.audio[0].id.clone();
    let sub = p.subtitles[0].id.clone();
    let id = e
        .add_media(NewMediaDownload {
            url: url(addr, "split/master.m3u8"),
            dir: Some(t.path().join("out")),
            container: Some(ContainerChoice::Mp4),
            subtitles: Some(SubtitleMode::Embed),
            selection: Some(Selection {
                video: Some(p.variants[0].video.id.clone()),
                audio: vec![en, de],
                subtitles: vec![sub],
            }),
            ..Default::default()
        })
        .await
        .expect("adds");
    let d = wait_done(&e, id).await;
    assert_eq!(d.status, Status::Completed, "{:?}", d.error);
    let file = output_of(&d);
    assert_eq!(file.extension().and_then(|x| x.to_str()), Some("mp4"));
    let s = streams(&ff, &file);
    assert_eq!(s.iter().filter(|x| x.0 == "video").count(), 1, "{s:?}");
    let audio: Vec<_> = s.iter().filter(|x| x.0 == "audio").collect();
    assert_eq!(audio.len(), 2, "{s:?}");
    // Languages are stored as ISO 639-2 codes.
    assert_eq!(audio[0].2.as_deref(), Some("eng"));
    assert_eq!(audio[1].2.as_deref(), Some("deu"));
    assert!(
        s.iter().any(|x| x.0 == "subtitle" && x.1 == "mov_text"),
        "{s:?}"
    );
    e.shutdown().await;
}

#[tokio::test]
async fn fmp4_hls_and_dash_download_as_mkv() {
    let ff = need_ffmpeg!();
    let (addr, _s) = spawn_server().await;
    let t = dir();
    let e = engine(t.path(), &ff).await;
    let a = e
        .add_media(NewMediaDownload {
            url: url(addr, "fmp4/index.m3u8"),
            dir: Some(t.path().join("out")),
            container: Some(ContainerChoice::Mkv),
            filename: Some("fmp4".into()),
            ..Default::default()
        })
        .await
        .expect("adds");
    let b = e
        .add_media(NewMediaDownload {
            url: url(addr, "dash/manifest.mpd"),
            dir: Some(t.path().join("out")),
            container: Some(ContainerChoice::Mkv),
            filename: Some("dash".into()),
            ..Default::default()
        })
        .await
        .expect("adds");
    let c = e
        .add_media(NewMediaDownload {
            url: url(addr, "dash/manifest.mpd"),
            dir: Some(t.path().join("out")),
            container: Some(ContainerChoice::Mp4),
            filename: Some("dash".into()),
            ..Default::default()
        })
        .await
        .expect("adds");
    for (id, want) in [(a, "mkv"), (b, "mkv"), (c, "mp4")] {
        let d = wait_done(&e, id).await;
        assert_eq!(d.status, Status::Completed, "{:?}", d.error);
        assert_eq!(
            output_of(&d).extension().and_then(|x| x.to_str()),
            Some(want)
        );
        let s = streams(&ff, &output_of(&d));
        assert!(
            s.iter().any(|x| x.0 == "video") && s.iter().any(|x| x.0 == "audio"),
            "{s:?}"
        );
        assert!((duration_of(&ff, &output_of(&d)) - 6.0).abs() < 0.8);
    }
    e.shutdown().await;
}

// ------------------------------------------------------------------ resilience

#[tokio::test]
async fn an_html_page_in_place_of_a_segment_refreshes_the_playlist() {
    let ff = need_ffmpeg!();
    let (addr, s) = spawn_server().await;
    s.faults
        .lock()
        .unwrap()
        .insert("/muxed/seg1.ts".into(), (1, Fault::Html));
    let t = dir();
    let e = engine(t.path(), &ff).await;
    let id = e
        .add_media(NewMediaDownload {
            url: url(addr, "muxed/index.m3u8"),
            dir: Some(t.path().join("out")),
            container: Some(ContainerChoice::Mkv),
            filename: Some("refreshed".into()),
            ..Default::default()
        })
        .await
        .expect("adds");
    let d = wait_done(&e, id).await;
    assert_eq!(d.status, Status::Completed, "{:?}", d.error);
    assert!(
        s.hits("/muxed/index.m3u8") >= 2,
        "the playlist was read again"
    );
    assert!(
        s.hits("/muxed/seg1.ts") >= 2,
        "the segment was fetched again"
    );
    e.shutdown().await;
}

#[tokio::test]
async fn an_expired_link_is_refreshed_and_the_segments_survive() {
    let ff = need_ffmpeg!();
    let (addr, s) = spawn_server().await;
    s.faults
        .lock()
        .unwrap()
        .insert("/muxed/seg2.ts".into(), (2, Fault::Forbidden));
    let t = dir();
    let e = engine(t.path(), &ff).await;
    let id = e
        .add_media(NewMediaDownload {
            url: url(addr, "muxed/index.m3u8"),
            dir: Some(t.path().join("out")),
            container: Some(ContainerChoice::Mkv),
            filename: Some("expired".into()),
            ..Default::default()
        })
        .await
        .expect("adds");
    let d = wait_done(&e, id).await;
    assert_eq!(d.status, Status::Completed, "{:?}", d.error);
    assert!(s.hits("/muxed/index.m3u8") >= 2);
    // The two good segments were fetched once each.
    assert_eq!(s.hits("/muxed/seg0.ts"), 1);
    assert_eq!(s.hits("/muxed/seg1.ts"), 1);
    e.shutdown().await;
}

#[tokio::test]
async fn pausing_keeps_finished_segments_and_resume_does_not_refetch_them() {
    let ff = need_ffmpeg!();
    let (addr, s) = spawn_server().await;
    *s.delay.lock().unwrap() = Some(Duration::from_millis(1500));
    let t = dir();
    let e = engine(t.path(), &ff).await;
    // One piece at a time, so there is a moment with a finished piece and one still in progress.
    e.set_settings(Settings {
        media_segment_concurrency: 1,
        ..e.settings()
    })
    .expect("settings");
    let id = e
        .add_media(NewMediaDownload {
            url: url(addr, "muxed/index.m3u8"),
            dir: Some(t.path().join("out")),
            container: Some(ContainerChoice::Mkv),
            filename: Some("paused".into()),
            ..Default::default()
        })
        .await
        .expect("adds");
    let deadline = Instant::now() + Duration::from_secs(30);
    while s.hits("/muxed/seg0.ts") == 0 && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // The first piece takes about 1.5 s; pause a little after the second one has started.
    tokio::time::sleep(Duration::from_millis(1800)).await;
    e.pause(id).expect("pauses");
    let d = wait_done(&e, id).await;
    assert_eq!(d.status, Status::Paused, "{:?}", d.error);
    let seg0 = std::fs::metadata(fixtures().join("muxed/seg0.ts"))
        .expect("fixture")
        .len();
    assert_eq!(
        d.downloaded, seg0,
        "a paused stream shows the bytes of its finished pieces"
    );
    *s.delay.lock().unwrap() = None;
    let seg0_before = s.hits("/muxed/seg0.ts");
    e.resume(id).expect("resumes");
    let d = wait_done(&e, id).await;
    assert_eq!(d.status, Status::Completed, "{:?}", d.error);
    assert_eq!(
        s.hits("/muxed/seg0.ts"),
        seg0_before,
        "a finished segment isn't fetched again"
    );
    e.shutdown().await;
}

/// A media download interrupted by a quit resumes when Siler starts again, with FFmpeg
/// known before anything is resumed (it used to be set just after, and a resumed rebuild
/// failed with "FFmpeg is needed").
#[tokio::test]
async fn a_download_interrupted_by_a_quit_finishes_after_a_restart() {
    let ff = need_ffmpeg!();
    let (addr, s) = spawn_server().await;
    *s.delay.lock().unwrap() = Some(Duration::from_millis(1500));
    let t = dir();
    let e = engine(t.path(), &ff).await;
    e.set_settings(Settings {
        media_segment_concurrency: 1,
        ..e.settings()
    })
    .expect("settings");
    let id = e
        .add_media(NewMediaDownload {
            url: url(addr, "muxed/index.m3u8"),
            dir: Some(t.path().join("out")),
            container: Some(ContainerChoice::Mkv),
            filename: Some("quit".into()),
            ..Default::default()
        })
        .await
        .expect("adds");
    let deadline = Instant::now() + Duration::from_secs(30);
    while s.hits("/muxed/seg1.ts") == 0 && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    e.shutdown().await;
    drop(e);
    *s.delay.lock().unwrap() = None;
    let seg0 = s.hits("/muxed/seg0.ts");
    let e = Engine::open_with(t.path(), None, Some(ff.clone())).expect("reopens");
    let d = wait_done(&e, id).await;
    assert_eq!(d.status, Status::Completed, "{:?}", d.error);
    assert_eq!(
        s.hits("/muxed/seg0.ts"),
        seg0,
        "a finished segment isn't fetched again"
    );
    assert!(streams(&ff, &output_of(&d)).iter().any(|x| x.0 == "video"));
    e.shutdown().await;
}

#[tokio::test]
async fn removing_a_media_download_deletes_its_work_directory() {
    let (addr, s) = spawn_server().await;
    *s.delay.lock().unwrap() = Some(Duration::from_millis(2500));
    let t = dir();
    let e = engine_plain(t.path()).await;
    // Without FFmpeg a multi-track output is refused up front, so use a single-track original.
    let id = e
        .add_media(NewMediaDownload {
            url: url(addr, "aes/index.m3u8"),
            dir: Some(t.path().join("out")),
            container: Some(ContainerChoice::Original),
            filename: Some("gone".into()),
            ..Default::default()
        })
        .await
        .expect("adds without FFmpeg for an original container");
    let deadline = Instant::now() + Duration::from_secs(20);
    while e.get(id).is_some_and(|d| d.status != Status::Downloading) && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let work = t.path().join("out").join(".gone.ts.siler.media");
    assert!(work.exists(), "the Work Directory exists while downloading");
    e.remove(id, false).expect("removes");
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(
        !work.exists(),
        "removing the download deletes its Work Directory"
    );
    e.shutdown().await;
}

#[tokio::test]
async fn multi_track_output_needs_ffmpeg() {
    let (addr, _s) = spawn_server().await;
    let t = dir();
    let e = engine_plain(t.path()).await;
    let err = e
        .add_media(NewMediaDownload {
            url: url(addr, "split/master.m3u8"),
            dir: Some(t.path().join("out")),
            container: Some(ContainerChoice::Mkv),
            ..Default::default()
        })
        .await
        .expect_err("needs FFmpeg");
    assert!(
        matches!(err, siler_core::Error::Media(MediaError::FfmpegMissing)),
        "{err:?}"
    );
    e.shutdown().await;
}

#[tokio::test]
async fn events_report_the_finished_media() {
    let ff = need_ffmpeg!();
    let (addr, _s) = spawn_server().await;
    let t = dir();
    let e = engine(t.path(), &ff).await;
    let mut rx = e.subscribe();
    let id = e
        .add_media(NewMediaDownload {
            url: url(addr, "muxed/index.m3u8"),
            dir: Some(t.path().join("out")),
            container: Some(ContainerChoice::Mkv),
            filename: Some("evt".into()),
            ..Default::default()
        })
        .await
        .expect("adds");
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        match tokio::time::timeout(Duration::from_secs(10), rx.recv()).await {
            Ok(Ok(Event::Completed {
                id: done, filename, ..
            })) if done == id => {
                assert_eq!(filename, "evt.mkv");
                break;
            }
            Ok(Ok(_)) => {}
            _ => assert!(Instant::now() < deadline, "no completion event"),
        }
    }
    let info = e.get(id).expect("exists");
    let m = info.media.expect("media details");
    assert_eq!(m.labels.len(), 1, "one muxed track: {:?}", m.labels);
    e.shutdown().await;
}

// ------------------------------------------------------------------ subtitles and names

/// The cue times of a WebVTT file, in seconds.
fn cue_starts(vtt: &str) -> Vec<f64> {
    vtt.lines()
        .filter_map(|l| l.split_once(" --> "))
        .map(|(a, _)| {
            let p: Vec<f64> = a
                .trim()
                .split(':')
                .map(|x| x.parse().unwrap_or(-1.0))
                .collect();
            p[0] * 3600.0 + p[1] * 60.0 + p[2]
        })
        .collect()
}

#[tokio::test]
async fn subtitles_keep_their_times_beside_and_inside_the_video() {
    let ff = need_ffmpeg!();
    let (addr, _s) = spawn_server().await;
    let t = dir();
    let e = engine(t.path(), &ff).await;
    let p = probe(&e, &url(addr, "split/master.m3u8")).await;
    let selection = Selection {
        video: Some(p.variants[0].video.id.clone()),
        audio: vec![p.audio[0].id.clone()],
        subtitles: vec![p.subtitles[0].id.clone()],
    };
    let add = |mode: SubtitleMode, name: &str| NewMediaDownload {
        url: url(addr, "split/master.m3u8"),
        dir: Some(t.path().join("out")),
        container: Some(ContainerChoice::Mkv),
        subtitles: Some(mode),
        selection: Some(selection.clone()),
        filename: Some(name.into()),
        ..Default::default()
    };
    let beside = e
        .add_media(add(SubtitleMode::Sidecar, "beside"))
        .await
        .expect("adds");
    let inside = e
        .add_media(add(SubtitleMode::Embed, "inside"))
        .await
        .expect("adds");

    // Beside: a WebVTT file named after the video and the language, cues on the video's
    // timeline, and the cue that crossed a segment boundary only once.
    let d = wait_done(&e, beside).await;
    assert_eq!(d.status, Status::Completed, "{:?}", d.error);
    let m = d.media.clone().expect("media");
    assert_eq!(m.sidecars, vec!["beside.en.vtt".to_string()]);
    let vtt = std::fs::read_to_string(t.path().join("out/beside.en.vtt")).expect("sidecar");
    assert_eq!(cue_starts(&vtt), vec![0.5, 1.8, 2.5, 4.5], "{vtt}");
    assert!(
        !streams(&ff, &output_of(&d))
            .iter()
            .any(|x| x.0 == "subtitle"),
        "the subtitles are beside the video, not in it"
    );

    // Inside: the same times in the file.
    let d = wait_done(&e, inside).await;
    assert_eq!(d.status, Status::Completed, "{:?}", d.error);
    let out = Command::new(ffprobe_of(&ff))
        .args([
            "-v",
            "error",
            "-select_streams",
            "s",
            "-show_entries",
            "packet=pts_time",
            "-of",
            "csv=p=0",
        ])
        .arg(output_of(&d))
        .output()
        .expect("ffprobe runs");
    let times: Vec<f64> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.split(',').next()?.trim().parse().ok())
        .collect();
    assert_eq!(times.len(), 4, "{times:?}");
    for (got, want) in times.iter().zip([0.5, 1.8, 2.5, 4.5]) {
        assert!((got - want).abs() < 0.05, "{times:?}");
    }

    // Deleting the finished download with its files takes the subtitle file too.
    e.remove(beside, true).expect("removes");
    assert!(!t.path().join("out/beside.en.vtt").exists());
    assert!(!t.path().join("out/beside.mkv").exists());
    e.shutdown().await;
}

/// The cues of a WebVTT file: (start, end, text).
fn cues_of(vtt: &str) -> Vec<(f64, f64, String)> {
    let secs = |t: &str| {
        t.trim()
            .split(':')
            .map(|x| x.parse::<f64>().unwrap_or(-1.0))
            .fold(0.0, |acc, x| acc * 60.0 + x)
    };
    vtt.split("\n\n")
        .filter_map(|block| {
            let mut lines = block.lines().skip_while(|l| !l.contains(" --> "));
            let (a, b) = lines.next()?.split_once(" --> ")?;
            let b = b.split_whitespace().next()?;
            Some((secs(a), secs(b), lines.collect::<Vec<_>>().join("\n")))
        })
        .collect()
}

/// WebVTT inside fMP4 (HLS), on a timeline that starts 10 s in: the cues come out once,
/// in step with the video, and none runs past its end.
#[tokio::test]
async fn subtitles_inside_mp4_follow_the_video_and_stop_at_its_end() {
    let ff = need_ffmpeg!();
    let (addr, _s) = spawn_server().await;
    let t = dir();
    let e = engine(t.path(), &ff).await;
    let p = probe(&e, &url(addr, "sub-fmp4/master.m3u8")).await;
    assert_eq!(p.subtitles.len(), 1, "{:?}", p.warnings);
    let selection = Selection {
        video: Some(p.variants[0].video.id.clone()),
        audio: Vec::new(),
        subtitles: vec![p.subtitles[0].id.clone()],
    };
    let add = |mode: SubtitleMode, name: &str| NewMediaDownload {
        url: url(addr, "sub-fmp4/master.m3u8"),
        dir: Some(t.path().join("out")),
        container: Some(ContainerChoice::Mkv),
        subtitles: Some(mode),
        selection: Some(selection.clone()),
        filename: Some(name.into()),
        ..Default::default()
    };
    let beside = e
        .add_media(add(SubtitleMode::Sidecar, "beside"))
        .await
        .expect("adds");
    let inside = e
        .add_media(add(SubtitleMode::Embed, "inside"))
        .await
        .expect("adds");

    let d = wait_done(&e, beside).await;
    assert_eq!(d.status, Status::Completed, "{:?}", d.error);
    let vtt = std::fs::read_to_string(t.path().join("out/beside.en.vtt")).expect("sidecar");
    let cues = cues_of(&vtt);
    // The stream's zero is its first sound, 10.179 s on its timeline (FFmpeg starts the file
    // there): "one" at 10.5 s plays 0.321 s in.
    let want = [
        (0.321, 1.321, "one"),
        (1.621, 2.421, "two, across a fragment"),
        (5.321, 6.0, "past the end"),
    ];
    assert_eq!(cues.len(), want.len(), "{vtt}");
    for (got, want) in cues.iter().zip(want) {
        assert!(
            (got.0 - want.0).abs() < 0.01 && (got.1 - want.1).abs() < 0.01,
            "{vtt}"
        );
        assert_eq!(got.2, want.2);
    }

    let d = wait_done(&e, inside).await;
    assert_eq!(d.status, Status::Completed, "{:?}", d.error);
    let file = output_of(&d);
    assert!(streams(&ff, &file).iter().any(|x| x.0 == "subtitle"));
    let len = duration_of(&ff, &file);
    assert!(
        len < 6.3,
        "the subtitles don't make the file longer than its video: {len}"
    );
    e.shutdown().await;
}

/// DASH with a `wvtt` track as one whole file and an `stpp` (TTML) track in segments.
#[tokio::test]
async fn dash_subtitles_in_mp4_become_tracks_of_the_file() {
    let ff = need_ffmpeg!();
    let (addr, _s) = spawn_server().await;
    let t = dir();
    let e = engine(t.path(), &ff).await;
    let p = probe(&e, &url(addr, "dash/subs.mpd")).await;
    assert_eq!(p.subtitles.len(), 2, "{:?}", p.warnings);
    assert!(
        p.warnings.iter().all(|w| !w.contains("subtitles")),
        "{:?}",
        p.warnings
    );
    let selection = Selection {
        video: Some(p.variants[0].video.id.clone()),
        audio: vec![p.audio[0].id.clone()],
        subtitles: p.subtitles.iter().map(|s| s.id.clone()).collect(),
    };
    let add = |mode: SubtitleMode, name: &str| NewMediaDownload {
        url: url(addr, "dash/subs.mpd"),
        dir: Some(t.path().join("out")),
        container: Some(ContainerChoice::Mkv),
        subtitles: Some(mode),
        selection: Some(selection.clone()),
        filename: Some(name.into()),
        ..Default::default()
    };
    let beside = e
        .add_media(add(SubtitleMode::Sidecar, "dash"))
        .await
        .expect("adds");
    let inside = e
        .add_media(add(SubtitleMode::Embed, "dashin"))
        .await
        .expect("adds");

    let d = wait_done(&e, beside).await;
    assert_eq!(d.status, Status::Completed, "{:?}", d.error);
    let mut sidecars = d.media.clone().expect("media").sidecars;
    sidecars.sort();
    assert_eq!(sidecars, ["dash.de.vtt", "dash.en.vtt"]);
    let en = cues_of(&std::fs::read_to_string(t.path().join("out/dash.en.vtt")).expect("en"));
    let de = cues_of(&std::fs::read_to_string(t.path().join("out/dash.de.vtt")).expect("de"));
    let starts = |c: &[(f64, f64, String)]| {
        c.iter()
            .map(|x| (x.0 * 10.0).round() / 10.0)
            .collect::<Vec<_>>()
    };
    assert_eq!(starts(&en), [0.5, 1.8, 5.5], "{en:?}");
    assert_eq!(en[1].2, "across");
    // Cut where the sound ends (6.037 s, a little after the picture), not at 7 s.
    assert!(
        (en[2].1 - 6.037).abs() < 0.01,
        "cut at the end of the media: {en:?}"
    );
    assert_eq!(starts(&de), [1.0, 2.5, 4.2], "{de:?}");
    assert_eq!(de[0].2, "eins");

    let d = wait_done(&e, inside).await;
    assert_eq!(d.status, Status::Completed, "{:?}", d.error);
    let st = streams(&ff, &output_of(&d));
    let subs: Vec<_> = st
        .iter()
        .filter(|x| x.0 == "subtitle")
        .map(|x| x.2.clone())
        .collect();
    assert_eq!(subs.len(), 2, "{st:?}");
    assert!(subs.contains(&Some("eng".into())), "{st:?}");
    assert!(
        subs.contains(&Some("ger".into())) || subs.contains(&Some("deu".into())),
        "{st:?}"
    );
    e.shutdown().await;
}

#[tokio::test]
async fn two_streams_with_one_name_never_share_a_work_directory() {
    let (addr, s) = spawn_server().await;
    *s.delay.lock().unwrap() = Some(Duration::from_millis(300));
    let t = dir();
    let e = engine_plain(t.path()).await;
    let add = || NewMediaDownload {
        url: url(addr, "aes/index.m3u8"),
        dir: Some(t.path().join("out")),
        container: Some(ContainerChoice::Original),
        filename: Some("same.mp4".into()),
        ..Default::default()
    };
    let a = e.add_media(add()).await.expect("adds");
    let b = e.add_media(add()).await.expect("adds");
    let (na, nb) = (
        e.get(a).and_then(|d| d.filename).expect("name"),
        e.get(b).and_then(|d| d.filename).expect("name"),
    );
    // A given media extension is replaced by the output's own.
    assert_eq!(na, "same.ts");
    assert_eq!(nb, "same (1).ts");
    for id in [a, b] {
        let d = wait_done(&e, id).await;
        assert_eq!(d.status, Status::Completed, "{:?}", d.error);
    }
    let want: Vec<u8> = (0..3)
        .flat_map(|i| std::fs::read(fixtures().join(format!("muxed/seg{i}.ts"))).expect("fixture"))
        .collect();
    for n in ["same.ts", "same (1).ts"] {
        assert!(
            std::fs::read(t.path().join("out").join(n)).expect("output") == want,
            "{n}"
        );
    }
    e.shutdown().await;
}

#[tokio::test]
async fn skip_leaves_an_existing_file_alone() {
    let (addr, s) = spawn_server().await;
    let t = dir();
    let e = engine_plain(t.path()).await;
    std::fs::create_dir_all(t.path().join("out")).expect("dir");
    std::fs::write(t.path().join("out/kept.ts"), b"mine").expect("file");
    let id = e
        .add_media(NewMediaDownload {
            url: url(addr, "aes/index.m3u8"),
            dir: Some(t.path().join("out")),
            container: Some(ContainerChoice::Original),
            filename: Some("kept".into()),
            on_conflict: Some(siler_core::OnConflict::Skip),
            ..Default::default()
        })
        .await
        .expect("adds");
    let d = wait_done(&e, id).await;
    assert_eq!(d.status, Status::Failed);
    assert_eq!(
        std::fs::read(t.path().join("out/kept.ts")).expect("file"),
        b"mine"
    );
    assert_eq!(s.hits("/aes/seg0.ts"), 0, "nothing was downloaded");
    e.shutdown().await;
}

// ------------------------------------------------------------------ streams the page gave

/// Whole MP4 files like YouTube's (video and audio apart), made from the DASH fixtures, and
/// a WebVTT file, in `dir`.
fn whole_files(dir: &Path) {
    let d = fixtures().join("dash");
    for (out, init, n) in [
        ("video.mp4", "init-0.m4s", 3),
        ("audio.mp4", "init-1.m4s", 4),
    ] {
        let mut b = std::fs::read(d.join(init)).expect("init");
        for i in 1..=n {
            b.extend(
                std::fs::read(d.join(format!("chunk-{}-{i:05}.m4s", &init[5..6]))).expect("chunk"),
            );
        }
        std::fs::write(dir.join(out), b).expect("write");
    }
    std::fs::write(
        dir.join("en.vtt"),
        "WEBVTT\n\n00:00:00.500 --> 00:00:02.000\nHello\n\n00:00:03.000 --> 00:00:04.000\nAgain\n",
    )
    .expect("vtt");
}

/// What the YouTube recogniser hands over: an MPD it wrote, with whole files in ranges.
fn page_hint(addr: SocketAddr) -> SourceHint {
    let mpd = format!(
        r#"<?xml version="1.0"?><MPD xmlns="urn:mpeg:dash:schema:mpd:2011" type="static" mediaPresentationDuration="PT6S"><Period><AdaptationSet mimeType="video/mp4"><Representation id="137" codecs="avc1.64001f" bandwidth="200000" width="320" height="240"><BaseURL>http://{addr}/extra/video.mp4?expire=1&amp;n=solved</BaseURL><SegmentBase indexRange="0-0"><Initialization range="0-0"/></SegmentBase></Representation></AdaptationSet><AdaptationSet mimeType="audio/mp4" lang="en"><Representation id="140" codecs="mp4a.40.2" bandwidth="64000"><BaseURL>http://{addr}/extra/audio.mp4?expire=1</BaseURL><SegmentBase indexRange="0-0"/></Representation></AdaptationSet></Period></MPD>"#
    );
    SourceHint {
        site: Some("youtube".into()),
        title: Some("A video from the page".into()),
        manifest: Some(InlineManifest {
            kind: "dash".into(),
            body: mpd,
            base_url: format!("http://{addr}/watch?v=x"),
        }),
        subtitles: vec![HintSubtitle {
            url: format!("http://{addr}/extra/en.vtt?lang=en"),
            lang: Some("en".into()),
            label: Some("English".into()),
        }],
        chunk_bytes: Some(16 << 10),
        not_before_ms: None,
    }
}

#[tokio::test]
async fn a_stream_the_page_gave_as_text_is_downloaded_in_ranges() {
    let ff = need_ffmpeg!();
    let (addr, s) = spawn_server().await;
    let t = dir();
    whole_files(t.path());
    *s.extra.lock().unwrap() = Some(t.path().to_path_buf());
    let e = engine(t.path(), &ff).await;
    let page = url(addr, "watch?v=x");
    let hint = page_hint(addr);

    let p = e
        .probe_media(&page, &RequestMeta::default(), Some(&hint))
        .await
        .expect("probe");
    assert_eq!(p.kind, MediaKind::Dash);
    assert_eq!(p.title.as_deref(), Some("A video from the page"));
    assert_eq!(p.subtitles.len(), 1, "the page's subtitles are offered");
    let mut sel = p.default.clone();
    sel.subtitles = vec![p.subtitles[0].id.clone()];
    let id = e
        .add_media(NewMediaDownload {
            url: page.clone(),
            hint: Some(hint),
            selection: Some(sel),
            container: Some(ContainerChoice::Mkv),
            subtitles: Some(SubtitleMode::Embed),
            ..Default::default()
        })
        .await
        .expect("add");
    let d = wait_done(&e, id).await;
    assert_eq!(d.status, Status::Completed, "{:?}", d.error);
    assert_eq!(d.filename.as_deref(), Some("A video from the page.mkv"));
    assert_eq!(s.hits("/watch"), 0, "the page itself is never fetched");
    let kinds: Vec<String> = streams(&ff, &output_of(&d))
        .into_iter()
        .map(|s| s.0)
        .collect();
    assert_eq!(kinds, ["video", "audio", "subtitle"]);
    // The audio file is the larger one here (about 50 kB): several ranges.
    let audio_len = std::fs::metadata(t.path().join("audio.mp4"))
        .expect("len")
        .len();
    let ranges: Vec<String> = s
        .ranges
        .lock()
        .unwrap()
        .iter()
        .filter(|(p, _)| p == "/extra/audio.mp4")
        .map(|(_, r)| r.clone())
        .collect();
    assert_eq!(
        ranges.len() as u64,
        audio_len.div_ceil(16 << 10),
        "{ranges:?}"
    );
    assert!(ranges.len() >= 3, "{ranges:?}");
    assert_eq!(ranges[0], "bytes=0-16383");
    assert_eq!(ranges[1], "bytes=16384-32767");
    e.shutdown().await;
}

#[tokio::test]
async fn a_video_from_the_page_reports_its_size_while_it_downloads() {
    let (addr, s) = spawn_server().await;
    let t = dir();
    whole_files(t.path());
    *s.extra.lock().unwrap() = Some(t.path().to_path_buf());
    // Each range takes a while, so progress is reported in the middle.
    *s.delay.lock().unwrap() = Some(Duration::from_millis(300));
    let e = engine_plain(t.path()).await;
    let mut rx = e.subscribe();
    let page = url(addr, "watch?v=x");
    let hint = page_hint(addr);
    let p = e
        .probe_media(&page, &RequestMeta::default(), Some(&hint))
        .await
        .expect("probe");
    // The audio alone (no FFmpeg needed): one whole file, fetched in several ranges.
    let sel = Selection {
        video: None,
        audio: p.default.audio.clone(),
        subtitles: Vec::new(),
    };
    let id = e
        .add_media(NewMediaDownload {
            url: page,
            hint: Some(hint),
            selection: Some(sel),
            container: Some(ContainerChoice::Original),
            ..Default::default()
        })
        .await
        .expect("add");
    let audio_len = std::fs::metadata(t.path().join("audio.mp4"))
        .expect("len")
        .len();
    // (downloaded, bytes_total) of each progress report in the downloading phase.
    let mut seen = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        match tokio::time::timeout(Duration::from_secs(10), rx.recv()).await {
            Ok(Ok(Event::Progress { items, .. })) => {
                for it in items.into_iter().filter(|it| it.id == id) {
                    if let Some(m) = it.media.filter(|m| m.phase == "downloading") {
                        seen.push((it.downloaded, m.bytes_total));
                    }
                }
            }
            Ok(Ok(Event::Completed { id: done, .. })) if done == id => break,
            Ok(Ok(_)) => {}
            _ => assert!(Instant::now() < deadline, "no completion event"),
        }
    }
    let midway: Vec<_> = seen
        .iter()
        .filter(|(got, _)| *got > 0 && *got < audio_len)
        .collect();
    assert!(!midway.is_empty(), "no report in the middle: {seen:?}");
    assert!(
        midway.iter().all(|(_, total)| *total == Some(audio_len)),
        "the size ({audio_len}) isn't known while it downloads: {midway:?}"
    );
    e.shutdown().await;
}

#[tokio::test]
async fn a_fresh_manifest_from_the_page_continues() {
    let (addr, s) = spawn_server().await;
    let t = dir();
    whole_files(t.path());
    *s.extra.lock().unwrap() = Some(t.path().to_path_buf());
    s.faults
        .lock()
        .unwrap()
        .insert("/extra/audio.mp4".into(), (99, Fault::Forbidden));
    let e = engine_plain(t.path()).await;
    let page = url(addr, "watch?v=x");
    let hint = page_hint(addr);
    let p = e
        .probe_media(&page, &RequestMeta::default(), Some(&hint))
        .await
        .expect("probe");
    let id = e
        .add_media(NewMediaDownload {
            url: page.clone(),
            hint: Some(hint),
            selection: Some(Selection {
                video: None,
                audio: p.default.audio.clone(),
                subtitles: Vec::new(),
            }),
            container: Some(ContainerChoice::Original),
            ..Default::default()
        })
        .await
        .expect("add");
    let d = wait_done(&e, id).await;
    assert!(d.needs_link, "the user isn't asked for a fresh link: {d:?}");

    // What the browser hands over next: the same page with a newly signed manifest, whose
    // links live under another path.
    s.faults.lock().unwrap().clear();
    let mut fresh = page_hint(addr);
    let m = fresh.manifest.as_mut().expect("a manifest");
    m.body = m.body.replace(
        "/extra/audio.mp4?expire=1",
        "/redir/extra/audio.mp4?expire=2",
    );
    e.refresh_link(
        id,
        siler_core::FoundLink {
            url: page,
            hint: Some(fresh),
            ..Default::default()
        },
    )
    .await
    .expect("refresh");
    let d = wait_done(&e, id).await;
    assert_eq!(d.status, Status::Completed, "{:?}", d.error);
    // A track that is one file keeps its own format's ending.
    let name = d.filename.as_deref().unwrap_or_default();
    assert!(name.ends_with(".mp4"), "saved as {name}");
    e.shutdown().await;
}

#[tokio::test]
async fn expired_links_from_the_page_say_so() {
    let (addr, s) = spawn_server().await;
    let t = dir();
    whole_files(t.path());
    *s.extra.lock().unwrap() = Some(t.path().to_path_buf());
    s.faults
        .lock()
        .unwrap()
        .insert("/extra/audio.mp4".into(), (99, Fault::Forbidden));
    let e = engine_plain(t.path()).await;
    let page = url(addr, "watch?v=x");
    let hint = page_hint(addr);
    let p = e
        .probe_media(&page, &RequestMeta::default(), Some(&hint))
        .await
        .expect("probe");
    // The audio alone, as it was (no FFmpeg needed).
    let sel = Selection {
        video: None,
        audio: p.default.audio.clone(),
        subtitles: Vec::new(),
    };
    let id = e
        .add_media(NewMediaDownload {
            url: page,
            hint: Some(hint),
            selection: Some(sel),
            container: Some(ContainerChoice::Original),
            ..Default::default()
        })
        .await
        .expect("add");
    let d = wait_done(&e, id).await;
    assert_eq!(d.status, Status::Failed);
    assert_eq!(
        d.error.as_deref(),
        Some(MediaError::LinksExpired.to_string().as_str())
    );
    assert_eq!(
        s.hits("/extra/audio.mp4"),
        1,
        "an expired link isn't asked again"
    );
    e.shutdown().await;
}

#[tokio::test]
async fn the_download_waits_until_the_site_allows_it() {
    let (addr, s) = spawn_server().await;
    let t = dir();
    whole_files(t.path());
    *s.extra.lock().unwrap() = Some(t.path().to_path_buf());
    let e = engine_plain(t.path()).await;
    let page = url(addr, "watch?v=x");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("time")
        .as_millis() as u64;
    let hint = SourceHint {
        not_before_ms: Some(now + 1500),
        ..page_hint(addr)
    };
    let p = e
        .probe_media(&page, &RequestMeta::default(), Some(&hint))
        .await
        .expect("probe");
    let start = Instant::now();
    let id = e
        .add_media(NewMediaDownload {
            url: page,
            hint: Some(hint),
            selection: Some(Selection {
                video: None,
                audio: p.default.audio.clone(),
                subtitles: Vec::new(),
            }),
            container: Some(ContainerChoice::Original),
            ..Default::default()
        })
        .await
        .expect("add");
    let d = wait_done(&e, id).await;
    assert_eq!(d.status, Status::Completed, "{:?}", d.error);
    let first = s.first.lock().unwrap()["/extra/audio.mp4"];
    assert!(
        first.duration_since(start) >= Duration::from_millis(1300),
        "fetched after {:?}",
        first.duration_since(start)
    );
    e.shutdown().await;
}

// ------------------------------------------------------------------ HDS, playlists, live

#[tokio::test]
async fn hds_fragments_become_one_mp4() {
    let ff = need_ffmpeg!();
    let (addr, s) = spawn_server().await;
    let t = dir();
    let e = engine(t.path(), &ff).await;
    let p = probe(&e, &url(addr, "hds/manifest.f4m")).await;
    assert_eq!(p.kind, MediaKind::Hds, "{:?}", p.message);
    assert_eq!(p.variants.len(), 1);
    assert!(!p.live);
    let id = e
        .add_media(NewMediaDownload {
            url: url(addr, "hds/manifest.f4m"),
            dir: Some(t.path().join("out")),
            container: Some(ContainerChoice::Mp4),
            filename: Some("hds".into()),
            ..Default::default()
        })
        .await
        .expect("adds");
    let d = wait_done(&e, id).await;
    assert_eq!(d.status, Status::Completed, "{:?}", d.error);
    assert!(
        s.hits("/hds/boot.abst") >= 2,
        "read at the probe and at the download"
    );
    let out = output_of(&d);
    assert!(out.to_string_lossy().ends_with(".mp4"));
    let kinds: Vec<String> = streams(&ff, &out).into_iter().map(|s| s.1).collect();
    assert!(kinds.contains(&"h264".to_string()), "{kinds:?}");
    assert!(kinds.contains(&"aac".to_string()), "{kinds:?}");
    let secs = duration_of(&ff, &out);
    assert!((5.0..7.0).contains(&secs), "{secs}");
    e.shutdown().await;
}

#[tokio::test]
async fn a_plain_playlist_lists_its_links() {
    let (addr, s) = spawn_server().await;
    let t = dir();
    let extra = t.path().join("extra");
    std::fs::create_dir_all(&extra).unwrap();
    std::fs::write(
        extra.join("radio.pls"),
        "[playlist]\nFile1=http://radio.example/a.mp3\nTitle1=Morning\nFile2=mms://radio.example/live\nNumberOfEntries=2\n",
    )
    .unwrap();
    *s.extra.lock().unwrap() = Some(extra);
    let e = engine_plain(t.path()).await;
    let p = probe(&e, &url(addr, "extra/radio.pls")).await;
    assert_eq!(p.kind, MediaKind::Playlist);
    let urls: Vec<&str> = p.entries.iter().map(|e| e.url.as_str()).collect();
    assert_eq!(
        urls,
        ["http://radio.example/a.mp3", "mms://radio.example/live"]
    );
    assert_eq!(p.entries[0].title.as_deref(), Some("Morning"));
    e.shutdown().await;
}

/// A live playlist over the muxed fixture's segments: `first..=last`, by media sequence.
fn live_playlist(seq: u64, segs: &[usize], ended: bool) -> String {
    let mut s = format!(
        "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:2\n#EXT-X-MEDIA-SEQUENCE:{seq}\n"
    );
    for i in segs {
        s.push_str(&format!("#EXTINF:2.0,\n/muxed/seg{i}.ts\n"));
    }
    if ended {
        s.push_str("#EXT-X-ENDLIST\n");
    }
    s
}

async fn live_setup(playlist: String) -> (SocketAddr, Arc<Shared>, tempfile::TempDir, PathBuf) {
    let (addr, s) = spawn_server().await;
    let t = dir();
    let extra = t.path().join("extra");
    std::fs::create_dir_all(&extra).unwrap();
    std::fs::write(extra.join("live.m3u8"), playlist).unwrap();
    *s.extra.lock().unwrap() = Some(extra.clone());
    (addr, s, t, extra)
}

async fn wait_hit(s: &Shared, path: &str) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while s.hits(path) == 0 {
        assert!(Instant::now() < deadline, "{path} was never fetched");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn a_live_stream_is_recorded_until_it_ends() {
    let ff = need_ffmpeg!();
    let (addr, s, t, extra) = live_setup(live_playlist(0, &[0, 1], false)).await;
    let e = engine(t.path(), &ff).await;
    let p = probe(&e, &url(addr, "extra/live.m3u8")).await;
    assert_eq!(p.kind, MediaKind::Hls);
    assert!(p.live);
    let id = e
        .add_media(NewMediaDownload {
            url: url(addr, "extra/live.m3u8"),
            dir: Some(t.path().join("out")),
            container: Some(ContainerChoice::Mkv),
            filename: Some("live".into()),
            ..Default::default()
        })
        .await
        .expect("adds");
    wait_hit(&s, "/muxed/seg1.ts").await;
    // The stream goes on, then ends.
    std::fs::write(extra.join("live.m3u8"), live_playlist(0, &[0, 1, 2], true)).unwrap();
    let d = wait_done(&e, id).await;
    assert_eq!(d.status, Status::Completed, "{:?}", d.error);
    assert_eq!(s.hits("/muxed/seg0.ts"), 1, "nothing is fetched twice");
    let secs = duration_of(&ff, &output_of(&d));
    assert!((5.0..7.0).contains(&secs), "{secs}");
    e.shutdown().await;
}

#[tokio::test]
async fn finish_ends_a_recording_and_keeps_it() {
    let ff = need_ffmpeg!();
    let (addr, s, t, _extra) = live_setup(live_playlist(0, &[0, 1], false)).await;
    let e = engine(t.path(), &ff).await;
    let id = e
        .add_media(NewMediaDownload {
            url: url(addr, "extra/live.m3u8"),
            dir: Some(t.path().join("out")),
            container: Some(ContainerChoice::Mp4),
            filename: Some("finished".into()),
            ..Default::default()
        })
        .await
        .expect("adds");
    wait_hit(&s, "/muxed/seg1.ts").await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    e.finish(id).expect("finishes");
    let d = wait_done(&e, id).await;
    assert_eq!(d.status, Status::Completed, "{:?}", d.error);
    let secs = duration_of(&ff, &output_of(&d));
    assert!((3.0..5.0).contains(&secs), "{secs}");
    assert_eq!(s.hits("/muxed/seg2.ts"), 0);
    e.shutdown().await;
}

#[tokio::test]
async fn a_recording_behind_the_live_edge_catches_up_and_finish_stops_it_at_once() {
    let ff = need_ffmpeg!();
    let (addr, s, t, extra) = live_setup(live_playlist(0, &[0], false)).await;
    let e = engine(t.path(), &ff).await;
    let id = e
        .add_media(NewMediaDownload {
            url: url(addr, "extra/live.m3u8"),
            dir: Some(t.path().join("out")),
            container: Some(ContainerChoice::Mkv),
            filename: Some("behind".into()),
            ..Default::default()
        })
        .await
        .expect("adds");
    wait_hit(&s, "/muxed/seg0.ts").await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    // Slow segments, and two new ones at once: they are fetched side by side.
    *s.delay.lock().unwrap() = Some(Duration::from_secs(4));
    std::fs::write(extra.join("live.m3u8"), live_playlist(0, &[0, 1, 2], false)).unwrap();
    wait_hit(&s, "/muxed/seg1.ts").await;
    wait_hit(&s, "/muxed/seg2.ts").await;
    let (a, b) = {
        let first = s.first.lock().unwrap();
        (first["/muxed/seg1.ts"], first["/muxed/seg2.ts"])
    };
    assert!(
        b.duration_since(a) < Duration::from_millis(1000),
        "fetched one after the other"
    );
    // Finish doesn't wait for the segments still coming: the recording keeps the first.
    let pressed = Instant::now();
    e.finish(id).expect("finishes");
    let d = wait_done(&e, id).await;
    assert!(
        pressed.elapsed() < Duration::from_secs(3),
        "{:?}",
        pressed.elapsed()
    );
    assert_eq!(d.status, Status::Completed, "{:?}", d.error);
    let secs = duration_of(&ff, &output_of(&d));
    assert!((1.0..3.0).contains(&secs), "{secs}");
    *s.delay.lock().unwrap() = None;
    e.shutdown().await;
}

#[tokio::test]
async fn a_paused_recording_continues_from_the_live_edge_with_a_gap() {
    let ff = need_ffmpeg!();
    let (addr, s, t, extra) = live_setup(live_playlist(0, &[0], false)).await;
    let e = engine(t.path(), &ff).await;
    let mut events = e.subscribe();
    let id = e
        .add_media(NewMediaDownload {
            url: url(addr, "extra/live.m3u8"),
            dir: Some(t.path().join("out")),
            container: Some(ContainerChoice::Mkv),
            filename: Some("gap".into()),
            ..Default::default()
        })
        .await
        .expect("adds");
    wait_hit(&s, "/muxed/seg0.ts").await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    e.pause(id).expect("pauses");
    let d = wait_done(&e, id).await;
    assert_eq!(d.status, Status::Paused, "{:?}", d.error);
    // Meanwhile the stream moved on: sequence 5 is seg2 (sequences 1–4 were missed).
    std::fs::write(extra.join("live.m3u8"), live_playlist(5, &[2], false)).unwrap();
    e.resume(id).expect("resumes");
    wait_hit(&s, "/muxed/seg2.ts").await;
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut gaps = 0;
    while gaps == 0 && Instant::now() < deadline {
        if let Ok(Ok(Event::Progress { items, .. })) =
            tokio::time::timeout(Duration::from_millis(200), events.recv()).await
        {
            gaps = items
                .iter()
                .find(|i| i.id == id)
                .and_then(|i| i.media.as_ref())
                .map_or(0, |m| m.gaps);
        }
    }
    assert_eq!(gaps, 1, "the missed segments count as one gap");
    e.finish(id).expect("finishes");
    let d = wait_done(&e, id).await;
    assert_eq!(d.status, Status::Completed, "{:?}", d.error);
    assert_eq!(s.hits("/muxed/seg1.ts"), 0);
    e.shutdown().await;
}

/// Needs an FFmpeg with the RTMP muxer to play the server (`SILER_FULL_FFMPEG`, e.g. the
/// system's), besides Siler's own for recording.
#[tokio::test]
async fn an_rtmp_stream_is_recorded_until_finish() {
    let ff = need_ffmpeg!();
    let Some(full) = std::env::var_os("SILER_FULL_FFMPEG").map(PathBuf::from) else {
        eprintln!("skipped: SILER_FULL_FFMPEG not set");
        return;
    };
    let t = dir();
    let e = engine(t.path(), &ff).await;
    if !e.ffmpeg_status().network {
        eprintln!("skipped: this FFmpeg can't record RTMP");
        return;
    }
    e.set_settings(Settings {
        allow_private_media_addresses: true,
        ..e.settings()
    })
    .expect("settings");
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let rtmp = format!("rtmp://127.0.0.1:{port}/live/test");
    let mut server = Command::new(&full)
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-re",
            "-stream_loop",
            "-1",
            "-i",
        ])
        .arg(fixtures().join("muxed/seg0.ts"))
        .args(["-c", "copy", "-f", "flv", "-listen", "1"])
        .arg(&rtmp)
        .spawn()
        .expect("RTMP server starts");
    tokio::time::sleep(Duration::from_millis(500)).await;
    let p = probe(&e, &rtmp).await;
    assert_eq!(p.kind, MediaKind::Stream);
    let id = e
        .add_media(NewMediaDownload {
            url: rtmp.clone(),
            dir: Some(t.path().join("out")),
            filename: Some("rtmp".into()),
            ..Default::default()
        })
        .await
        .expect("adds");
    let deadline = Instant::now() + Duration::from_secs(20);
    while e.get(id).unwrap().downloaded < 20_000 && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    e.finish(id).expect("finishes");
    let d = wait_done(&e, id).await;
    let _ = server.kill();
    let _ = server.wait();
    assert_eq!(d.status, Status::Completed, "{:?}", d.error);
    let out = output_of(&d);
    assert!(out.to_string_lossy().ends_with(".mp4"), "{out:?}");
    assert!(duration_of(&ff, &out) > 0.5);
    e.shutdown().await;
}
