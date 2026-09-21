use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::time::{Duration, Instant};

use async_process::{Command, Stdio};
use futures_lite::StreamExt;
use futures_lite::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use super::parser::OutputParser;
use super::progress::{DownloadStats, ProgressTracker};

/// How often we re-measure the download directory's size on disk.
const DISK_POLL_INTERVAL: Duration = Duration::from_millis(500);

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

    let mut child = match Command::new(&request.depot_downloader_binary)
        .args(request.build_args())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
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
    let _ = updates.send(ProcessEvent::Exited(status)).await;
}

enum Sample {
    Line(String),
    DiskBytes(u64),
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
    futures_lite::future::or(next_line, next_disk_sample).await
}

async fn forward_lines(
    reader: impl futures_lite::AsyncRead + Unpin,
    sink: async_channel::Sender<String>,
) {
    let mut lines = BufReader::new(reader).lines();
    while let Some(Ok(line)) = lines.next().await {
        if sink.send(line).await.is_err() {
            break;
        }
    }
}

async fn poll_disk_usage(dir: PathBuf, sink: async_channel::Sender<u64>) {
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
