# Siler's interface, in English (en-US): the source every translation starts from.
# Format: Fluent (https://projectfluent.org). See docs/translating.md for adding a language.
# The same file serves the window (Svelte) and the tray, notifications and errors (Rust).
# Ids are grouped by where they appear; `node app/locales/check.mjs` checks them.

## countdown

countdown-cancel = Cancel

## main

main-drop-links-to-download-them = Drop links to download them
main-a-list-of-links = A list of links
main-one-address-per-line-any-download = One address per line; any download manager reads it
main-siler-list = Siler list
main-also-names-folders-queues-checksums-and = Also names, folders, queues, checksums and mirrors
main-include-cookies = Include cookies
main-they-can-log-someone-else-in = — they can log someone else in to your accounts; keep the file private
main-cancel = Cancel
main-export = Export…
main-what-was-downloaded-so-far-is =
    { $count ->
        [one] What was downloaded so far is discarded and the file is downloaded again from the start.
       *[other] What was downloaded so far is discarded and the files are downloaded again from the start.
    }
main-download-again = Download “{ $v }” again?
main-the-file-was-moved-or-deleted = The file was moved or deleted. Siler downloads it again into { $dir }.
main-the-file-was-moved-or-deleted-its-folder = The file was moved or deleted. Siler downloads it again into its folder.
main-the-file-is-downloaded-again-from = The file is downloaded again from { $url }.
main-the-file-is-downloaded-again-from-its-address = The file is downloaded again from its address.
main-replace-the-existing-file-when-done = Replace the existing file when done (otherwise both are kept)
main-remove-failed =
    { $count ->
        [one] Remove { $count } failed download?
       *[other] Remove { $count } failed downloads?
    }
main-they-are-removed-from-the-list = They are removed from the list and their unfinished data is discarded.
main-quit-siler = Quit Siler?
main-still-running-now-and-the-next =
    { $count ->
        [one] A download is still running. It pauses now and continues the next time Siler starts.
       *[other] { $count } downloads are still running. They pause now and continue the next time Siler starts.
    }
main-still-running-resume-by-hand =
    { $count ->
        [one] A download is still running. It pauses now and can be resumed the next time Siler starts.
       *[other] { $count } downloads are still running. They pause now and can be resumed the next time Siler starts.
    }
main-don-t-ask-again = Don't ask again
main-keep-up-to-date = Keep up to date
main-siler-asks-the-server-whether-changed =
    { $count ->
        [one] Siler asks the server whether the file has changed and downloads a newer version over the old one. Checks run while Siler is running.
       *[other] Siler asks the server whether these files have changed and downloads a newer version over the old one. Checks run while Siler is running.
    }
main-last-checked = Last checked { $v }
main-check-now = Check now

## add

add-new-download = New download
add-from-the-command-line = From the command line
add-copied-to-the-clipboard = Copied to the clipboard
add-from-a-list-or-metalink = From a list or metalink
add-found-by-the-site-grabber = Found by the site grabber
add-from = From { $source }
add-from-the-browser = From the browser
add-form-submission = {" · "}form submission
add-paste-one-or-more-links = Paste one or more links
add-looks-like = Looks like
add-done = ({ $v }% done)
add-use-this-new-link-to-continue = Use this new link to continue it instead of starting over.
add-continue-it = Continue it
add-address = Address
add-a-playlist-of-link =
    { $count ->
        [one] A playlist of { $count } link
       *[other] A playlist of { $count } links
    }
add-review-the-links = Review the links…
add-and-more = …and { $v } more
add-checking-link = Checking link…
add-this-is-a-video-stream-and = This is a video stream, and it couldn't be read. Check the link, then try again.
add-you-can-still-add-it-siler = You can still add it — Siler will retry when it starts.
add-resumable = Resumable
add-can-t-resume = Can't resume
add-file-details-appear-here-once-the = File details appear here once the link is checked.
add-see-what-s-inside-and-pick = See what's inside and pick files
add-preview-contents = Preview contents
add-file-name = File name
add-save-to = Save to
add-video-folder = Video folder
add-choose-folder = Choose folder
add-detected-automatically = Detected automatically
add-category = Category
add-category-folder = Category folder
add-a-file-named-is-already-in = A file named “{ $v }” is already in this folder.
add-keep-both = Keep both
add-replace-it-when-done = Replace it when done
add-the-new-file-is-saved-as = The new file is saved as “{ $suggested }”.
add-the-old-file-stays-until-the = The old file stays until the new one is complete.
add-advanced = Advanced
add-connections = Connections
add-checksum = Checksum
add-md5-sha-1-sha-256-or = (MD5, SHA-1, SHA-256 or SHA-512; checked when the file is done)
add-mirrors = Mirrors
add-other-addresses-of-the-same-file = (other addresses of the same file, one per line; fetched from at the same time)
add-isn-t-a-link = “{ $v }” isn't a link
add-cancel = Cancel
add-download-later = Download later
add-start-download = Start download
add-start-recording = Start recording

## batch

batch-download-files = Download { $length } files
batch-found-by-the-site-grabber = Found by the site grabber.
batch-from-a-list-or-metalink = From a list or metalink.
batch-choose-which-files-siler-should-download = Choose which files Siler should download.
batch-filter-by-name-or-address = Filter by name or address
batch-clear = Clear
batch-all = All
batch-select-all-shown = Select all shown
batch-of-selected = { $count } of { $length } selected
batch-select-shown = Select shown
batch-clear-shown = Clear shown
batch-checked-against = Checked against { $checksum }
batch-nothing-matches-the-filter = Nothing matches the filter.
batch-cancel = Cancel
batch-if-a-file-exists = If a file exists
batch-keep-both = Keep both
batch-replace-it = Replace it
batch-skip-it = Skip it
batch-download-later = Download later
batch-download = Download { $count }
batch-pattern-numbered-batch-download = Numbered batch download
batch-pattern-write-the-address-with-a-range = Write the address with a range where the number or letter changes, like
batch-pattern-goes-through-letters = goes through letters,
batch-pattern-counts-in-fives-and = counts in fives, and
batch-pattern-tries-each-word = tries each word.
batch-pattern-address-pattern = Address pattern
batch-pattern-or-build-a-range = Or build a range
batch-pattern-numbers = Numbers
batch-pattern-letters = Letters
batch-pattern-from = From
batch-pattern-to = to
batch-pattern-to-2 = To
batch-pattern-digits = Digits
batch-pattern-put-into-the-address-where-is = Put { $range } into the address (where * is, or before the extension)
batch-pattern-insert = Insert { $range }
batch-pattern-the-first-and-last-address-appear = The first and last address appear here.
batch-pattern-cancel = Cancel
batch-pattern-review-files = Review { $v } files

## category

category-editor-files-are-sorted-by-extension-each = Files are sorted by extension. Each category can have its own folder.
category-editor-reset-all = Reset all
category-editor-choose-folder = Choose folder
category-editor-use-the-default-folder = Use the default folder
category-editor-everything-that-doesn-t-match-another = Everything that doesn't match another category.
category-editor-remove = Remove .{ $ext }
category-editor-add = add…

## confirm

confirm-cancel = Cancel
confirm-remove-remove = Remove { $label }?
confirm-remove-the-download-will-be-removed-from = The download will be removed from the list.
confirm-remove-unfinished-data-is-discarded = Unfinished data is discarded.
confirm-remove-also-delete-the-downloaded-from-disk =
    { $count ->
        [one] Also delete the downloaded file from disk
       *[other] Also delete the downloaded files from disk
    }
confirm-remove-cancel = Cancel
confirm-remove-remove-2 = Remove

## detail

detail-panel-close-details = Close details
detail-panel-of = of { $v }
detail-panel-recorded = recorded
detail-panel-left = { $v } · { $v2 } left
detail-panel-the-file-was-moved-or-deleted = The file was moved or deleted. Download it again, or remove it from the list.
detail-panel-expected = Expected { $expected }
detail-panel-got = Got { $actual }
detail-panel-download-again = Download again
detail-panel-keep-it-anyway = Keep it anyway
detail-panel-connections = Connections
detail-panel-active-of = { $active_connections } active of
detail-panel-single-connection = Single connection
detail-panel-fewer-connections = Fewer connections
detail-panel-more-connections = More connections
detail-panel-sources = Sources
detail-panel-of-in-use = { $v } of { $length } in use
detail-panel-speed = Speed
detail-panel-address = Address
detail-panel-referrer = Referrer
detail-panel-saved-to = Saved to
detail-panel-resume = Resume
detail-panel-supported = Supported
detail-panel-not-supported = Not supported
detail-panel-unknown = Unknown
detail-panel-retransferred = Fetched twice
detail-panel-retransferred-value = { $bytes } ({ $times }× the file)
detail-panel-retransferred-hint = Data received and thrown away: dropped connections, a download that started over, damaged pieces fetched again. A data quota counts it too.
detail-panel-retries = Retries
detail-panel-protocol = Protocol
detail-panel-speed-limit = Speed limit
detail-panel-queue = Queue
detail-panel-kept-up-to-date = Kept up to date
detail-panel-checked = Checked { $every }
detail-panel-checked-last = Checked { $every }, last { $when }
detail-panel-stream = Stream
detail-panel-tracks = Tracks
detail-panel-subtitles = Subtitles
detail-panel-also-saved = Also saved
detail-panel-progress = Progress
detail-panel-from-archive = From archive
detail-panel-checksum = Checksum
detail-panel-matches = { $v } matches
detail-panel-doesn-t-match = { $v } doesn't match
detail-panel-checked-when-done =
    { $source ->
        [server] { $algo } from the server, checked when done
        [metalink] { $algo } from the list, checked when done
       *[other] { $algo }, checked when done
    }
detail-panel-command = Command
detail-panel-running = Running…
detail-panel-exited-with = Exited with { $exit_code }
detail-panel-added = Added
detail-panel-finished = Finished
detail-panel-open = Open
detail-panel-start = Start
detail-panel-retry = Retry
detail-panel-pause = Pause
detail-panel-stop-recording-and-keep-what-was = Stop recording and keep what was recorded
detail-panel-finish = Finish
detail-panel-show-in-folder = Show in folder
detail-panel-copy-link = Copy link
detail-panel-properties = Properties
detail-panel-remove = Remove

## details

details-list-live = LIVE
details-list-done = Done
details-list-columns = Columns

## download

download-row-the-file-was-moved-or-deleted = The file was moved or deleted
download-row-file-missing = File missing
download-row-its-matches-the-expected-checksum = Its { $v } matches the expected checksum
download-row-verified = Verified
download-row-its-doesn-t-match = Its { $algo } doesn't match
download-row-its-checksum-doesn-t-match = Its checksum doesn't match
download-row-damaged = Damaged
download-row-of-mirrors-in-use = { $usedMirrors } of { $v } mirrors in use
download-row-sources = { $v } sources
download-row-from-inside-the-archive = From inside the archive: { $zip_path }
download-row-zip = ZIP
download-row-running-the-command-after-the-download = Running the command after the download
download-row-running-command = Running command
download-row-command-failed = Command failed
download-row-live = LIVE
download-row-in-the-queue = In the queue “{ $v }”
download-row-checked-for-a-newer-version = Checked for a newer version { $every }
download-row-checked-for-a-newer-version-last = Checked for a newer version { $every } · last checked { $when }
download-row-left = { $v } left
download-row-finish-the-recording-and-keep-it = Finish the recording and keep it
download-row-pause = Pause
download-row-resume = Resume
download-row-check-the-server-s-key = Check the server's key
download-row-log-in = Log in
download-row-get-a-fresh-link = The link expired: get a fresh one
download-row-retry = Retry
download-row-show-in-folder = Show in folder

## empty

empty-state-nothing-here = Nothing here
empty-state-no-downloads-match-this-view = No downloads match this view.
empty-state-ready-when-you-are = Ready when you are
empty-state-add-a-link-paste-one-with = Add a link, paste one with
empty-state-ctrl = Ctrl
empty-state-or-click-a-download-in-the-browser = , or click a download in your browser with the Siler extension installed.
empty-state-add-url = Add URL
empty-state-paste-anywhere = Paste anywhere

## fatal

fatal-screen-siler = Siler
fatal-screen-siler-couldn-t-start = Siler couldn't start
fatal-screen-your-downloads-are-safe-on-disk = Your downloads are safe on disk. The log file has more details; check that the folder above exists and that you can write to it, then start Siler again.
fatal-screen-open-log-folder = Open log folder
fatal-screen-quit = Quit

## grabber

grabber-site-grabber = Site grabber
grabber-new-grab = New grab
grabber-saved-projects = Saved projects
grabber-start-page = Start page
grabber-project-name = Project name
grabber-named-after-the-site = Named after the site
grabber-follow-links = Follow links
grabber-start-page-only = Start page only
grabber-deep =
    { $n ->
        [one] { $n } level deep
       *[other] { $n } levels deep
    }
grabber-stay-within = Stay within
grabber-at-most = At most
grabber-pages = { $n } pages
grabber-files-to-find = Files to find
grabber-also-these-extensions-epub-srt = Also these extensions: epub, srt
grabber-at-least-mb = At least … MB
grabber-at-most-mb = At most … MB
grabber-save-the-pages-for-offline-browsing = Save the pages for offline browsing
grabber-with-their-pictures-and-styles-links = — with their pictures and styles; links point to the saved copies
grabber-take-files-from-other-servers-too = Take files from other servers too
grabber-sites-often-keep-files-elsewhere = (sites often keep files elsewhere)
grabber-leave-out-what-the-site-s = Leave out what the site's robots.txt asks crawlers to
grabber-only-files-this-project-hasn-t = Only files this project hasn't found before
grabber-save-the-project-first = (save the project first)
grabber-save-to = Save to
grabber-choose-folder = Choose folder
grabber-queue = Queue
grabber-run-on-a-schedule = Run on a schedule
grabber-found-files-go-straight-to-the = (found files go straight to the queue)
grabber-at = at
grabber-found =
    { $count ->
        [one] { $count } file found
       *[other] { $count } files found
    }
grabber-on =
    { $pages ->
        [one] on { $pages } page
       *[other] on { $pages } pages
    }
grabber-delete = Delete
grabber-stop = Stop
grabber-open-saved-site = Open saved site
grabber-run-again = Run again
grabber-review-files = Review { $length } files
grabber-close = Close
grabber-start = Start

## host

host-key-form-siler-hasn-t-connected-to = Siler hasn't connected to
host-key-form-before = before
host-key-form-make-sure-this-is-the-server = Make sure this is the server's key before trusting it, for example by asking its administrator or running
host-key-form-on-the-server = on the server.
host-key-form-key-type = Key type
host-key-form-fingerprint = Fingerprint
host-key-form-cancel = Cancel
host-key-form-trust-and-connect = Trust and connect

## login

login-trust-this-server = Trust this server?
login-log-in-to-continue = Log in to continue
login-form-asks-for-a-username-and-password = asks for a username and password
login-form-the-server-didn-t-accept-the = The server didn't accept the last username or password.
login-form-ticket-failed = The server didn't accept your Kerberos ticket, or there isn't one. Run kinit, or log in with a password.
login-form-not-encrypted = This connection isn't encrypted (http), so others on the network could misuse the login.
login-form-use-ticket = Use my Kerberos ticket for this site
login-form-use-ticket-proxy = Use my Kerberos ticket for this proxy
login-form-the-proxy = The proxy
login-form-windows-placeholder = DOMAIN\user
login-form-windows-hint = A Windows account: DOMAIN\user, or user@REALM for Kerberos.
login-form-username = Username
login-form-password = Password
login-form-remember-in-the-keyring = Remember in the keyring
login-form-cancel = Cancel
login-form-log-in = Log in

## media

media-picker-live-stream-rtmp-mms = Live stream (RTMP/MMS)
media-picker-siler-records-it-from-now-until = Siler records it from now until you press
media-picker-finish = Finish
media-picker-or-until-the-stream-ends-a = , or until the stream ends. A recording can't be paused: pausing finishes it. RTMP becomes MP4, MMS Matroska.
media-picker-track-refused =
    { $count ->
        [one] { $count } track refused.
       *[other] { $count } tracks refused.
    }
media-picker-live-stream = Live stream
media-picker-or-until-the-stream-ends-pausing = , or until the stream ends. Pausing keeps what was recorded; resuming continues from the live edge.
media-picker-about = About { $v }
media-picker-video = Video
media-picker-video-quality = Video quality
media-picker-audio = Audio
media-picker-default = default
media-picker-subtitles = Subtitles
media-picker-container = Container
media-picker-automatic = Automatic
media-picker-mp4 = MP4
media-picker-mkv = MKV
media-picker-original-format = Original format
media-picker-original-format-one-track = Original format (one track only)
media-picker-subtitles-in-the-file = Subtitles in the file
media-picker-inside-the-video = Inside the video
media-picker-as-a-separate-file = As a separate file
media-picker-leave-out = Leave out
media-picker-combining-video-and-audio-needs-ffmpeg = Combining video and audio needs FFmpeg, which Siler couldn't find. See Settings → Media.

## modal

modal-close = Close

## properties

properties-properties = Properties
properties-file-name = File name
properties-decided-when-the-download-starts = Decided when the download starts
properties-folder = Folder
properties-category-folder = Category folder
properties-choose-folder = Choose folder
properties-the-file-will-be-moved = The file will be moved.
properties-the-partial-file-will-be-moved = The partial file will be moved.
properties-the-partial-file-will-be-moved-pause = The partial file will be moved; the download pauses for a moment.
properties-address = Address
properties-refresh-link = Refresh link…
properties-connections = Connections
properties-fewer = Fewer
properties-more = More
properties-speed-limit = Speed limit
properties-checksum = Checksum
properties-md5-sha-1-sha-256-or = MD5, SHA-1, SHA-256 or SHA-512
properties-mirrors = Mirrors
properties-other-addresses-of-the-same-file = (other addresses of the same file, one per line)
properties-referrer = Referrer
properties-request = Request
properties-form-submission = { $method } form submission
properties-added = Added
properties-finished = Finished
properties-close = Close
properties-save = Save

## queue

queue-name = Name
queue-night-downloads = Night downloads
queue-downloads-at-the-same-time = Downloads at the same time
queue-the-overall-limit-in-settings-still = The overall limit in Settings still applies.
queue-start-on-a-schedule = Start on a schedule
queue-siler-must-be-running-in-the = Siler must be running (in the tray is fine); it can't wake a sleeping computer.
queue-start-at = Start at
queue-stop-at = Stop at
queue-runs-until-everything-in-the-queue = Runs until everything in the queue has finished.
queue-stops-the-next-morning-downloads-that = Stops the next morning; downloads that haven't finished wait for the next start.
queue-downloads-that-haven-t-finished-by = Downloads that haven't finished by then wait for the next start.
queue-days = Days
queue-only-once = Only once
queue-when-the-queue-is-done = When the queue is done
queue-siler-counts-down-for-a-minute = Siler counts down for a minute first, so you can cancel.
queue-delete = Delete “{ $name }”
queue-delete-2 = Delete
queue-cancel = Cancel
queue-picker-the-queue-these-downloads-go-into = The queue these downloads go into
queue-picker-queue = Queue

## quota

quota-banner-data-quota-reached = Data quota reached
quota-banner-used-wait = — { $used } of { $limit } used. Downloads wait for the next period.
quota-banner-used-wait-until = — { $used } of { $limit } used. Downloads wait until { $at }.
quota-banner-ignore-for-this-period = Ignore for this period

## refresh

refresh-link-refresh-download-link = Refresh download link
refresh-link-if-the-link-to = If the link to
refresh-link-expired-give-siler-a-new-one = expired, give Siler a new one. The { $v } already downloaded are kept.
refresh-link-new-address = New address
refresh-link-update = Update
refresh-link-or = or
refresh-link-click-the-download-link-on-the = Click the download link on the page again
refresh-link-siler-will-take-the-new-link = Siler will take the new link from the browser and continue this download. Waiting for 2 minutes.
refresh-link-get-a-fresh-link-from = Get a fresh link from { $host }
refresh-link-get-a-fresh-link-from-the-website = Get a fresh link from the website
refresh-link-opens-the-page-the-download-came = Opens the page the download came from.
refresh-link-open-page = Open page
refresh-link-cancel = Cancel

## settings

settings-settings = Settings
settings-download-folder = Download folder
settings-browse = Browse
settings-sort-into-category-folders = Sort into category folders
settings-video-music-documents-inside-the-download = Video, Music, Documents… inside the download folder
settings-when-a-file-with-the-same = When a file with the same name exists
settings-ask-me = Ask me
settings-keep-both-add-a-number = Keep both (add a number)
settings-replace-the-old-file = Replace the old file
settings-skip-the-download = Skip the download
settings-keep-running-in-the-tray = Keep running in the tray
settings-no-system-tray-was-found-so = No system tray was found, so closing the window quits Siler
settings-closing-the-window-keeps-downloads-going = Closing the window keeps downloads going
settings-start-when-you-log-in = Start when you log in
settings-opens-the-window-when-you-log = Opens the window when you log in
settings-opens-in-the-tray-ready-for = Opens in the tray, ready for downloads
settings-ask-before-quitting = Ask before quitting
settings-confirm-when-quitting-would-pause-running = Confirm when quitting would pause running downloads
settings-notifications = Notifications
settings-notify-when-a-download-finishes-or = Notify when a download finishes or fails
settings-resume-on-startup = Resume on startup
settings-continue-unfinished-downloads-when-siler-starts = Continue unfinished downloads when Siler starts
settings-keep-the-computer-awake = Keep the computer awake
settings-don-t-let-it-go-to = Don't let it go to sleep while downloads are running
settings-watch-the-clipboard-for-links = Watch the clipboard for links
settings-copying-a-link-to-a-file = Copying a link to a file (one of the types in Categories) offers to download it
settings-updates = Updates
settings-siler = Siler { $version }
settings-siler-packaged = Siler { $version } (installed by your package manager)
settings-check-for-updates-every-day = Check for updates every day
settings-asks-github-whether-a-newer-siler = Asks GitHub whether a newer Siler is out; nothing else is sent
settings-logs = Logs: { $dir }
settings-open-log-folder = Open log folder
settings-ffmpeg = FFmpeg
settings-checking = Checking…
settings-at = Found at { $path }
settings-version-at = Version { $version } at { $path }
settings-ffmpeg-isn-t-usable-single-track = FFmpeg isn't usable: { $missing }. Single-track downloads still work; combining video and audio needs it.
settings-ffmpeg-not-found = FFmpeg isn't usable: not found. Single-track downloads still work; combining video and audio needs it.
settings-build-the-bundled-copy-with = Build the bundled copy with
settings-pieces-fetched-at-once = Pieces fetched at once
settings-segments-of-one-stream-downloaded-in = Segments of one stream downloaded in parallel. The connection limits still apply.
settings-highest-video-quality-by-default = Highest video quality by default
settings-no-limit = No limit
settings-preferred-audio-language = Preferred audio language
settings-en = en
settings-preferred-subtitle-language = Preferred subtitle language
settings-none = none
settings-default-container = Default container
settings-automatic-mp4-or-mkv-when-needed = Automatic (MP4, or MKV when needed)
settings-mp4 = MP4
settings-mkv = MKV
settings-subtitles-by-default = Subtitles by default
settings-inside-the-video = Inside the video
settings-as-a-separate-file = As a separate file
settings-leave-out = Leave out
settings-allow-streams-from-this-computer-or = Allow streams from this computer or the local network
settings-off-by-default-so-a-playlist = Off by default, so a playlist can't make Siler contact other machines. Turn it on for a media server at home.
settings-connections-per-download = Connections per download
settings-more-connections-help-on-fast-links = More connections help on fast links and slow servers. 8 is a good default.
settings-simultaneous-downloads = Simultaneous downloads
settings-connections-per-server = Connections per server
settings-http2-connections = HTTP/2 connections per server (experimental)
settings-http2-off = Off
settings-http2-connections-hint = A download's connections to an https server share this many HTTP/2 connections instead of each opening its own. Off uses HTTP/1.1, as before.
settings-extra-safe-resume = Extra-safe resume (slower)
settings-extra-safe-resume-hint = Writes downloaded data to the disk before saving progress, so a power cut or a crash of the whole computer can't leave a resumed download with gaps. Siler itself crashing is covered either way.
settings-shared-by-all-downloads-from-one = Shared by all downloads from one server, so busy servers don't refuse or ban you.
settings-example-com = example.com
settings-connections = Connections
settings-remove = Remove
settings-different-limit-for-a-server = Different limit for a server
settings-global-speed-limit = Global speed limit
settings-retries-per-connection = Retries per connection
settings-user-agent = User agent
settings-used-for-links-added-by-hand = Used for links added by hand. Browser downloads use the browser's own user agent.
settings-auto-refresh-links = Refresh expired links automatically
settings-auto-refresh-links-hint = Siler asks the original address again. If that fails, the browser the link came from looks at its page in a background tab, without clicking anything.
settings-data-quota = Data quota
settings-download-at-most-so-much-in = Download at most so much in each period; downloads wait for the next one. Useful on metered connections.
settings-at-most = At most
settings-mb-every = MB every
settings-run-a-command-after-each-download = Run a command after each download
settings-for-example-to-unpack-scan-or = For example to unpack, scan or move files. It runs without a shell, one download at a time.
settings-the-file = the file ·
settings-its-folder = its folder ·
settings-its-name = its name ·
settings-its-address = its address ·
settings-bytes-quote-words-with-spaces-use = bytes. Quote words with spaces; use
settings-for-a-shell = for a shell.
settings-only-for = Only for
settings-every-download = (every download)
settings-notify-when-it-fails = Notify when it fails
settings-open-metalinks = Open metalinks
settings-a-downloaded-meta4-or-metalink-file = A downloaded .meta4 or .metalink file is replaced by the files it lists, with their mirrors and checksums.
settings-proxy = Proxy
settings-type = Type
settings-http = HTTP
settings-socks5 = SOCKS5
settings-https = HTTPS
settings-socks4 = SOCKS4
settings-socks4a = SOCKS4a
settings-proxy-script = Script
settings-proxy-detect = Detect
settings-ftp-proxy = FTP proxy
settings-ftp-proxy-hint = For ftp:// downloads, with a proxy that takes user@server as the username
settings-script-address = Proxy script (PAC)
settings-script-placeholder = http://config.example.com/proxy.pac
settings-script-hint = An address or a file. If the script fails, downloads stop instead of going around it.
settings-detect-hint = Siler asks the network (WPAD) for a proxy script. Without one it connects directly. Use this only on networks you trust.
settings-proxy-test = Test with an address
settings-proxy-test-placeholder = https://example.com/file.zip
settings-proxy-test-run = Test
settings-proxy-direct = Direct
settings-proxy-why-none = No proxy is set for it.
settings-proxy-why-settings = From these settings.
settings-proxy-why-system = From the system's settings ({ $source }).
settings-proxy-why-excepted = The exception list says so.
settings-proxy-why-this-computer = Proxy scripts aren't asked about this computer.
settings-proxy-why-script = The script { $from } says so.
settings-proxy-why-detected = The script found at { $from } says so.
settings-proxy-why-not-detected = No working script was found: { $reason }
settings-login-proxy = Proxy { $host }
settings-address = Address
settings-proxy-example-com = proxy.example.com
settings-port = Port
settings-username = Username
settings-optional = (optional)
settings-password = Password
settings-connect-directly-to = Connect directly to
settings-localhost-127-0-0-1-lan = localhost, 127.0.0.1, .lan
settings-now = Now: { $summary }
settings-applies-when-you-save = Applies when you save.
settings-saved-logins = Saved logins
settings-kerberos-ticket = Kerberos ticket
settings-no-keyring-service-kwallet-gnome-keyring = No keyring service (KWallet, GNOME Keyring…) is running, so logins last until Siler quits.
settings-when-a-server-asks-for-a = When a server asks for a username and password, you can save it here.
settings-forget = Forget
settings-the-extension-and-siler-are-from = The extension ({ $extension_version }) and Siler ({ $app_version }) are from different versions and may not work together.
settings-browser-integration-is-installed = Browser integration is installed
settings-extension-last-seen = Extension { $v } last seen { $v2 }
settings-waiting-for-the-extension-to-connect = Waiting for the extension to connect
settings-browser-integration-is-not-set-up = Browser integration is not set up
settings-install-the-native-messaging-host-so = Install the native messaging host so the extension can talk to Siler.
settings-application-menu-entry-and-icon = Application menu entry and icon
settings-app-menu-icon = App menu & icon
settings-start-browser-downloads-immediately = Start browser downloads immediately
settings-skip-the-confirmation-window-for-files = Skip the confirmation window for files captured from the browser
settings-firefox-librewolf-waterfox-floorp-zen = Firefox, LibreWolf, Waterfox, Floorp, Zen
settings-the-signed-siler-extension-is-ready = The signed Siler extension is ready to add. Then allow it to access all websites when Firefox asks.
settings-add-to-firefox = Add to Firefox
settings-click = Click
settings-install = Install
settings-above-to-register-siler-with-firefox = above to register Siler with Firefox.
settings-download = Download
settings-siler-integration-xpi = siler-integration.xpi
settings-from-the-latest-release = from the latest release
settings-open-releases = Open releases
settings-open-the-file-with-firefox-and = Open the file with Firefox, and allow the extension to access all websites when Firefox asks.
settings-building-from-source-see = Building from source? See
settings-docs-browser-extension-md = docs/browser-extension.md
settings-chrome-chromium-brave-vivaldi-edge-opera = Chrome, Chromium, Brave, Vivaldi, Edge, Opera
settings-above-again-if-the-browser-was = above (again, if the browser was installed since).
settings-open-the-browser-s-extensions-page = Open the browser's extensions page and turn on
settings-developer-mode = Developer mode
settings-open-in = Open in { $b }
settings-load-unpacked = Load unpacked
settings-and-choose-this-folder = and choose this folder:
settings-copy = Copy
settings-siler-s-extension-isn-t-in = Siler's extension isn't in the Chrome Web Store yet; once it is, it updates itself from there.
settings-siler-s-extension-for-chromium-based = Siler's extension for Chromium-based browsers, loaded from a folder.
settings-set-up = Set up
settings-theme = Theme
settings-sounds = Sounds
settings-play-the-desktop-s-sounds-for = Play the desktop's sounds for download events
settings-use-the-theme-sound = Use the theme sound
settings-choose = Choose…
settings-play = Play
settings-cancel = Cancel
settings-save = Save

## sidebar

sidebar-siler = Siler
sidebar-download-manager = Download manager
sidebar-add-url = Add URL
sidebar-queues = Queues
sidebar-new-queue = New queue
sidebar-running = Running
sidebar-stop-the-queue = Stop the queue
sidebar-start-the-queue = Start the queue
sidebar-edit-the-queue = Edit the queue
sidebar-categories = Categories
sidebar-global-speed-limit = Global speed limit
sidebar-settings = Settings

## speed

speed-menu-set = Set
speed-menu-custom = Custom…

## toolbar

toolbar-selected = { $selectedCount } selected
toolbar-resume-space = Resume (Space)
toolbar-pause-space = Pause (Space)
toolbar-remove-del = Remove (Del)
toolbar-start-the-downloads-added-with-download = Start the downloads added with “Download later”
toolbar-start-queue = Start queue
toolbar-hold-back-queued-downloads-that-haven = Hold back queued downloads that haven't started
toolbar-stop-queue = Stop queue
toolbar-resume-all = Resume all
toolbar-pause-all = Pause all
toolbar-sort = Sort: { $by }
toolbar-sort-reversed = Sort: { $by } (reversed)
toolbar-batch-download = Batch download…
toolbar-site-grabber = Site grabber…
toolbar-import-list = Import list…
toolbar-settings = Settings
toolbar-more = More
toolbar-search-downloads = Search downloads
toolbar-clear = Clear
toolbar-customize-the-toolbar = Customize the toolbar
toolbar-choose-the-buttons-beside-the-search = Choose the buttons beside the search box, and their order. Resume, Pause and Remove always appear for selected downloads.
toolbar-move-up = Move up
toolbar-move-down = Move down
toolbar-reset = Reset
toolbar-cancel = Cancel
toolbar-save = Save

## update

update-banner-siler-is-available = Siler { $latest } is available
update-banner-downloading = — downloading…
update-banner-update-it-with-your-package-manager = — update it with your package manager.
update-banner-this-release-has-no-appimage-yet = — this release has no AppImage yet.
update-banner-you-have = (you have { $current }).
update-banner-what-s-new = What's new
update-banner-update-and-restart = Update and restart
update-banner-download = Download
update-banner-skip-this-version = Skip this version
update-banner-not-now = Not now

## verify

verify-verify = Verify “{ $v }”
verify-expected-checksum = Expected checksum
verify-paste-it-from-the-site-md5 = (paste it from the site; MD5, SHA-1, SHA-256 or SHA-512)
verify-leave-empty-to-work-out-its = Leave empty to work out its SHA-256
verify-reading-the-file = Reading the file…
verify-the-file-is-intact = The file is intact
verify-its-matches = — its { $v } matches.
verify-the-file-doesn-t-match = The file doesn't match
verify-it-is-damaged-or-a-different = — it is damaged or a different file.
verify-its-is-below = Its { $v } is below.
verify-copy = Copy
verify-close = Close
verify-verify-2 = Verify

## zip

zip-preview-back = Back
zip-preview-files-unpacked-pick-the-ones-you = { $length } files · { $v } unpacked · pick the ones you want
zip-preview-reading-the-archive-s-list-of = Reading the archive's list of files…
zip-preview-filter-by-name = Filter by name
zip-preview-clear = Clear
zip-preview-select-all-shown = Select all shown
zip-preview-of-selected = { $length } of { $length2 } selected
zip-preview-size-to-fetch = {" · "}{ $size } ({ $fetch } to fetch)
zip-preview-nothing-matches-the-filter = Nothing matches the filter.
zip-preview-save-to = Save to
zip-preview-folders-inside-the-archive-are-kept = (folders inside the archive are kept)
zip-preview-choose-folder = Choose folder
zip-preview-download-later = Download later
zip-preview-download-selected = Download selected

## countdown (script)


## main (script)

main-link-updated-the-download-continues = Link updated — the download continues
main-all-downloads = All downloads
main-downloading = Downloading
main-queued-paused = Queued & paused
main-completed = Completed
main-failed = Failed
main-kept-up-to-date = Kept up to date
main-downloads = Downloads
main-moved-in-the-queue-sort-by = Moved in the queue — sort by queue order to see it
main-open = Open
main-show-in-folder = Show in folder
main-start = Start
main-resume = Resume
main-pause = Pause
main-start-now = Start now
main-restart = Restart…
main-download-again-2 = Download again…
main-finish-recording = Finish recording
main-check-server-key = Check server key…
main-log-in = Log in…
main-refresh-link = Refresh link…
main-copy-links = Copy links
main-copy-link = Copy link
main-properties = Properties…
main-move-to-queue = Move to queue…
main-verify-checksum = Verify checksum…
main-keep-it-anyway = Keep it anyway
main-keep-up-to-date-2 = Keep up to date…
main-move-to-top = Move to top
main-move-up = Move up
main-move-down = Move down
main-move-to-bottom = Move to bottom
main-remove = Remove…
main-no-completed-downloads = No completed downloads
main-no-failed-downloads = No failed downloads
main-import-a-download-list = Import a download list
main-download-lists = Download lists
main-all-files = All files
main-export-downloads = Export downloads
main-text = Text
main-drop-a-link-to-a-file = Drop a link to a file (http or https)

## categories (script)

categories-video = Video
categories-music = Music
categories-images = Images
categories-documents = Documents
categories-compressed = Compressed
categories-programs = Programs
categories-other = Other
categories-waiting = Waiting
categories-queued = Queued
categories-connecting = Connecting
categories-downloading = Downloading
categories-paused = Paused
categories-completed = Completed
categories-failed = Failed

## columns (script)

columns-date-added = Date added
columns-name = Name
columns-size = Size
columns-progress = Progress
columns-status = Status
columns-queue-order = Queue order
columns-speed = Speed
columns-time-left = Time left
columns-server = Server
columns-type = Type
columns-finished = Finished
columns-connections = Connections
columns-queue = Queue
columns-added = Added
columns-start-stop-queue = Start / stop queue
columns-resume-all = Resume all
columns-pause-all = Pause all
columns-when-downloads-finish = When downloads finish
columns-sort = Sort
columns-cards-details = Cards / details
columns-batch-download = Batch download
columns-site-grabber = Site grabber
columns-import-list = Import list
columns-settings = Settings
columns-more = More

## add (script)

add-other = Other
add-the-page-gave-no-video-siler = The page gave no video Siler can download.
add-save-to-folder = Save to folder
add-link-updated-the-download-continues = Link updated — the download continues

## detail (script)

detail-panel-waiting-for-the-site-to-allow = Waiting for the site to allow the download
detail-panel-reading-the-stream = Reading the stream
detail-panel-downloading = Downloading
detail-panel-saving-the-file = Saving the file
detail-panel-added-by-you = added by you
detail-panel-from-a-list = from a list
detail-panel-named-by-the-server = named by the server
detail-panel-link-copied = Link copied

## grabber (script)

grabber-mo = Mo
grabber-tu = Tu
grabber-we = We
grabber-th = Th
grabber-fr = Fr
grabber-sa = Sa
grabber-su = Su
grabber-site = Site
grabber-project-saved = Project saved
grabber-save-the-site-to = Save the site to
grabber-save-the-files-to = Save the files to
grabber-this-server = This server
grabber-the-whole-site-its-other-servers = The whole site (its other servers too)
grabber-this-folder-and-below = This folder and below

## media (script)

media-picker-hls-stream = HLS stream
media-picker-dash-stream = DASH stream
media-picker-hds-stream = HDS stream
media-picker-rtmp-mms-stream = RTMP/MMS stream
media-picker-file = File
media-picker-not-media = Not media
media-picker-protected = Protected
media-picker-unsupported = Unsupported

## properties (script)

properties-move-to-folder = Move to folder
properties-file-moved = File moved
properties-download-updated = Download updated

## queue (script)

queue-mon = Mon
queue-tue = Tue
queue-wed = Wed
queue-thu = Thu
queue-fri = Fri
queue-sat = Sat
queue-sun = Sun
queue-every-day = Every day
queue-weekdays = Weekdays
queue-weekends = Weekends
queue-do-nothing = Do nothing
queue-quit-siler = Quit Siler
queue-put-the-computer-to-sleep = Put the computer to sleep
queue-hibernate-the-computer = Hibernate the computer
queue-shut-down-the-computer = Shut down the computer

## refresh (script)

refresh-link-link-updated-the-download-continues = Link updated — the download continues

## settings (script)

settings-siler-couldn-t-check-it = Siler couldn't check it
settings-browser-integration-installed = Browser integration installed
settings-firefox-will-ask-to-add-the = Firefox will ask to add the extension
settings-update-the-browser-extension = Update the browser extension
settings-update-siler = Update Siler
settings-general = General
settings-transfers = Transfers
settings-media = Media
settings-network = Network
settings-categories = Categories
settings-browser = Browser
settings-appearance = Appearance
settings-system = System
settings-light = Light
settings-dark = Dark
settings-a-download-finished = A download finished
settings-a-download-failed = A download failed
settings-a-queue-finished = A queue finished
settings-choose-a-sound = Choose a sound
settings-try-the-command-on-a-file = Try the command on a file
settings-no-proxy = No proxy
settings-manual = Manual

## sidebar (script)

sidebar-all-downloads = All downloads
sidebar-downloading = Downloading
sidebar-queued-paused = Queued & paused
sidebar-completed = Completed
sidebar-failed = Failed
sidebar-kept-up-to-date = Kept up to date

## toolbar (script)

toolbar-do-nothing = Do nothing
toolbar-quit-siler = Quit Siler
toolbar-sleep = Sleep
toolbar-hibernate = Hibernate
toolbar-shut-down = Shut down

## verify (script)

verify-copied = Copied

## zip (script)

zip-preview-save-the-files-to = Save the files to

## format (script)

format-just-now = just now
format-unlimited = Unlimited
format-mon = Mon
format-tue = Tue
format-wed = Wed
format-thu = Thu
format-fri = Fri
format-sat = Sat
format-sun = Sun
format-every-day = every day

## rowview (script)

rowview-waiting-for-the-site = Waiting for the site…
rowview-reading-the-stream = Reading the stream…
rowview-saving = Saving…
rowview-not-started = Not started
rowview-unknown-size = Unknown size
rowview-check-server-key = Check server key
rowview-link-expired = Link expired
rowview-needs-login = Needs login
rowview-recording = Recording

## store (script)

store-main-queue = Main queue

## format (by hand)
format-bytes = { $n } { $unit }
format-per-second = { $size }/s
format-duration-s = { $s }s
format-duration-ms = { $m }m { $s }s
format-duration-hm = { $h }h { $m }m
format-duration-dh = { $d }d { $h }h
format-minutes-ago = { $n } min ago
format-hours-ago = { $n } h ago
format-schedule-from = from { $at }
format-schedule-range = { $from }–{ $to }
format-once = once
format-weekdays = weekdays
format-weekends = weekends
format-schedule = { $time }, { $days }
format-every-n-weeks =
    { $n ->
        [one] every week
       *[other] every { $n } weeks
    }
format-every-n-days =
    { $n ->
        [one] every day
       *[other] every { $n } days
    }
format-every-n-hours =
    { $n ->
        [one] every hour
       *[other] every { $n } hours
    }
format-every-n-minutes = every { $n } min

## by hand
countdown-title =
    { $action ->
        [quit] Siler quits in
        [sleep] The computer goes to sleep in
        [hibernate] The computer hibernates in
       *[other] The computer shuts down in
    }
countdown-seconds = { $s } s
countdown-now =
    { $action ->
        [quit] Quit now
        [sleep] Sleep now
        [hibernate] Hibernate now
       *[other] Shut down now
    }
main-items =
    { $count ->
        [one] { $count } item
       *[other] { $count } items
    }
main-queue-running = running
main-downloading-at = { $count } downloading at { $speed }
main-links-copied =
    { $count ->
        [one] Link copied
       *[other] { $count } links copied
    }
main-export-n = Export { $count } downloads…
main-cleared-completed =
    { $count ->
        [one] Cleared { $count } completed download; the files are kept
       *[other] Cleared { $count } completed downloads; the files are kept
    }
main-to-review =
    { $count ->
        [one] { $count } download to review
       *[other] { $count } downloads to review
    }
main-exported =
    { $count ->
        [one] Exported { $count } download
       *[other] Exported { $count } downloads
    }
main-export-title =
    { $count ->
        [one] Export { $count } download
       *[other] Export { $count } downloads
    }
main-export-all-downloads = Export all downloads
main-restart-title =
    { $count ->
        [one] Restart this download?
       *[other] Restart { $count } downloads?
    }
main-move-to-queue-title =
    { $count ->
        [one] Move to queue
       *[other] Move { $count } downloads to a queue
    }
main-kept-up-to-date-every = Kept up to date { $every }
main-checking-for-newer =
    { $count ->
        [one] Checking for a newer version…
       *[other] Checking { $count } files…
    }
add-server-gives-checksum = {" · "}the server gives its { $algo }, which is checked
add-checksum-from-the-list = { $checksum } (from the list)
add-paste-the-checksum = Paste the checksum the site gives
add-mirrors-from-the-list = { $count } from the list
category-editor-folder-title = { $category } folder
confirm-remove-quoted-name = “{ $name }”
confirm-remove-n-downloads = { $count } downloads
detail-panel-recording-so-far = Recording · { $time } so far
detail-panel-recording-gaps =
    { $gaps ->
        [one] Recording · { $time } so far · { $gaps } gap (paused or closed while live)
       *[other] Recording · { $time } so far · { $gaps } gaps (paused or closed while live)
    }
detail-panel-pieces = Pieces { $done } of { $total }
detail-panel-rebuilding = Rebuilding { $pct }%
download-row-command-exited = The command exited with { $code }
grabber-scheduled = Scheduled ·{" "}
grabber-ran = ran { $when }
grabber-not-run-yet = not run yet
grabber-saved-offline = {" · "}{ $n } saved for offline browsing
grabber-left-out-robots = {" · "}{ $n } left out for robots.txt
grabber-couldnt-be-read = {" · "}{ $n } couldn't be read
grabber-pages-read = { $n } pages read
grabber-to-go = , { $n } to go
grabber-files = {" · "}{ $n } files
grabber-saved = {" · "}{ $n } saved
queue-deleted = Deleted “{ $name }”; its downloads moved to the main queue
settings-latest-version = Siler { $version } is the latest version
settings-is-available = { $version } is available
settings-up-to-date = up to date
settings-exited-with = Exited with { $code }
sidebar-scheduled = Scheduled { $when }
sidebar-active = { $count } active
sidebar-idle = Idle
toolbar-when-all-downloads-finish = When all downloads finish…
toolbar-when-all-downloads-finish-action = When all downloads finish: { $action }
update-banner-downloading-done = — downloading { $done }
update-banner-downloading-of = — downloading { $done } of { $total }
zip-preview-added =
    { $count ->
        [one] { $count } file from the archive added
       *[other] { $count } files from the archive added
    }
rowview-recording-line = Recording · { $time } · { $size }
rowview-recording-line-gaps =
    { $gaps ->
        [one] Recording · { $time } · { $size } · { $gaps } gap
       *[other] Recording · { $time } · { $size } · { $gaps } gaps
    }
rowview-of = { $done } of { $total }
rowview-pieces = Pieces { $done } of { $total }
rowview-pieces-size = Pieces { $done } of { $total } · { $size }
rowview-rebuilding = Rebuilding { $pct }%
rowview-checking-the-checksum = Checking the checksum
rowview-checking-the-checksum-pct = Checking the checksum { $pct }%
rowview-unpacking = Unpacking
rowview-unpacking-pct = Unpacking { $pct }%
rowview-so-far = { $size } so far

## template expressions
main-restart-2 = Restart
main-download-again-3 = Download again
main-remove-2 = Remove
main-quit = Quit
main-don-t-check = Don't check
main-check-every-hour = Check every hour
main-check-every-day = Check every day
main-check-every-week = Check every week
add-unknown-size = Unknown size
add-named-after-the-stream = Named after the stream
detail-panel-not-used = {" "}· not used
detail-panel-decided-when-the-download-starts = Decided when the download starts
detail-panel-live = {" "}· live
detail-panel-original-format = original format
detail-panel-inside-the-video = Inside the video
detail-panel-as-a-separate-file = As a separate file
detail-panel-left-out = Left out
detail-panel-checking-the-checksum = Checking the checksum
detail-panel-unpacking = Unpacking
download-row-stream = Stream
grabber-the-category-folders = The category folders
grabber-save = Save
grabber-save-as-project = Save as project
media-picker-not-downloadable = Not downloadable
media-picker-siler-can-t-download-this = Siler can't download this.
properties-applies-right-away = applies right away
properties-use-verify-checksum-to-check-the = use Verify checksum to check the file now
properties-checked-when-the-download-finishes = checked when the download finishes
queue-edit-queue = Edit queue
queue-new-queue = New queue
queue-no-own-limit = No own limit
queue-not-allowed-on-this-computer = {" "}(not allowed on this computer)
queue-save = Save
queue-create-queue = Create queue
settings-check-now = Check now
settings-running = Running…
settings-try-it-on-a-file = Try it on a file…
settings-no-keyring-available = No keyring available
settings-saved-in-the-keyring = Saved in the keyring
settings-repair = Repair
settings-copied = Copied
settings-theme-sound = Theme sound
sidebar-no-limit = No limit
toolbar-reverse-order = ✓ Reverse order
toolbar-reverse-order-2 = Reverse order
toolbar-show-as-a-table-details = Show as a table (Details)
toolbar-show-as-cards = Show as cards
toolbar-export-list = Export list…
toolbar-clear-completed-keeps-files = Clear completed (keeps files)
toolbar-remove-failed-downloads = Remove failed downloads
toolbar-customize-toolbar = Customize toolbar…
verify-working-them-out = Working them out…
verify-show-its-md5-sha-1-and = Show its MD5, SHA-1 and SHA-256
batch-mirrors =
    { $count ->
        [one] { $count } mirror
       *[other] { $count } mirrors
    }
login-form-no-keyring-service = No keyring service (KWallet, GNOME Keyring…) is running; the login lasts until Siler quits

## Engine errors (error-<code>; the English matches the engine's own text)

error-invalid-url = Invalid URL: { $why }
error-network = Network error: { $why }
error-http =
    Server returned HTTP { $code } { $reason }{ $code ->
        [401] {" — "}the server requires authentication
        [403] {" — "}access denied (the link may have expired)
        [404] {" — "}the file is no longer available
        [410] {" — "}the file is no longer available
        [429] {" — "}the server asks to slow down
        [503] {" — "}the server is busy
       *[other] {""}
    }
error-text = { $text }
error-disk-full = Not enough disk space
error-io = File error: { $why }
error-db = Database error: { $why }
error-not-found = Download not found
error-exists = A file named “{ $name }” already exists
error-file-missing = The file was moved or deleted
error-unsafe-part = { $path } is a link or has other names pointing at it, so Siler won't write to it. Look at it and remove it if it isn't yours.
error-auth-required = { $host } needs a username and password
error-auth-ticket = { $host } didn't accept your Kerberos ticket, or there isn't one
error-auth-rejected = { $host } didn't accept the username and password
error-proxy-auth = The proxy needs a username and password
error-proxy-login-required = The proxy { $host } needs a username and password
error-proxy-login-rejected = The proxy { $host } didn't accept the username and password
error-proxy-script = The proxy script didn't work, so nothing was sent: { $why }
error-ftp =
    { $code ->
        [550] The file isn't on the FTP server, or can't be read: { $text } (FTP { $code })
        [553] The FTP server doesn't allow this file name: { $text } (FTP { $code })
       *[other] The FTP server refused: { $text } (FTP { $code })
    }
error-ftp-bare =
    { $code ->
        [550] The file isn't on the FTP server, or can't be read (FTP { $code })
        [553] The FTP server doesn't allow this file name (FTP { $code })
       *[other] The FTP server refused (FTP { $code })
    }
error-host-key-unknown = { $server } hasn't been connected to before. Check its key and trust it to continue.
error-host-key-changed = The key of { $host } changed since the last connection. This can mean someone is intercepting the connection; Siler won't connect. If the server was reinstalled, remove its old key from ~/.ssh/known_hosts.
error-checksum-mismatch = The file is damaged: its { $algo } doesn't match the expected checksum
error-media-not-media = This isn't a video or audio stream Siler can download.
error-media-live-unsupported = This stream is live, and Siler can't record live streams yet.
error-media-drm = This video is protected by DRM. Siler doesn't download DRM-protected media.
error-media-unsupported = This stream uses something Siler doesn't support yet: { $what }
error-media-invalid = The playlist can't be read: { $why }
error-media-body-too-large = The playlist or a file in it is larger than Siler accepts ({ $limit } bytes).
error-media-selection-unavailable = The chosen video or audio is no longer offered; choose again.
error-media-manifest-changed = The stream changed since the download started. Restart the download to use the new version.
error-media-refresh-exhausted = The server stopped giving valid pieces of the stream, even after asking for the playlist again.
error-media-segment-invalid = A piece of the stream didn't arrive as video or audio (the link may have expired).
error-media-key-fetch = The decryption key couldn't be used: { $why }
error-media-decrypt = A piece of the stream couldn't be decrypted (wrong key or damaged data).
error-media-ffmpeg-missing = FFmpeg is needed to combine these streams, but Siler couldn't find a usable copy. See Settings → Media.
error-media-ffmpeg-failed = FFmpeg couldn't combine the streams: { $why }
error-media-discontinuity-incompatible = The audio or video can't be joined into one file in this container ({ $why }). Choose another container or the original format.
error-media-url-forbidden = Siler won't contact that address: { $why }
error-media-disk-space = There isn't enough free disk space for this stream.
error-media-links-expired = The links from the page have expired. Open the page in the browser and send the video to Siler again.

## App (Rust side: tray, notifications, errors)

tray-do-nothing = Do nothing
tray-quit-siler = Quit Siler
tray-sleep = Sleep
tray-hibernate = Hibernate
tray-shut-down = Shut down
tray-show-siler = Show Siler
tray-pause-all = Pause all
tray-resume-all = Resume all
tray-start-queue = Start queue
tray-when-downloads-finish = When downloads finish
tray-watch-the-clipboard = Watch the clipboard for links
tray-quit = Quit
tray-finish = Finish
tray-pause = Pause
tray-open-folder = Open folder
tray-checking = Checking
tray-unpacking = Unpacking
tray-building = Building the file
tray-left = { $time } left
tray-waiting = { $count } waiting
tray-needs-you =
    { $count ->
        [one] 1 needs you
       *[other] { $count } need you
    }
tray-and-more = and { $count } more
window-tray-panel = Siler downloads
tray-panel-cancel = Cancel the download
tray-panel-cancel-question = Discard this download?
tray-panel-keep = Keep
tray-panel-discard = Discard
tray-panel-needs-you = Needs you
tray-panel-fresh-link = Get a fresh link
tray-panel-fix = Show
tray-panel-waiting = Waiting
tray-panel-resume = Resume
tray-panel-finished = Just finished
tray-panel-file-missing = The file was moved or deleted
tray-panel-open = Open
tray-panel-empty = Downloads from your browser show up here.
tray-panel-limit = Limit
tray-panel-open-siler = Open Siler
settings-tray-panel = Click the tray icon for the downloads panel
settings-tray-panel-hint = Off, a click opens Siler's window. A middle click always does.
tray-no-active-downloads = No active downloads
window-new-download = New download — Siler
notify-open = Open
notify-show-in-folder = Show in folder
notify-a-download = A download
notify-link-expired = The link expired
notify-click-to-get-a-fresh-link = Click to open its page, then click the link there. Siler continues from where it was.
notify-login-needed = Login needed
notify-proxy-asks-for-a-login = The proxy { $host } asks for a username and password
notify-asks-for-a-login = { $host } asks for a username and password
notify-check-the-servers-key = Check the server's key
notify-not-connected-before = Siler hasn't connected to { $server } before
power-queue-finished = The queue “{ $name }” has finished
notify-download-complete = Download complete
notify-download = Download
notify-download-failed = Download failed
tray-downloading = { $count } downloading · { $speed }/s
power-will-in-a-minute =
    { $action ->
        [quit] Siler will quit in a minute
        [sleep] Siler will put the computer to sleep in a minute
        [hibernate] Siler will hibernate the computer in a minute
       *[other] Siler will shut down the computer in a minute
    }
power-open-siler-to-cancel = { $reason }. Open Siler to cancel.
power-couldnt =
    { $action ->
        [quit] Siler couldn't quit
        [sleep] Siler couldn't put the computer to sleep
        [hibernate] Siler couldn't hibernate the computer
       *[other] Siler couldn't shut down the computer
    }
power-all-downloads-finished = All downloads have finished
tools-it-exited-with = It exited with { $code }.
tools-it-failed = It failed.
tools-the-command-failed = The command after a download failed
tools-data-quota-reached = Data quota reached
tools-downloads-continue-next-period = Downloads continue when the next period starts.
tools-file-too-large-for-a-list = The file is too large to be a download list.
tools-saved-page-gone = The saved page is no longer there
update-available = Siler { $version } is available
update-open-siler-to-update = Open Siler to update.
update-check-first = Check for updates first
update-up-to-date = Siler is up to date
update-only-appimage = Only an AppImage can update itself
update-no-appimage-in-release = The release has no AppImage with a checksum
update-checksum-unreadable = The release's checksum can't be read
update-download-failed = The update couldn't be downloaded: { $why }
update-checksum-mismatch = The downloaded update doesn't match the release's checksum, so it wasn't used
update-swap-failed = The update couldn't be put in place: { $why }
update-release-unreadable = The release information can't be read: { $why }
update-latest-is-prerelease = The latest release is a draft or a pre-release
command-siler-isnt-running = Siler isn't running
command-no-file-yet = This download has no file yet
command-not-finished-yet = The download hasn't finished yet
command-folder-gone = The folder no longer exists
command-only-web-links = Only web links can be opened
command-couldnt-start-firefox = Couldn't start Firefox: { $why }
logins-no-keyring = No keyring is available
logins-couldnt-save = Couldn't save the login in the keyring: { $why }
logins-couldnt-save-proxy = Couldn't save the proxy password in the keyring: { $why }
integration-no-chromium-extension = The Chromium extension isn't part of this build of Siler
integration-no-signed-extension = No signed extension found. Download siler-integration.xpi from the latest release and open it with Firefox.
ipc-couldnt-start = Siler couldn't start; open it to see why
settings-language = Language
settings-language-system = The system's language
settings-language-restart = Takes effect when Siler starts again.

## Skins

skin-title = Skin
skin-name-siler = Siler
skin-name-graphite = Graphite
skin-name-breeze = Breeze
skin-name-ocean = Ocean
skin-name-forest = Forest
skin-name-sunset = Sunset
skin-name-high-contrast = High contrast
skin-import = Import…
skin-export = Export…
skin-remove = Remove
skin-file = Siler skin
skin-exported = Skin exported
skin-toolbar = Toolbar buttons
skin-toolbar-skins = As the skin says
skin-toolbar-mixed = Main actions with words
skin-toolbar-icons = Icons
skin-toolbar-text = Icons and words
skin-error-too-big = A skin file can be 64 KB at most.
skin-error-not-a-skin = This isn't a Siler skin ({ $why }).
skin-error-name = A skin needs a name of 40 characters at most.
skin-error-radius = A skin's corner radius can be 0 to 20.
skin-error-unknown = Skins have no colour called “{ $key }”.
skin-error-colour = The colour “{ $key }” must be written as hex, like #5b6cff.
skin-error-gone = That skin isn't there.
toolbar-label-resume = Resume
toolbar-label-pause = Pause
toolbar-label-remove = Remove
toolbar-label-when-done = When done
toolbar-label-sort = Sort
toolbar-label-table = Table
toolbar-label-cards = Cards
