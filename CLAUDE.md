# DepotDownloader GUI

A native desktop GUI for [DepotDownloader](https://github.com/SteamRE/DepotDownloader) (a
Steam depot/game downloader), written in Rust with Zed's `gpui` framework. It wraps the
DepotDownloader CLI as a subprocess, parses its console output, and presents progress the way a
Steam-client download would look, styled to match Zed's own UI.

## Goals

- Username/password login and QR login (scan with the Steam Mobile app), both backed by
  DepotDownloader's own `-username`/`-password` and `-qr` flags.
- Exact progress numbers from our DepotDownloader fork's `-json` output: total/known download size,
  current downloaded size, an accurate time estimate, download speed, and disk write speed, all
  in MB/s.
- For this first version, the inputs are the Steam app id, the library folder (default: the
  detected Steam library's `steamapps/common` folder, or none to let DepotDownloader use its own
  default), and an optional `-max-downloads` count (persisted; empty omits the flag). No branch/depot/platform/language selection yet.
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
- **DepotDownloader is our own fork**: upstream prints no byte counters, so every number a
  scraper could show would be an estimate. `github.com/deezhizyu/depot-downloader` (GPL-2.0, a fork
  of SteamRE/DepotDownloader) adds a `-json` flag: stdout then carries only JSON Lines (schema in
  the fork's `docs/json-mode.md`) with exact cumulative counters - `network_bytes` (compressed bytes
  received), `written_bytes` (uncompressed bytes written), `verified_bytes` (already valid on disk,
  so not re-downloaded) - plus a `plan` event with exact totals before any download starts, prompt
  events, the raw QR challenge URL, `login_success` with the account name, and an `error` event.
  `depot_downloader::provisioning` downloads the matching `DepotDownloader-{os}-{arch}.zip` from the
  fork's `releases/latest/download/...` permalink (no GitHub API call) into
  `depot-downloader-fork/` under the data dir the first time it is missing.
- **Parsing is JSON, not text** (`depot_downloader::event`, serde): every line is an `EventLine`
  (`t_ms` + a tagged `Event`); unknown events deserialize to `Event::Ignored`, and lines that are not
  JSON (stderr traces) are dropped after being echoed to the console. `t_ms` is the fork's own
  monotonic clock, so speeds are right even if this app reads a queued batch late.
- **Progress numbers** (`depot_downloader::progress`): `ProgressTracker` copies the counters from
  `progress`/`done` events into `DownloadStats` and keeps a short sliding window of samples; download
  speed is `network_bytes` growth and disk speed is growth of `written + verified` bytes (so the validation pass shows a rate) over that window (1s), both
  divided by elapsed `t_ms`. `process::run` also refreshes speeds every 100ms
  (`SPEED_REFRESH_INTERVAL`) so they decay to zero when counters stop growing instead of freezing.
  ETA is remaining uncompressed bytes (`total - written - verified`) over disk speed. "Downloaded"
  shows `network_bytes` against `network_total_bytes()`: the plan's compressed total scaled by the
  share of uncompressed bytes not already verified, which is exact on a clean download and the best
  available figure on a resume. "Written to disk" is `written + verified` against the plan's
  uncompressed total. The race in `process::next_sample` polls the refresh timer before the line
  channel because `futures_lite::future::or` polls its first argument first, so a busy line channel
  can never starve it.
- **QR login**: the fork emits the challenge URL (on creation and every refresh); the GUI encodes it
  with the `qrcode` crate and draws each module as an explicit black/white square `div` with an
  explicit pixel width and height (it sits in a `v_flex`, which would otherwise stretch it).
- **Prompts**: an `auth_prompt` (Steam Guard / email code / device confirmation) stays on
  `DownloadStats` until any other event arrives - the fork is silent while a prompt is pending. A
  `password` prompt during a `RememberedUsername` run means the saved login expired: the tracker
  sets `login_expired`, `process::run` kills the child, and `RootView` forgets the username.
- **Reading the child's stdout/stderr is byte-based, not `AsyncBufReadExt::lines()`**
  (`depot_downloader::process::forward_lines`): `lines()` silently ends its whole stream the
  moment one line fails strict UTF-8 decoding. `forward_lines` reads with `read_until(b'\n', ...)`
  and decodes with `String::from_utf8_lossy`, which never fails.
- **Remembering a login, and Logout** (`LoginMethod::RememberedUsername`): every launch passes
  `-remember-password` unconditionally (DepotDownloader only rejects it when *both* `-username` and
  `-qr` are absent, which never happens here). After a successful login DepotDownloader persists a
  login token itself via .NET's per-user `IsolatedStorageFile`. A later run reconnects with just
  `-username {name} -remember-password`. `Config.logged_in_username` is set from the
  `login_success` event, which only fires on success, so a bad password or rejected QR scan can
  never poison the remembered account. `Logout` clears it and best-effort deletes DepotDownloader's
  own `account.config` (`RootView::logout`).
- **`process::run`'s `Command` sets a fixed `current_dir`** (the binary's install directory, from
  `provisioning::install_dir`): DepotDownloader falls back to a `"depots"` folder relative to its
  working directory whenever `-dir` is omitted, and keeps its `.DepotDownloader` manifest/staging
  cache alongside the download. Without a fixed `current_dir` both would depend on wherever the OS
  launched the GUI from.
- **Pause / Resume / Cancel**: pausing kills the child (`process::run` races a `cancel` channel into
  its select loop) rather than suspending it - DepotDownloader verifies existing files against the
  manifest on every run and only re-fetches what is missing, so relaunching *is* a correct
  continuation, and `verified_bytes` accounts for what was already there. `RunState::Paused` is only
  reachable from `Running`, which guarantees a confirmed username, so `resume_download` always
  reconnects via `LoginMethod::RememberedUsername`. Cancel reuses the same kill; the difference is
  what `app::apply_process_event` does with `ProcessEvent::Exited` (tracked via `PendingControl`).
  Cancel is gated behind a confirmation (`Window::open_alert_dialog`).
- **Install folder lookup** (`steam::fetch_install_dir_name`): the chosen location is a library
  folder and the game lands in `{library}/{installdir}`. The fork only reports `installdir` after
  launch but `-dir` must be chosen before, so one blocking call to
  `api.steamcmd.net/v1/info/{app_id}` (10s timeout, `RunState::LookingUpApp`) supplies it, falling
  back to the app id if the name is missing or not a single safe path component.
- **Console logging**: `depot_downloader::process` and `provisioning` print `eprintln!` lines
  (the fork's stdout/stderr verbatim, the launch command with the password redacted, exit status,
  provisioning steps) so a stuck run can be diagnosed from a terminal.
- **Process I/O**: `async-process` + `smol` (already in gpui's own dependency tree) rather than
  `tokio`, since gpui's executor is smol-based.
- **Config persistence**: the last app id, download location, max-downloads text and remembered
  username are saved as JSON under the OS config directory (`directories::ProjectDirs`). Login
  credentials are never persisted.

## Project layout

```
src/
  main.rs                       Entry point: opens the window, installs the theme
  app.rs                        The single root view (form + status/progress area)
  config.rs                     Persisted settings
  theme.rs                      Applies assets/theme/zed_one_dark.json to gpui-component
  steam/
    library_path.rs             Per-OS default Steam "steamapps/common" detection
    app_id.rs                   Steam app id from plain text or a store URL
    app_info.rs                 App id -> `installdir` folder name (api.steamcmd.net)
  depot_downloader/
    event.rs                    Serde model of the fork's -json event lines
    progress.rs                 Events -> DownloadStats (exact counters, windowed speeds, ETA)
    process.rs                  Spawns DepotDownloader, streams output, feeds stdin
    provisioning.rs             Locates or downloads+extracts the fork's binary
  ui/
    format.rs                   Byte/speed/ETA formatting shared by the views
assets/
  theme/zed_one_dark.json       Zed's One Dark palette in gpui-component's theme schema
```
