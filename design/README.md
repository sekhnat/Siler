# K-Star interface redesign — "Graphite Signal"

Design proposal for the desktop app UI. Live canvas: https://claude.ai/artifact/9kJnfLVHwv4yXCreR72323

Source artboards live in `project/` (`*.dc.html`, one per artboard, laid out by `project/canvas.json`).

## What changes compared with the current UI

| Area | Today | Proposal |
| --- | --- | --- |
| Look | Purple→cyan gradient, soft cards | Graphite surfaces, a single amber signal accent for live transfers; light theme with the same tokens |
| Numbers | Proportional Inter | JetBrains Mono, tabular — speeds and ETAs don't jitter |
| Throughput | Small card at the bottom of the sidebar | Strip across the top: total speed, sparkline, active/paused/queued counts, global limit, Pause all, Start queue |
| List | Cards with a single progress bar; sort hidden in a menu | Table-like rows with column headers that sort (Name, Progress, Size); speed and time left get their own columns |
| Segments | Only in the detail panel | Every row shows its per-connection segment bar, so multi-connection downloading is visible at a glance |
| Detail panel | Segment map + facts | Large percentage, connection stepper, per-connection speed chips, speed chart, a clear failure explanation with "Refresh link…" |
| Density | One row height | Comfortable (58 px) and compact (42 px) |
| Tray | Tooltip only | Popover with live speed, active items, a fresh-link warning, Pause all / Add URL |

## Tokens (dark · light)

- Surfaces: side `#0A0C0F · #ECECE7`, bg `#0E1014 · #F4F4F1`, panel `#13161B · #FFFFFF`, raise `#1A1E25 · #F6F6F3`
- Ink: `#ECEEF2 · #14161B`, secondary `#B0B6C2 · #434955`, muted `#8B92A0 · #636A77`
- Accent (downloading): `#F5A524`, text on accent `#1A1300`; in light, accent text is the accent mixed 58% with black
- Status: paused `#8EA2FF · #4457C8`, completed `#3DD68C · #12784A`, failed `#FF7A7A · #C33131`, queued = muted
- Type: Inter (UI), JetBrains Mono (numbers). Radii 7 / 9 / 12 / 16.

These map one to one onto the CSS variables in `app/src/app.css`.
