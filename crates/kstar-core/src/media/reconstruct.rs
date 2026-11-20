//! Rebuilding the output from the fetched pieces (media-design §13): each track's
//! initialisation segment and segments are joined into one file, subtitles become one
//! WebVTT file, and FFmpeg muxes the tracks with stream copy (no re-encoding).

use std::path::{Path, PathBuf};

use kstar_media::classify::sniff_segment;
use kstar_media::select::Output;
use kstar_media::{ParseLimits, SegmentContainer, Track, TrackKind, subtitle};

use crate::error::MediaError;
use crate::{Error, Result};

/// The directory a track's pieces live in, inside the Work Directory.
pub(crate) fn track_dir(work: &Path, track: &Track) -> PathBuf {
    work.join("t").join(track.id.as_str())
}

/// The file of one fetched segment (or initialisation segment) of a track.
pub(crate) fn piece_path(work: &Path, track: &Track, index: usize) -> PathBuf {
    track_dir(work, track).join(format!("{index:08}.seg"))
}

/// The file of one initialisation segment of a track.
pub(crate) fn init_path(work: &Path, track: &Track, index: usize) -> PathBuf {
    track_dir(work, track).join(format!("init-{index}.bin"))
}

/// The file of a track that is one whole file (DASH `SegmentBase`, a plain URL).
pub(crate) fn whole_path(work: &Path, track: &Track) -> PathBuf {
    track_dir(work, track).join("whole.bin")
}

/// Joins a track's pieces into one file: its initialisation segment (when the container
/// needs one), then its segments in order. A whole-file track is already one file.
pub(crate) fn join_track(work: &Path, track: &Track) -> Result<PathBuf> {
    if track.plan.whole {
        let p = whole_path(work, track);
        if !p.exists() {
            return Err(Error::Media(MediaError::SegmentInvalid));
        }
        return Ok(p);
    }
    let out = track_dir(work, track).join(format!("track.{}", track.container.extension()));
    let part = PathBuf::from(format!("{}.part", out.display()));
    let inits: Vec<usize> = {
        let mut v: Vec<usize> = track.plan.segments.iter().filter_map(|s| s.init).collect();
        v.sort_unstable();
        v.dedup();
        v
    };
    if inits.len() > 1 && track.container == SegmentContainer::Fmp4 {
        return Err(Error::Media(MediaError::DiscontinuityIncompatible(
            "the stream starts over with a new header partway through".into(),
        )));
    }
    {
        use std::io::Write;
        let mut file = std::io::BufWriter::new(std::fs::File::create(&part)?);
        if let Some(&first) = inits.first() {
            std::io::copy(
                &mut std::fs::File::open(init_path(work, track, first))?,
                &mut file,
            )?;
        }
        for (i, _) in track.plan.segments.iter().enumerate() {
            let p = piece_path(work, track, i);
            let mut src =
                std::fs::File::open(&p).map_err(|_| Error::Media(MediaError::SegmentInvalid))?;
            std::io::copy(&mut src, &mut file)?;
        }
        file.flush()?;
    }
    std::fs::rename(&part, &out)?;
    Ok(out)
}

/// Joins the cues of a subtitle track into one WebVTT file (converting TTML). Returns the
/// file's path.
pub(crate) fn join_subtitles(work: &Path, track: &Track, lim: &ParseLimits) -> Result<PathBuf> {
    let out = track_dir(work, track).join("subs.vtt");
    let mut bodies: Vec<(f64, Vec<u8>)> = Vec::new();
    if track.plan.whole {
        bodies.push((0.0, std::fs::read(whole_path(work, track))?));
    } else {
        for (i, seg) in track.plan.segments.iter().enumerate() {
            let body = std::fs::read(piece_path(work, track, i))
                .map_err(|_| Error::Media(MediaError::SegmentInvalid))?;
            bodies.push((seg.start, body));
        }
    }
    // Each piece is WebVTT with times relative to its own start, or TTML with absolute times.
    let mut converted: Vec<(f64, Vec<u8>)> = Vec::with_capacity(bodies.len());
    for (start, body) in bodies {
        match sniff_segment(&body) {
            SegmentContainer::Ttml => {
                let vtt = subtitle::ttml_to_webvtt(&body, lim).map_err(super::probe::map_parse)?;
                converted.push((0.0, vtt.into_bytes()));
            }
            SegmentContainer::Vtt => converted.push((start, body)),
            _ => {
                return Err(Error::Media(MediaError::Unsupported(
                    "this subtitle format isn't supported yet".into(),
                )));
            }
        }
    }
    let parts: Vec<subtitle::VttPart<'_>> = converted
        .iter()
        .map(|(start, body)| subtitle::VttPart {
            start: *start,
            body,
        })
        .collect();
    let merged = subtitle::merge_webvtt(&parts).map_err(super::probe::map_parse)?;
    std::fs::write(&out, merged)?;
    Ok(out)
}

/// One input to the mux.
pub(crate) struct MuxInput {
    pub path: PathBuf,
    pub kind: TrackKind,
    pub lang: Option<String>,
    /// AAC in ADTS framing (MPEG-TS or raw AAC): needs `aac_adtstoasc` for MP4.
    pub adts: bool,
}

/// The three-letter ISO 639-2 code MP4 and Matroska store for a language tag (`en` → `eng`).
/// Unknown languages are left untagged rather than tagged wrongly.
pub(crate) fn iso639_2(lang: &str) -> Option<&'static str> {
    let base = lang.split(['-', '_']).next()?.to_ascii_lowercase();
    Some(match base.as_str() {
        "en" => "eng",
        "de" => "deu",
        "fr" => "fra",
        "es" => "spa",
        "it" => "ita",
        "pt" => "por",
        "ru" => "rus",
        "ja" => "jpn",
        "zh" => "zho",
        "ko" => "kor",
        "nl" => "nld",
        "pl" => "pol",
        "sv" => "swe",
        "da" => "dan",
        "no" => "nor",
        "fi" => "fin",
        "tr" => "tur",
        "ar" => "ara",
        "hi" => "hin",
        "cs" => "ces",
        "el" => "ell",
        "he" => "heb",
        "hu" => "hun",
        "uk" => "ukr",
        _ => return None,
    })
}

/// The FFmpeg arguments that stream-copy the inputs into one output file (media-design §13.4).
/// Paths are passed with the `file:` prefix, and the protocol whitelist admits only files and
/// pipes. Nothing from a manifest is ever an argument: only K-Star's own paths.
pub(crate) fn mux_args(inputs: &[MuxInput], output: Output, out: &Path) -> Vec<String> {
    let mut a: Vec<String> = [
        "-nostdin",
        "-hide_banner",
        "-nostats",
        "-loglevel",
        "error",
        "-progress",
        "pipe:1",
        "-protocol_whitelist",
        "file,pipe",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    for i in inputs {
        a.push("-i".into());
        a.push(format!("file:{}", i.path.display()));
    }
    for idx in 0..inputs.len() {
        a.push("-map".into());
        a.push(idx.to_string());
    }
    a.push("-c".into());
    a.push("copy".into());
    let mp4 = output == Output::Mp4;
    if mp4 {
        if inputs.iter().any(|i| i.kind == TrackKind::Subtitle) {
            // WebVTT can't be stream-copied into MP4; mov_text is its MP4 form.
            a.push("-c:s".into());
            a.push("mov_text".into());
        }
        let audio: Vec<&MuxInput> = inputs
            .iter()
            .filter(|i| i.kind == TrackKind::Audio)
            .collect();
        if !audio.is_empty() && audio.iter().all(|i| i.adts) {
            a.push("-bsf:a".into());
            a.push("aac_adtstoasc".into());
        }
        a.push("-movflags".into());
        a.push("+faststart".into());
        a.push("-f".into());
        a.push("mp4".into());
    } else {
        a.push("-f".into());
        a.push("matroska".into());
    }
    let (mut ai, mut si) = (0usize, 0usize);
    for i in inputs {
        match i.kind {
            TrackKind::Audio => {
                if let Some(l) = i.lang.as_deref().and_then(iso639_2) {
                    a.push(format!("-metadata:s:a:{ai}"));
                    a.push(format!("language={l}"));
                }
                ai += 1;
            }
            TrackKind::Subtitle => {
                if let Some(l) = i.lang.as_deref().and_then(iso639_2) {
                    a.push(format!("-metadata:s:s:{si}"));
                    a.push(format!("language={l}"));
                }
                si += 1;
            }
            TrackKind::Video => {}
        }
    }
    a.push(format!("file:{}", out.display()));
    a
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(kind: TrackKind, lang: Option<&str>, adts: bool) -> MuxInput {
        MuxInput {
            path: PathBuf::from(format!("/w/{kind:?}.bin")),
            kind,
            lang: lang.map(str::to_string),
            adts,
        }
    }

    #[test]
    fn mp4_mux_table() {
        let inputs = [
            input(TrackKind::Video, None, false),
            input(TrackKind::Audio, Some("en"), true),
            input(TrackKind::Subtitle, Some("de"), false),
        ];
        let a = mux_args(&inputs, Output::Mp4, Path::new("/w/out.mp4"));
        let joined = a.join(" ");
        assert!(
            joined.contains("-i file:/w/Video.bin -i file:/w/Audio.bin -i file:/w/Subtitle.bin")
        );
        assert!(joined.contains("-map 0 -map 1 -map 2 -c copy"));
        assert!(joined.contains("-c:s mov_text"));
        assert!(joined.contains("-bsf:a aac_adtstoasc"));
        assert!(
            joined.contains("-f mp4 -movflags +faststart")
                || joined.contains("-movflags +faststart -f mp4")
        );
        assert!(joined.contains("-metadata:s:a:0 language=en"));
        assert!(joined.contains("-metadata:s:s:0 language=de"));
        assert!(joined.contains("-protocol_whitelist file,pipe"));
        assert!(joined.ends_with("file:/w/out.mp4"));
    }

    #[test]
    fn mkv_keeps_subtitles_and_skips_the_mp4_fixes() {
        let inputs = [
            input(TrackKind::Video, None, false),
            input(TrackKind::Audio, None, false),
            input(TrackKind::Subtitle, None, false),
        ];
        let joined = mux_args(&inputs, Output::Mkv, Path::new("/w/o.mkv")).join(" ");
        assert!(joined.contains("-f matroska"));
        assert!(!joined.contains("mov_text"));
        assert!(!joined.contains("aac_adtstoasc"));
        assert!(!joined.contains("-movflags"));
    }

    #[test]
    fn the_bsf_needs_every_audio_input_to_be_adts() {
        let inputs = [
            input(TrackKind::Video, None, false),
            input(TrackKind::Audio, None, true),
            input(TrackKind::Audio, None, false),
        ];
        assert!(
            !mux_args(&inputs, Output::Mp4, Path::new("/w/o.mp4"))
                .join(" ")
                .contains("aac_adtstoasc")
        );
    }

    #[test]
    fn a_join_writes_init_then_pieces() {
        use kstar_media::{Object, Protection, Segment, SegmentPlan, TrackId};
        let dir = std::env::temp_dir().join(format!("kstar-join-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let track = Track {
            id: TrackId::of(&["j"]),
            kind: TrackKind::Video,
            lang: None,
            name: None,
            codecs: None,
            container: SegmentContainer::Fmp4,
            bandwidth: None,
            resolution: None,
            channels: None,
            is_default: false,
            protection: Protection::Clear,
            plan: SegmentPlan {
                inits: vec![Object {
                    url: url::Url::parse("https://x/i").expect("URL"),
                    range: None,
                }],
                segments: (0..3)
                    .map(|i| Segment {
                        seq: i,
                        object: Object {
                            url: url::Url::parse("https://x/s").expect("URL"),
                            range: None,
                        },
                        init: Some(0),
                        key: None,
                        discontinuity: 0,
                        start: 0.0,
                        duration: 1.0,
                    })
                    .collect(),
                whole: false,
            },
        };
        std::fs::create_dir_all(track_dir(&dir, &track)).expect("dirs");
        std::fs::write(init_path(&dir, &track, 0), b"INIT").expect("init");
        for i in 0..3 {
            std::fs::write(piece_path(&dir, &track, i), format!("p{i}")).expect("piece");
        }
        let out = join_track(&dir, &track).expect("joins");
        assert_eq!(std::fs::read(&out).expect("read"), b"INITp0p1p2");
        std::fs::remove_file(piece_path(&dir, &track, 1)).expect("remove");
        assert!(matches!(
            join_track(&dir, &track),
            Err(Error::Media(MediaError::SegmentInvalid))
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
