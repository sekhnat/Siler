# Video and audio streams

Siler downloads HLS (`.m3u8`), DASH (`.mpd`) and Adobe HDS (`.f4m`) streams into one ordinary video file, with the quality, audio languages and subtitles you pick. It records live streams, including RTMP and MMS, until you press **Finish**.

<p align="center">
  <picture><source media="(prefers-color-scheme: dark)" srcset="images/add-video-dark.png"><source media="(prefers-color-scheme: light)" srcset="images/add-video-light.png"><img src="images/add-video-light.png" alt="The Add window listing a stream's video qualities, audio languages and subtitles" width="480"></picture>
</p>

## Adding a stream

- Paste a stream's address into the Add window, give it on the command line, or copy it (the clipboard watcher offers it).
- From a web page, the browser extension finds the streams and files a page plays and hands them over with the page's cookies. See [Videos on web pages](browser-extension.md#videos-on-web-pages).
- Siler lists the video qualities, audio languages and subtitles, with the size each choice will take, and downloads the ones you pick into one file.
- Settings → Media sets the defaults: the highest quality to pick, preferred audio and subtitle languages, the container and where subtitles go.

## Live streams

- A live HLS or DASH stream (no `#EXT-X-ENDLIST`, or `type="dynamic"`) is recorded: the Add window says so, the list shows **Recording · time · size** with a LIVE badge, and **Finish** (beside Pause, in the detail panel and the right-click menu) ends the recording and builds the file from what was recorded.
- Recording starts at the live edge (the last three segments the playlist lists). The playlist is read again about once a segment; each new segment is kept once, by its media sequence (HLS) or `$Number$` (DASH, with `SegmentTimeline` or a segment duration; `availabilityStartTime` and `timeShiftBufferDepth` decide what is available now).
- A recording that falls behind the live edge (on a slow connection) fetches up to four segments at once to catch up. **Finish** doesn't wait for segments still arriving.
- The recording ends by itself when the stream does (`#EXT-X-ENDLIST`, or the MPD becomes static).
- **Pause**, or quitting Siler, keeps what was recorded. Resuming continues from the live edge; what was missed meanwhile counts as a **gap** (shown in the detail panel). A segment that can't be fetched after a few tries is skipped and counted the same way.
- Subtitles of live streams aren't recorded. Live HDS isn't supported.

### RTMP and MMS

- `rtmp://`, `rtmpt://`, `mms://`, `mmsh://` and `mmst://` addresses are recorded by FFmpeg, which copies the stream into Matroska until **Finish** (FFmpeg is asked to stop, so the file is closed properly) or the stream ends. `mms://` tries MMS over HTTP, then over TCP.
- The recording becomes MP4 (RTMP, usually H.264 and AAC) or stays Matroska (MMS, Windows Media), as the container setting says; if MP4 can't hold its codecs, it stays Matroska.
- A recording can't continue where it stopped: pausing finishes it.
- Siler looks the server's name up itself and gives FFmpeg the address; the same address check as for other streams applies. Encrypted RTMP (`rtmps`, `rtmpe`) isn't supported. These connections don't go through Siler's proxy.
- It needs an FFmpeg with network support; Siler's own build has it (Settings → Media shows whether yours does).

## Adobe HDS

An `.f4m` manifest lists its bitrates and a bootstrap (inline or by address) that numbers the fragments (`…Seg1-Frag1`). Siler fetches the fragments like any stream's pieces, takes the FLV tags out of each one, and joins them after an FLV header into one file that FFmpeg copies into MP4 or MKV. Media with Adobe Access DRM (`drmAdditionalHeader`) is refused.

## Playlists

Plain M3U (without HLS tags), PLS and ASX files are lists of links, not streams. The Add window shows their entries and **Review the links…** opens them in the batch window, where each becomes its own download (or recording, for RTMP and MMS entries).

## FFmpeg

- Streams are muxed with **FFmpeg**, with no re-encoding. Siler ships a static build made from a pinned release by `scripts/build-ffmpeg.sh`, with only the codecs, containers and filters it needs, plus the network protocols for RTMP and MMS recording (`tcp`, `http`, `rtmp`, `rtmpt`, `mmsh`, `mmst`; no TLS).
- `scripts/install-dev.sh` builds it the first time and puts `ffmpeg` and `ffprobe` next to the app. The AppImage and the release tarball include it.
- Without FFmpeg, single-track downloads still work; combining video and audio needs it.
- Settings → Media shows which FFmpeg is in use. `SILER_FFMPEG=/path/to/ffmpeg` overrides it.
- The bundled build enables nothing that needs `--enable-gpl`, so it is LGPL-2.1-or-later. It runs as a separate program. The build checks the release tarball against a pinned SHA-256; its PGP signature can be checked by hand.

## Output

- Output is MP4 when every codec fits it, and MKV otherwise (Opus audio, VP9, unknown codecs, or subtitles kept inside the file). The Add window says why.
- Subtitles go inside the file by default, or beside it as `<name>.<language>.vtt`.
- WebVTT and TTML subtitles are read from plain files and from inside MP4 (`wvtt`, `stpp`, which Siler reads itself because FFmpeg can't), and timed to the video. No cue runs past the end of the picture and sound, so the subtitles never make the file longer.
- AES-128 streams are decrypted as they download, including encrypted initialisation segments (`EXT-X-MAP` under an `EXT-X-KEY`). Each key is fetched once and kept only in memory.

## How a stream downloads

- The manifest is read again at every attempt, so expired signed links are replaced by the playlist's own refresh.
- When the manifest's own address has expired (YouTube's links, or a CDN's tokenised paths), Siler gets a fresh one from the page the stream came from ([Fresh links](browser-extension.md#fresh-links)). Since the new pieces have other addresses, it fetches the first and last finished piece of each track again and keeps the work only if they are identical.
- Downloaded pieces are kept in a hidden work folder (`.<name>.siler.media`) beside the output until it is published, and never fetched twice.
- FFmpeg reads each track's pieces in order through a named pipe (under `$XDG_RUNTIME_DIR/siler/media/`), so no joined copy is written. If a track can't be read from a pipe (an MP4 with its index at the end), Siler joins it into one file and runs FFmpeg again.
- The pieces stay until the output is written, so a download needs about twice the stream's size on disk at the end.

## Safety

- A manifest's own links must be `http(s)`, and by default may not point at this computer or the local network. Settings → Media can allow it, for a media server at home.
- Names are checked in the same lookup the connection uses, so a name can't pass the check with a public address and then connect to a private one (DNS rebinding). A proxy on this computer still works.
- Redirects are followed one hop at a time, and each hop is checked the same way.
- The cookies and headers the browser sent go only to the stream's own site, not to other servers its playlist names.
- DRM-protected streams are refused, and Siler never contacts a license server.

## Not supported yet

Live HDS, encrypted RTMP, recording a live stream from the start of its time-shift window, SAMPLE-AES, and subtitles carried inside WebM.

## Testing

See [Development → Media tests](development.md#media-tests) for the fixture streams and the optional test against real CDNs.
