use std::path::PathBuf;
use std::process::ExitStatus;
use std::time::{Duration, Instant};

use async_process::{Command, Stdio};
use futures_lite::StreamExt;
use futures_lite::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use super::event::EventLine;
use super::progress::{DownloadStats, ProgressTracker};

/// How often speeds are re-measured while the fork reports no new counters,
/// so they fall to zero during a stall instead of freezing on the last value.
const SPEED_REFRESH_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Debug, Clone)]
pub enum LoginMethod {
    UsernamePassword {
        username: String,
        password: String,
    },
    Qr,
    /// Reconnects with a previously remembered login (see the
    /// `-remember-password`/`account.config` mechanism documented in
    /// CLAUDE.md) - no password or QR scan needed, just the account name.
    RememberedUsername {
        username: String,
    },
}

#[derive(Debug, Clone)]
pub struct DownloadRequest {
    pub depot_downloader_binary: PathBuf,
    pub app_id: String,
    pub download_dir: Option<PathBuf>,
    pub max_downloads: Option<u32>,
    pub login: LoginMethod,
}

impl DownloadRequest {
    fn build_args(&self) -> Vec<String> {
        // -json makes stdout carry only exact machine-readable events.
        // -remember-password is unconditional: it's what makes DepotDownloader
        // persist a login token to account.config after any successful login
        // (password or QR alike), which is what LoginMethod::RememberedUsername
        // relies on for later runs. Passing it is only ever an error when
        // there's no -username and no -qr, which never happens here.
        let mut args = vec![
            "-app".to_string(),
            self.app_id.clone(),
            "-json".to_string(),
            "-remember-password".to_string(),
        ];
        if let Some(dir) = &self.download_dir {
            args.push("-dir".to_string());
            args.push(dir.to_string_lossy().into_owned());
        }
        if let Some(max_downloads) = self.max_downloads {
            args.push("-max-downloads".to_string());
            args.push(max_downloads.to_string());
        }
        match &self.login {
            LoginMethod::UsernamePassword { username, password } => {
                args.push("-username".to_string());
                args.push(username.clone());
                args.push("-password".to_string());
                args.push(password.clone());
            }
            LoginMethod::Qr => args.push("-qr".to_string()),
            LoginMethod::RememberedUsername { username } => {
                args.push("-username".to_string());
                args.push(username.clone());
            }
        }
        args
    }

    /// Same as [`Self::build_args`], but with the password blanked out, for
    /// logging the launch command without leaking credentials to the console.
    fn build_args_for_logging(&self) -> Vec<String> {
        let mut args = self.build_args();
        if let Some(password_position) = args.iter().position(|arg| arg == "-password")
            && let Some(password) = args.get_mut(password_position + 1)
        {
            "***".clone_into(password);
        }
        args
    }
}

// A `Stats` event is sent at most a few dozen times a second over an async
// channel - not a hot path where boxing it to shrink this enum would buy
// anything real.
#[allow(clippy::large_enum_variant)]
pub enum ProcessEvent {
    Stats(DownloadStats),
    Exited(std::io::Result<ExitStatus>),
    FailedToStart(String),
}

/// Runs a DepotDownloader download to completion, sending a fresh
/// [`DownloadStats`] snapshot on `updates` after every meaningful change,
/// forwarding lines from `respond` (e.g. a typed Steam Guard code) to the
/// child's stdin, and killing the child as soon as anything arrives on
/// `cancel` (used for both pausing and cancelling - see `app::RootView`,
/// which decides what a resulting early exit means). Intended to be driven
/// from `cx.background_executor()`.
pub async fn run(
    request: DownloadRequest,
    updates: async_channel::Sender<ProcessEvent>,
    respond: async_channel::Receiver<String>,
    cancel: async_channel::Receiver<()>,
) {
    if let Some(dir) = &request.download_dir
        && let Err(error) = std::fs::create_dir_all(dir)
    {
        let _ = updates
            .send(ProcessEvent::FailedToStart(format!(
                "Could not create download directory: {error}"
            )))
            .await;
        return;
    }

    eprintln!(
        "[depot-downloader-gpui] launching {} {}",
        request.depot_downloader_binary.display(),
        request.build_args_for_logging().join(" ")
    );

    let mut command = Command::new(&request.depot_downloader_binary);
    command
        .args(request.build_args())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    // The remembered-login token itself (account.config) is unaffected by
    // this - DepotDownloader stores it via .NET's per-user IsolatedStorage,
    // keyed by the assembly, not by any real filesystem path. But without
    // `-dir`, DepotDownloader falls back to a "depots" folder relative to
    // its own current working directory - which defaults to whatever
    // directory the GUI happened to be launched from otherwise, and it also
    // keeps its ".DepotDownloader" manifest/staging cache alongside that
    // same install directory. Pinning it to the binary's own install
    // directory (already a stable, writable location - see
    // `provisioning::install_dir`) keeps both predictable regardless of how
    // the user starts the app.
    if let Some(install_dir) = request.depot_downloader_binary.parent() {
        command.current_dir(install_dir);
    }

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            eprintln!("[depot-downloader-gpui] failed to launch DepotDownloader: {error}");
            let _ = updates
                .send(ProcessEvent::FailedToStart(format!(
                    "Could not start DepotDownloader: {error}"
                )))
                .await;
            return;
        }
    };

    let stdout = child.stdout.take().expect("stdout was piped");
    let stderr = child.stderr.take().expect("stderr was piped");
    let mut stdin = child.stdin.take().expect("stdin was piped");

    let (line_tx, line_rx) = async_channel::unbounded::<String>();
    smol::spawn(forward_lines(stdout, line_tx.clone())).detach();
    smol::spawn(forward_lines(stderr, line_tx.clone())).detach();
    drop(line_tx);

    smol::spawn(async move {
        while let Ok(line) = respond.recv().await {
            let _ = stdin.write_all(line.as_bytes()).await;
            let _ = stdin.write_all(b"\n").await;
            let _ = stdin.flush().await;
        }
    })
    .detach();

    let mut tracker = ProgressTracker::new();
    let mut speed_refresh = smol::Timer::interval(SPEED_REFRESH_INTERVAL);

    loop {
        let stats_changed = match next_sample(&line_rx, &cancel, &mut speed_refresh).await {
            Sample::CancelRequested => {
                eprintln!(
                    "[depot-downloader-gpui] stopping DepotDownloader (pause/cancel requested)"
                );
                let _ = child.kill();
                break;
            }
            Sample::Line(line) => match serde_json::from_str::<EventLine>(&line) {
                Ok(event_line) => {
                    tracker.apply(event_line, Instant::now());
                    true
                }
                Err(_) => false,
            },
            Sample::SpeedRefreshDue => tracker.refresh_speeds(Instant::now()),
            Sample::LineChannelClosed => break,
        };

        if stats_changed
            && updates
                .send(ProcessEvent::Stats(tracker.stats().clone()))
                .await
                .is_err()
        {
            return;
        }
        if tracker.stats().login_expired {
            let _ = child.kill();
            break;
        }
    }

    let status = child.status().await;
    eprintln!("[depot-downloader-gpui] DepotDownloader exited with {status:?}");
    let _ = updates.send(ProcessEvent::Exited(status)).await;
}

enum Sample {
    Line(String),
    SpeedRefreshDue,
    LineChannelClosed,
    CancelRequested,
}

/// Racing `speed_refresh` before `line_rx` matters: `or` polls its first
/// argument first, so a continuously busy `line_rx` could otherwise starve the
/// refresh forever.
async fn next_sample(
    line_rx: &async_channel::Receiver<String>,
    cancel_rx: &async_channel::Receiver<()>,
    speed_refresh: &mut smol::Timer,
) -> Sample {
    let next_line = async {
        match line_rx.recv().await {
            Ok(line) => Sample::Line(line),
            Err(_) => Sample::LineChannelClosed,
        }
    };
    let refresh_due = async {
        speed_refresh.next().await;
        Sample::SpeedRefreshDue
    };
    let cancel_requested = async {
        let _ = cancel_rx.recv().await;
        Sample::CancelRequested
    };
    futures_lite::future::or(
        cancel_requested,
        futures_lite::future::or(refresh_due, next_line),
    )
    .await
}

/// Reads raw bytes and splits on `\n` ourselves, rather than using
/// `AsyncBufReadExt::lines()`, because that adapter silently ends the whole
/// stream the moment one line fails strict UTF-8 decoding - and DepotDownloader's
/// QR code block is exactly the kind of output most likely to hit an encoding
/// edge case on some platforms. `from_utf8_lossy` never fails, so one malformed
/// line can never take down the rest of the read loop with it.
async fn forward_lines(
    reader: impl futures_lite::AsyncRead + Unpin,
    sink: async_channel::Sender<String>,
) {
    let mut reader = BufReader::new(reader);
    let mut raw_line = Vec::new();
    loop {
        raw_line.clear();
        match reader.read_until(b'\n', &mut raw_line).await {
            Ok(0) => break,
            Ok(_) => {
                let line = String::from_utf8_lossy(&raw_line);
                let line = line.trim_end_matches(['\r', '\n']);
                eprintln!("[DepotDownloader] {line}");
                if sink.send(line.to_string()).await.is_err() {
                    break;
                }
            }
            Err(error) => {
                eprintln!("[depot-downloader-gpui] error reading DepotDownloader output: {error}");
                break;
            }
        }
    }
}
