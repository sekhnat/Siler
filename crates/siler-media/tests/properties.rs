//! Property tests: parsed plans agree with what the playlists say, whatever the values.

use proptest::prelude::*;
use std::collections::HashMap;
use url::Url;

use siler_media::hls::{self, Master, MasterVariant};
use siler_media::{ParseLimits, TrackId, dash};

fn base() -> Url {
    Url::parse("https://cdn.example/show/master.m3u8").expect("URL")
}

proptest! {
    /// A media playlist round-trips: every segment comes back with its duration, the starts
    /// add up, and the sequence numbers run on from the media sequence.
    #[test]
    fn media_playlists_round_trip(
        first in 0u64..10_000,
        durations in prop::collection::vec(0.5f64..12.0, 1..60),
    ) {
        let mut text = format!("#EXTM3U\n#EXT-X-MEDIA-SEQUENCE:{first}\n");
        for (i, d) in durations.iter().enumerate() {
            text.push_str(&format!("#EXTINF:{d:.3},\nseg{i}.ts\n"));
        }
        text.push_str("#EXT-X-ENDLIST\n");
        let m = hls::parse_media(text.as_bytes(), &base(), &ParseLimits::default()).expect("parses");
        prop_assert_eq!(m.segments.len(), durations.len());
        let mut start = 0.0f64;
        for (i, seg) in m.segments.iter().enumerate() {
            prop_assert_eq!(seg.seq, first + i as u64);
            prop_assert!((seg.start - start).abs() < 1e-6);
            prop_assert!((seg.duration - (durations[i] * 1000.0).round() / 1000.0).abs() < 1e-6);
            start += seg.duration;
        }
        prop_assert!(m.vod);
    }

    /// A DASH timeline with explicit repeat counts yields exactly the segments the counts
    /// describe, in order, with the durations of their entries.
    #[test]
    fn timelines_expand_to_their_counts(
        entries in prop::collection::vec((1u64..5_000, 0u32..6), 1..12),
    ) {
        let mut s = String::new();
        for (d, r) in &entries {
            s.push_str(&format!(r#"<S d="{d}" r="{r}"/>"#));
        }
        let mpd = format!(
            r#"<MPD type="static"><Period><AdaptationSet mimeType="audio/mp4" codecs="mp4a.40.2"><SegmentTemplate timescale="1000" media="a$Number$.m4s" initialization="i.mp4"><SegmentTimeline>{s}</SegmentTimeline></SegmentTemplate><Representation id="1" bandwidth="9"/></AdaptationSet></Period></MPD>"#
        );
        let p = dash::parse(mpd.as_bytes(), &base(), &ParseLimits::default()).expect("parses");
        let segs = &p.audio[0].plan.segments;
        let expected: u64 = entries.iter().map(|(_, r)| u64::from(*r) + 1).sum();
        prop_assert_eq!(segs.len() as u64, expected);
        let mut want = Vec::new();
        for (d, r) in &entries {
            for _ in 0..=*r {
                want.push(*d as f64 / 1000.0);
            }
        }
        for (seg, d) in segs.iter().zip(want) {
            prop_assert!((seg.duration - d).abs() < 1e-9);
        }
        // Numbers are consecutive from 1.
        for (i, seg) in segs.iter().enumerate() {
            prop_assert_eq!(seg.object.url.as_str().ends_with(&format!("a{}.m4s", i + 1)), true);
        }
    }

    /// A track's identity ignores its query string, so a signed URL that rotates keeps the
    /// track (and so a saved selection) the same, while a different bandwidth is another track.
    #[test]
    fn track_ids_ignore_query_strings(token in "[a-zA-Z0-9]{1,24}", bandwidth in 1u64..50_000_000) {
        let variant = |query: &str, bw: u64| -> Master {
            Master {
                variants: vec![MasterVariant {
                    uri: Url::parse(&format!("https://cdn.example/v/index.m3u8?{query}")).expect("URL"),
                    bandwidth: bw,
                    codecs: Some("avc1.4d401f".into()),
                    resolution: Some((640, 360)),
                    audio_group: None,
                    subtitle_group: None,
                }],
                renditions: vec![],
                session_keys: vec![],
            }
        };
        let media = hls::parse_media(
            b"#EXTM3U\n#EXTINF:4,\na.ts\n#EXT-X-ENDLIST\n",
            &base(),
            &ParseLimits::default(),
        )
        .expect("parses");
        let lists_for = |m: &Master| {
            let mut lists = HashMap::new();
            for v in &m.variants {
                lists.insert(v.uri.clone(), media.clone());
            }
            lists
        };
        let a = variant(&format!("token={token}"), bandwidth);
        let b = variant("token=other", bandwidth);
        let pa = hls::assemble(&a, &lists_for(&a), &ParseLimits::default()).expect("assembles");
        let pb = hls::assemble(&b, &lists_for(&b), &ParseLimits::default()).expect("assembles");
        prop_assert_eq!(&pa.variants[0].video.id, &pb.variants[0].video.id);
        let c = variant("token=x", bandwidth + 1);
        let pc = hls::assemble(&c, &lists_for(&c), &ParseLimits::default()).expect("assembles");
        prop_assert_ne!(&pa.variants[0].video.id, &pc.variants[0].video.id);
        prop_assert!(TrackId::parse(pa.variants[0].video.id.as_str()).is_some());
    }
}
