# DepotDownloader GUI

A native desktop app for downloading Steam games and tools straight from Steam's
content servers — no Steam client required. It's a GUI wrapper around
[DepotDownloader](https://github.com/SteamRE/DepotDownloader), built in Rust with
[Zed](https://zed.dev)'s own UI engine (`gpui`), so it looks and feels like Zed:
dark, minimal, fast.

## Features

- **Sign in your way** — username/password, or scan a QR code with the Steam
  Mobile app. Steam Guard prompts (email code, mobile confirmation) appear
  inline, right where you'd expect them.
- **Exact progress, not a guess** — total size, downloaded bytes, bytes written
  to disk, elapsed time, ETA, download speed, and disk write speed, all updated
  live. Powered by a fork of DepotDownloader that reports real byte counters
  instead of leaving the GUI to estimate.
- **Pick a game like you would in Steam** — a searchable dropdown of every game
  and tool your account owns (plus Steam's free-to-play/tools catalog), with
  DLC and branch pickers that appear automatically once you choose one.
- **Steam library aware** — defaults to your existing Steam library folder and
  can write an appmanifest when the download finishes, so the game shows up in
  Steam after a restart, no manual import step.
- **Pause, resume, cancel** — pause safely stops the process; resuming picks up
  exactly where it left off by re-verifying what's already on disk.
- **Zero manual setup** — the first time you run it, it downloads and installs
  the DepotDownloader binary it needs automatically.

## Installation

### Download a build

Grab the latest build for your platform from the
[Releases page](../../releases):

| Platform | Download |
| --- | --- |
| Windows (x64) | `depot-downloader-gpui-windows-x64.zip` |
| macOS (Apple Silicon) | `depot-downloader-gpui-macos-arm64.zip` |
| macOS (Intel) | `depot-downloader-gpui-macos-x64.zip` |
| Linux (x64) | `depot-downloader-gpui-linux-x64.zip` |

Unzip and run the executable inside.

- **Windows**: if SmartScreen warns you the app is unsigned, click *More info* →
  *Run anyway*.
- **macOS**: the app isn't notarized yet, so Gatekeeper will refuse to open it
  on the first double-click. Either right-click the app → *Open* → *Open*
  again in the dialog, or run:
  ```bash
  xattr -d com.apple.quarantine depot-downloader-gpui
  ```
- **Linux**: mark it executable first:
  ```bash
  chmod +x depot-downloader-gpui
  ./depot-downloader-gpui
  ```
  A desktop generally already has the shared libraries it needs (audio,
  fontconfig, X11/Wayland, Vulkan). If it fails to start, run it from a
  terminal — the error names whichever library is missing.

### Build from source

Requires the latest stable Rust toolchain (edition 2024, so 1.85+) — install
via [rustup](https://rustup.rs).

```bash
git clone https://github.com/deezhizyu/depot-downloader-gpui.git
cd depot-downloader-gpui
cargo build --release
```

The binary lands at `target/release/depot-downloader-gpui` (`.exe` on
Windows). On Linux you'll additionally need the development packages for
audio, fonts, and windowing — see the package list in
[`.github/workflows/release.yml`](.github/workflows/release.yml) for exactly
what CI installs before building.

## How to use

1. **Launch the app.** On first run it downloads its DepotDownloader binary
   automatically — you don't need to install anything yourself.
2. **Sign in.** Choose *Username & Password* or *QR Code*. For QR, scan the
   code that appears with the Steam Mobile app. If Steam asks for a Guard
   code or a mobile confirmation, the prompt shows up right there — answer it
   and sign-in continues.
3. **Choose a game.** Type a name or an app id into the *Game* field. The list
   covers everything your account owns plus Steam's free/tools catalog.
4. **Pick DLC and a branch**, if the game has them. Both dropdowns appear
   automatically once a game is selected; branches default to `public`, and a
   password-protected beta branch is marked as such.
5. **Set a library folder.** Defaults to your detected Steam library's
   `steamapps/common`. Use *Browse…* to pick somewhere else, or clear the
   field to let DepotDownloader use its own default folder. If the folder is
   a real Steam library, check *Add to Steam library when finished* to have
   the game show up in Steam after you restart it.
6. **Optional: cap concurrent downloads** in the "Max concurrent downloads"
   field. Leave it blank to use DepotDownloader's default.
7. **Click Download.** Watch live progress — percentage, elapsed time, ETA,
   bytes downloaded/written, and a real-time speed graph.
8. **Pause, Resume, or Cancel** at any time from the same screen. Cancelling
   asks for confirmation first; anything already downloaded stays on disk
   either way.
9. **Logout** clears the remembered Steam session from the login section once
   you're signed in.

## Why a fork of DepotDownloader?

Upstream DepotDownloader prints human-readable console text with no byte
counters, so a GUI wrapping it could only guess at progress. This app uses
[deezhizyu/depot-downloader](https://github.com/deezhizyu/depot-downloader), a
fork that adds a `-json` output mode: exact cumulative byte counters, a
`plan` event with the real total before anything downloads, login/prompt
events, and the raw QR login URL — everything this GUI's progress view is
built on.

## Credits

- [DepotDownloader](https://github.com/SteamRE/DepotDownloader) by the SteamRE
  project, and [deezhizyu/depot-downloader](https://github.com/deezhizyu/depot-downloader),
  the `-json`-mode fork this app drives (GPL-2.0).
- [Zed](https://zed.dev) and its `gpui` UI engine, via
  [gpui-kit](https://gpui-kit.com), for the rendering engine and widgets this
  app's interface is built with.
