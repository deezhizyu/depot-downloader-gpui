# DepotDownloader GUI

A native desktop GUI for [DepotDownloader](https://github.com/SteamRE/DepotDownloader) (a
Steam depot/game downloader), written in Rust with Zed's `gpui` framework. It wraps the
DepotDownloader CLI as a subprocess, parses its console output, and presents progress the way a
Steam-client download would look, styled to match Zed's own UI.

## Goals

- Username/password login and QR login (scan with the Steam Mobile app), both backed by
  DepotDownloader's own `-username`/`-password` and `-qr` flags.
- A genuinely good parser of DepotDownloader's console output: total/known download size,
  current downloaded size, an accurate time estimate, download speed, and disk write speed, all
  in MB/s.
- For this first version, the only inputs are the Steam app id and the download location
  (default: the detected Steam library's `steamapps/common` folder, or none to let
  DepotDownloader use its own default). No branch/depot/platform/language selection yet.
- Look and feel like Zed: dark, minimal, custom window chrome, matching color/typography tokens.

## Code style rules (apply to every change in this repo)

- Prefer the simple implementation. Do not reach for a complex mechanism when a simple one
  works just as well.
- Code should read as self-documenting: full, precise function and variable names, idiomatic
  Rust naming conventions. Do not write comments that restate what the code already says; only
  comment a genuinely non-obvious constraint, invariant, or reason.
- Keep a conventional file/directory layout (see below) — one clear responsibility per module.
- No duplicated logic and no repeated computation: compute a value once, store it, reuse it.
- Code must be professional, readable, efficient, and correct the first time — write it so it
  will not need a follow-up refactor.
- Use the latest stable (never beta/nightly) Rust toolchain and the latest stable version of
  every dependency.

## Architecture and key decisions

- **UI toolkit**: a single dependency, [`gpui-kit`](https://gpui-kit.com) (crates.io), which
  bundles a published snapshot of Zed's `gpui`, the `gpui-component` widget library (buttons,
  text inputs, a `TitleBar`), and default assets. This avoids pinning a git dependency on the
  Zed monorepo while still getting Zed's actual rendering engine.
- **Theme**: `src/theme.rs` loads `assets/theme/zed_one_dark.json` (a `ThemeConfig` in
  gpui-component's own theme schema) and applies it with `Theme::apply_config`, overriding
  gpui-component's default dark palette with Zed's real "One Dark" color tokens and IBM Plex
  Sans/Mono typography. Every widget then matches Zed's look with no per-widget style overrides.
- **One screen, not a login screen + a download screen**: DepotDownloader requires `-app` even
  to reach a QR login prompt, so login and downloading are not separate CLI invocations — there
  is exactly one DepotDownloader run per click of "Download", carrying whichever login method was
  selected. The UI reflects that: one form (app id, download location, login method/credentials)
  plus a status area that shows the QR code, a Steam Guard prompt, or live progress depending on
  what DepotDownloader's output says.
- **DepotDownloader is provisioned automatically**: `depot_downloader::provisioning` downloads
  the matching `DepotDownloader-{os}-{arch}.zip` from GitHub's `releases/latest/download/...`
  permalink (no GitHub API call needed) the first time it's needed, extracts the executable, and
  remembers its path in the config file so later launches skip the download.
- **Parsing DepotDownloader's output** (`depot_downloader::parser`): DepotDownloader's own
  overall-progress escape sequence (`ESC ]9;4;{state};{progress} BEL`) is only ever emitted when
  stdout is a real terminal — `Console.IsOutputRedirected` disables it, and piping stdout (which
  we must do, to parse it) always redirects it. So the primary, always-present live signal is the
  per-file completion line (`"{percent}% {path}"`), which reports the *current depot's* running
  percentage; DepotDownloader never prints an absolute byte total while downloading, only once a
  depot (`"Depot N - Downloaded ..."`) or the whole run (`"Total downloaded: ..."`) finishes.
  QR login is similar: DepotDownloader renders the QR code as ASCII/Unicode block art in the
  terminal and never prints the underlying URL as text, so we capture that block verbatim. Both
  the parser and the renderer deliberately avoid checking for the literal `'█'` (U+2588 FULL
  BLOCK) glyph QRCoder uses for a dark module: on some platforms DepotDownloader's stdout for
  that character does not survive as valid UTF-8 and decodes to the Unicode replacement character
  instead, so the real byte identity of "dark" isn't predictable — only that light modules are
  always plain spaces and dark modules are always some other single, consistent, non-space
  character. `parser::is_qr_art_line` treats a line as QR art whenever it has at most one distinct
  non-space character; `app::render_qr_code` treats a sampled module as dark whenever it's
  non-whitespace at all. We also do not render the block as text: each QR module is drawn as two
  identical characters, and that glyph isn't in IBM Plex Mono, so text rendering produced a blank
  area even once the text itself was captured correctly. `render_qr_code` instead samples one
  character per module (`step_by(2)`) and draws each module as an explicit black/white square
  `div`, which is correct regardless of font or encoding and reads reliably by a phone camera. It
  then blocks silently waiting for the phone to confirm the scan, so no line ever
  marks where the block ends either — `process::run`'s read loop races a short idle timer
  (`QR_FLUSH_IDLE_TIMEOUT`) against new output and calls
  `OutputParser::flush_pending_qr_block` once things go quiet, rather than waiting for a
  terminator that will never come.
- **Reading the child's stdout/stderr is byte-based, not `AsyncBufReadExt::lines()`**
  (`depot_downloader::process::forward_lines`): `lines()` silently ends its whole stream the
  moment one line fails strict UTF-8 decoding, which would permanently kill that reader task
  with no error logged — exactly the "output just stops forever, right in the middle of a QR
  block" failure this app hit in practice. `forward_lines` instead reads with
  `read_until(b'\n', ...)` and decodes each line with `String::from_utf8_lossy`, which never
  fails, so one malformed line can never take the rest of the stream down with it.
- **Progress numbers** (`depot_downloader::progress`): DepotDownloader never prints a running
  byte counter, and polling the download directory's raw size on disk (`depot_downloader::process`'s
  disk poller) can't stand in for one directly — DepotDownloader pre-allocates each file to its
  full final size the moment it creates it, so the directory's size jumps to (near) the total
  almost instantly, long before the data has actually arrived. What that polling *is* good for is
  recovering the total size DepotDownloader itself never prints: `ProgressTracker` nets out
  whatever was already in the directory before this run (`baseline_disk_bytes`, so a shared or
  reused download folder's pre-existing content isn't mistaken for part of this download) to get
  `total_size_estimate_bytes`, then multiplies that by the current depot's real completion
  percentage (from CLI output) to get `downloaded_bytes`. Disk/download speed are the rate of
  change of that estimate over a short sliding window; "download speed" (network) further scales
  disk throughput by the compressed/uncompressed byte ratio learned from depots that have already
  finished (1:1 until the first one completes, since that's the only place DepotDownloader reports
  both figures). ETA is derived from the current depot's percent-per-second rate directly, not
  from a byte estimate, since a depot's total size is never printed while it's running. A Steam
  Guard prompt or QR code is cleared from `DownloadStats` as soon as real depot activity resumes
  (`ProcessingDepot`/`DownloadingDepot`/`FileProgress`), since otherwise it would stay set for the
  rest of the run and permanently block the progress view from showing.
- **Console logging**: `depot_downloader::process` and `provisioning` print `eprintln!` lines
  (DepotDownloader's own stdout/stderr verbatim, plus the launch command with the password
  redacted, exit status, and provisioning steps) so a stuck or confusing run can be diagnosed by
  running the app from a terminal, without needing a dedicated logging crate.
- **Process I/O**: `async-process` + `smol` (already in gpui's own dependency tree, so this adds
  no new runtime) rather than `tokio`, since gpui's executor is smol-based.
- **Config persistence**: the last app id, last download location, and the resolved
  DepotDownloader binary path are saved as JSON under the OS config directory
  (`directories::ProjectDirs`). Login credentials are never persisted.

## Project layout

```
src/
  main.rs                       Entry point: opens the window, installs the theme
  app.rs                        The single root view (form + status/progress area)
  config.rs                     Persisted settings (app id, download location, binary path)
  theme.rs                      Applies assets/theme/zed_one_dark.json to gpui-component
  steam/
    library_path.rs             Per-OS default Steam "steamapps/common" detection
  depot_downloader/
    parser.rs                   DepotDownloader stdout/stderr line -> DownloadEvent
    progress.rs                 DownloadEvent + disk samples -> DownloadStats (speeds, ETA)
    process.rs                  Spawns DepotDownloader, streams output, feeds stdin
    provisioning.rs             Locates or downloads+extracts the DepotDownloader binary
  ui/
    format.rs                   Byte/speed/ETA formatting shared by the views
assets/
  theme/zed_one_dark.json       Zed's One Dark palette in gpui-component's theme schema
```
