use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::time::{Duration, Instant};

use async_process::{Command, Stdio};
use futures_lite::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use super::parser::OutputParser;
use super::progress::{DownloadStats, ProgressTracker};

/// How often we re-measure the download directory's size on disk.
const DISK_POLL_INTERVAL: Duration = Duration::from_millis(500);

/// How long to wait for more output before assuming a pending QR code block
/// is complete. DepotDownloader prints the QR and then blocks silently
/// waiting for the phone to confirm the scan, so nothing ever marks the
/// block's end in the text itself.
const QR_FLUSH_IDLE_TIMEOUT: Duration = Duration::from_millis(700);

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
    pub login: LoginMethod,
}

impl DownloadRequest {
    fn build_args(&self) -> Vec<String> {
        // -debug enables per-chunk "Downloading chunk ..." lines, the
        // finest-grained progress signal DepotDownloader offers - see
        // ProgressTracker::record_chunk_download_started for what it buys us.
        // -remember-password is unconditional: it's what makes DepotDownloader
        // persist a login token to account.config after any successful login
        // (password or QR alike), which is what LoginMethod::RememberedUsername
        // relies on for later runs. Passing it is only ever an error when
        // there's no -username and no -qr, which never happens here.
        let mut args = vec![
            "-app".to_string(),
            self.app_id.clone(),
            "-debug".to_string(),
            "-remember-password".to_string(),
        ];
        if let Some(dir) = &self.download_dir {
            args.push("-dir".to_string());
            args.push(dir.to_string_lossy().into_owned());
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

// `DownloadStats` naturally accumulates fields as this app's data model
// grows, and a `Stats` event is only ever sent a few times a second over an
// async channel - not a hot path where boxing it to shrink this enum would
// buy anything real.
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
        .stderr(Stdio::piped());
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

    let (disk_tx, disk_rx) = async_channel::unbounded::<u64>();
    if let Some(dir) = request.download_dir.clone() {
        smol::spawn(poll_disk_usage(dir, disk_tx)).detach();
    }

    let mut parser = OutputParser::new();
    let mut tracker = ProgressTracker::new();
    // Only pushed forward by real output (`Sample::Line`), never by a disk
    // sample - see `next_sample`'s doc comment for why that distinction matters.
    let mut qr_idle_deadline = Instant::now() + QR_FLUSH_IDLE_TIMEOUT;

    loop {
        // Drained unconditionally, before racing anything else in
        // `next_sample`, so a burst of `-debug` diagnostic noise on
        // `line_rx` (see `next_sample`'s doc comment) can never starve disk
        // polling the way it could when a disk sample was just another arm
        // of that race.
        let mut stats_changed = false;
        while let Ok(bytes) = disk_rx.try_recv() {
            tracker.apply_disk_sample(bytes, Instant::now());
            stats_changed = true;
        }

        match next_sample(&line_rx, &cancel, qr_idle_deadline).await {
            Sample::CancelRequested => {
                eprintln!(
                    "[depot-downloader-gpui] stopping DepotDownloader (pause/cancel requested)"
                );
                let _ = child.kill();
                break;
            }
            Sample::Line(line) => {
                qr_idle_deadline = Instant::now() + QR_FLUSH_IDLE_TIMEOUT;
                let events = parser.feed(&line);
                // `-debug` also enables .NET's HttpClient diagnostics, which
                // produce no event at all (see `parser::dotnet_diagnostic_noise`).
                // Not treating those as a change keeps a fast download from
                // triggering a Stats resend + re-render on every single one.
                stats_changed |= !events.is_empty();
                for event in events {
                    tracker.apply_event(event);
                }
            }
            Sample::Idle => {
                qr_idle_deadline = Instant::now() + QR_FLUSH_IDLE_TIMEOUT;
                if let Some(event) = parser.flush_pending_qr_block() {
                    eprintln!("[depot-downloader-gpui] QR code ready (no further output arrived)");
                    tracker.apply_event(event);
                    stats_changed = true;
                }
            }
            Sample::LineChannelClosed => break,
        }

        if stats_changed
            && updates
                .send(ProcessEvent::Stats(tracker.stats().clone()))
                .await
                .is_err()
        {
            return;
        }
    }

    let status = child.status().await;
    eprintln!("[depot-downloader-gpui] DepotDownloader exited with {status:?}");
    let _ = updates.send(ProcessEvent::Exited(status)).await;
}

enum Sample {
    Line(String),
    Idle,
    LineChannelClosed,
    CancelRequested,
}

/// Disk samples are deliberately not one of this function's arms - they're
/// drained separately, non-blockingly, at the top of every iteration of
/// `run`'s loop, specifically so line volume can never delay or starve them.
/// An earlier version raced a `next_disk_sample` future here alongside
/// `next_line`, but `futures_lite::future::or` always polls its first
/// argument first and returns immediately if it's ready: once `-debug` was
/// added and its diagnostic output could keep `line_rx` continuously
/// non-empty, `next_disk_sample` stopped being polled at all for as long as
/// that backlog lasted, and `Downloaded`/`Download speed`/`Disk speed`
/// froze at zero for the whole run.
///
/// `qr_idle_deadline` is an absolute point in time, carried across every call
/// from `run`'s loop and advanced only when a `Sample::Line` is actually
/// processed - never by a disk sample. An earlier version built a fresh
/// `Timer::after(QR_FLUSH_IDLE_TIMEOUT)` on every call instead, so each disk
/// sample - not real DepotDownloader output - restarted its 700ms window
/// before it could ever elapse, starving the QR flush indefinitely: the QR
/// code silently never appeared until something else (like the next QR
/// refresh's own heading) happened to end the block for an unrelated reason.
/// Racing the idle tick as the *first* argument to `or` also matters: once
/// `qr_idle_deadline` has passed, that guarantees it wins even in the instant
/// a line also happens to be ready, since `or` favors whichever argument it
/// polls first.
///
/// `cancel_rx` is also raced in here, rather than only drained at the top of
/// the loop the way disk samples are: a user-initiated pause/cancel click is
/// never frequent enough to risk the starvation class of bug fixed for disk
/// polling, and racing it keeps reaction time near-instant even in the
/// middle of a long QR wait.
async fn next_sample(
    line_rx: &async_channel::Receiver<String>,
    cancel_rx: &async_channel::Receiver<()>,
    qr_idle_deadline: Instant,
) -> Sample {
    let next_line = async {
        match line_rx.recv().await {
            Ok(line) => Sample::Line(line),
            Err(_) => Sample::LineChannelClosed,
        }
    };
    let idle_tick = async {
        smol::Timer::at(qr_idle_deadline).await;
        Sample::Idle
    };
    let cancel_tick = async {
        let _ = cancel_rx.recv().await;
        Sample::CancelRequested
    };
    futures_lite::future::or(cancel_tick, futures_lite::future::or(idle_tick, next_line)).await
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

async fn poll_disk_usage(dir: PathBuf, sink: async_channel::Sender<u64>) {
    eprintln!(
        "[depot-downloader-gpui] watching disk usage of {}",
        dir.display()
    );
    loop {
        let scan_dir = dir.clone();
        let size = smol::unblock(move || directory_size(&scan_dir)).await;
        if sink.send(size).await.is_err() {
            break;
        }
        smol::Timer::after(DISK_POLL_INTERVAL).await;
    }
}

fn directory_size(dir: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .filter_map(Result::ok)
        .map(|entry| {
            let Ok(metadata) = entry.metadata() else {
                return 0;
            };
            if metadata.is_dir() {
                directory_size(&entry.path())
            } else {
                metadata.len()
            }
        })
        .sum()
}
