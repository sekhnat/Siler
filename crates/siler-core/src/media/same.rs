//! Whether a stream at fresh addresses is the stream the finished pieces came from.
//!
//! Fresh links often change the host or the path (a token in it), and with that the ids
//! made from addresses and the fingerprint. So the tracks are found again by what they are,
//! and what is on disk is kept only when pieces fetched again are the same bytes.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::Path;

use siler_media::select::{self, Selection};
use siler_media::{Presentation, Track, TrackId};

use super::reconstruct;
use super::run::{Fetch, Job};
use crate::error::MediaError;
use crate::{Error, Result, http};

/// Compared at each end of a track that is one file.
const SAMPLE: u64 = 64 * 1024;

/// One chosen track, by what it is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Picked {
    id: TrackId,
    shape: String,
    /// Which of the tracks with that shape (a backup copy has the same one).
    nth: usize,
}

fn shape(t: &Track) -> String {
    format!(
        "{:?}|{:?}|{:?}|{}|{}|{}",
        t.kind,
        t.bandwidth,
        t.resolution,
        t.codecs.as_deref().unwrap_or(""),
        t.lang.as_deref().unwrap_or(""),
        t.name.as_deref().unwrap_or(""),
    )
}

fn offered(pres: &Presentation) -> impl Iterator<Item = &Track> {
    pres.variants
        .iter()
        .map(|v| &v.video)
        .chain(pres.audio.iter())
        .chain(pres.subtitles.iter())
}

/// What the selection's tracks are, to find them again.
pub(crate) fn picked(pres: &Presentation, sel: &Selection) -> Vec<Picked> {
    let Ok(resolved) = select::resolve(pres, sel) else {
        return Vec::new();
    };
    resolved
        .tracks()
        .into_iter()
        .map(|t| {
            let shape_of_it = shape(t);
            let nth = offered(pres)
                .take_while(|o| o.id != t.id)
                .filter(|o| shape(o) == shape_of_it)
                .count();
            Picked {
                id: t.id.clone(),
                shape: shape_of_it,
                nth,
            }
        })
        .collect()
}

/// Gives the tracks of a re-read stream the ids they were picked under, where those are gone.
pub(crate) fn adopt(pres: &mut Presentation, picked: &[Picked]) {
    let kept: HashSet<&TrackId> = picked.iter().map(|p| &p.id).collect();
    for p in picked {
        if pres.track(&p.id).is_some() {
            continue;
        }
        let found = offered(pres)
            .filter(|t| shape(t) == p.shape)
            .nth(p.nth)
            .filter(|t| !kept.contains(&t.id))
            .map(|t| t.id.clone());
        let Some(now) = found else { continue };
        let rename = |id: &mut TrackId| {
            if *id == now {
                *id = p.id.clone();
            }
        };
        for v in &mut pres.variants {
            rename(&mut v.video.id);
            v.audio.iter_mut().for_each(rename);
            v.subtitles.iter_mut().for_each(rename);
        }
        for t in pres.audio.iter_mut().chain(pres.subtitles.iter_mut()) {
            rename(&mut t.id);
        }
    }
}

fn changed() -> Error {
    Error::Media(MediaError::ManifestChanged)
}

/// The numbers of a track's finished pieces and initialisation segments.
fn finished(dir: &Path) -> Result<(Vec<usize>, Vec<usize>)> {
    let (mut pieces, mut inits) = (Vec::new(), Vec::new());
    for entry in std::fs::read_dir(dir)? {
        let name = entry?.file_name();
        let Some(name) = name.to_str() else { continue };
        if let Some(i) = name.strip_suffix(".seg").and_then(|n| n.parse().ok()) {
            pieces.push(i);
        } else if let Some(i) = name
            .strip_prefix("init-")
            .and_then(|n| n.strip_suffix(".bin"))
            .and_then(|n| n.parse().ok())
        {
            inits.push(i);
        }
    }
    pieces.sort_unstable();
    inits.sort_unstable();
    Ok((pieces, inits))
}

/// Fails unless what is on disk is this stream's too: the first and the last finished piece
/// of each track are fetched again and compared.
pub(super) async fn verify(
    c: &Fetch,
    pres: &Presentation,
    sel: &Selection,
    work: &Path,
) -> Result<()> {
    let resolved =
        select::resolve(pres, sel).map_err(|_| Error::Media(MediaError::SelectionUnavailable))?;
    let scratch = work.join("compare.tmp");
    for t in resolved.tracks() {
        let dir = reconstruct::track_dir(work, t);
        if !dir.exists() {
            continue;
        }
        if reconstruct::joined_path(work, t).exists() {
            // Its pieces are gone, so there is nothing to compare.
            return Err(changed());
        }
        let (pieces, inits) = finished(&dir)?;
        let whole = reconstruct::whole_path(work, t);
        let whole = [whole.clone(), http::part_of(&whole)]
            .into_iter()
            .find(|p| p.exists());
        if t.plan.whole {
            if !pieces.is_empty() {
                return Err(changed());
            }
            if let (Some(file), Some(object)) = (whole, t.plan.segments.first()) {
                compare_whole(c, &file, &object.object.url, &scratch).await?;
            }
            continue;
        }
        if whole.is_some()
            || pieces.iter().any(|&i| i >= t.plan.segments.len())
            || inits.iter().any(|&i| i >= t.plan.inits.len())
        {
            return Err(changed());
        }
        let mut again: Vec<Job> = Vec::new();
        if let Some(&i) = inits.first() {
            again.push(Job::init(work, t, i));
        }
        if let (Some(&first), Some(&last)) = (pieces.first(), pieces.last()) {
            again.push(Job::piece(work, t, first));
            if last != first {
                again.push(Job::piece(work, t, last));
            }
        }
        for job in again {
            let there = c.fetch_again(&job, &scratch).await?;
            if tokio::fs::read(&job.dest).await? != there {
                return Err(changed());
            }
        }
    }
    Ok(())
}

/// Both ends of what a whole-file track has on disk.
async fn compare_whole(c: &Fetch, file: &Path, url: &url::Url, scratch: &Path) -> Result<()> {
    use std::os::unix::fs::FileExt;
    let len = std::fs::metadata(file)?.len();
    if len == 0 {
        return Ok(());
    }
    let n = SAMPLE.min(len);
    let mut starts = vec![0];
    if len > n {
        starts.push(len - n);
    }
    for at in starts {
        let mut here = vec![0u8; n as usize];
        std::fs::File::open(file)?.read_exact_at(&mut here, at)?;
        let there = match c.fetch_range_again(url, (at, at + n - 1), scratch).await {
            // Shorter than what is on disk.
            Err(Error::Http(416)) => return Err(changed()),
            r => r?,
        };
        if there != here {
            return Err(changed());
        }
    }
    Ok(())
}
