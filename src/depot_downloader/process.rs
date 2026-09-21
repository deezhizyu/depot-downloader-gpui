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
    UsernamePassword { username: String, password: String },
    Qr,
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
        let mut args = vec!["-app".to_string(), self.app_id.clone()];
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

pub enum ProcessEvent {
    Stats(DownloadStats),
    Exited(std::io::Result<ExitStatus>),
    FailedToStart(String),
}

/// Runs a DepotDownloader download to completion, sending a fresh
/// [`DownloadStats`] snapshot on `updates` after every meaningful change and
/// forwarding lines from `respond` (e.g. a typed Steam Guard code) to the
/// child's stdin. Intended to be driven from `cx.background_executor()`.
pub async fn run(
    request: DownloadRequest,
    updates: async_channel::Sender<ProcessEvent>,
    respond: async_channel::Receiver<String>,
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

    let mut child = match Command::new(&request.depot_downloader_binary)
        .args(request.build_args())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
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

    let has_disk_dir = request.download_dir.is_some();
    let (disk_tx, disk_rx) = async_channel::unbounded::<u64>();
    if let Some(dir) = request.download_dir.clone() {
        smol::spawn(poll_disk_usage(dir, disk_tx)).detach();
    }

    let mut parser = OutputParser::new();
    let mut tracker = ProgressTracker::new();

    loop {
        match next_sample(&line_rx, &disk_rx, has_disk_dir).await {
            Sample::Line(line) => {
                for event in parser.feed(&line) {
                    tracker.apply_event(event);
                }
            }
            Sample::DiskBytes(bytes) => tracker.apply_disk_sample(bytes, Instant::now()),
            Sample::Idle => {
                if let Some(event) = parser.flush_pending_qr_block() {
                    eprintln!("[depot-downloader-gpui] QR code ready (no further output arrived)");
                    tracker.apply_event(event);
                }
            }
            Sample::LineChannelClosed => break,
        }
        if updates
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
    DiskBytes(u64),
    Idle,
    LineChannelClosed,
}

async fn next_sample(
    line_rx: &async_channel::Receiver<String>,
    disk_rx: &async_channel::Receiver<u64>,
    has_disk_dir: bool,
) -> Sample {
    let next_line = async {
        match line_rx.recv().await {
            Ok(line) => Sample::Line(line),
            Err(_) => Sample::LineChannelClosed,
        }
    };
    let next_disk_sample = async {
        if !has_disk_dir {
            return std::future::pending::<Sample>().await;
        }
        match disk_rx.recv().await {
            Ok(bytes) => Sample::DiskBytes(bytes),
            Err(_) => std::future::pending::<Sample>().await,
        }
    };
    // Recreated fresh on every call, so it naturally debounces: any line or
    // disk sample arriving first cancels it, and it only ever fires after a
    // real idle gap.
    let idle_tick = async {
        smol::Timer::after(QR_FLUSH_IDLE_TIMEOUT).await;
        Sample::Idle
    };
    futures_lite::future::or(
        futures_lite::future::or(next_line, next_disk_sample),
        idle_tick,
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
