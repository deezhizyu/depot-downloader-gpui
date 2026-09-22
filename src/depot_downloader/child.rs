use std::path::Path;

use async_process::{Child, Command, Stdio};
use futures_lite::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

/// Builds the `Command` DepotDownloader is always launched with: piped
/// stdio, killed if dropped, and run from its own install directory (see
/// `process::run`'s doc comment for why `current_dir` is pinned there rather
/// than left to whatever directory the GUI happened to be launched from).
pub(super) fn spawn(binary: &Path, args: Vec<String>) -> std::io::Result<Child> {
    let mut command = Command::new(binary);
    command
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(install_dir) = binary.parent() {
        command.current_dir(install_dir);
    }
    command.spawn()
}

/// Forwards the child's stdout and stderr, line by line, onto one channel -
/// see `forward_lines` for why this reads raw bytes rather than using
/// `AsyncBufReadExt::lines()`. Takes `stdout`/`stderr` from `child`; panics
/// if either was already taken (both are always piped by `spawn`).
pub(super) fn stream_output(child: &mut Child) -> async_channel::Receiver<String> {
    let stdout = child.stdout.take().expect("stdout was piped");
    let stderr = child.stderr.take().expect("stderr was piped");

    let (line_tx, line_rx) = async_channel::unbounded::<String>();
    smol::spawn(forward_lines(stdout, line_tx.clone())).detach();
    smol::spawn(forward_lines(stderr, line_tx.clone())).detach();
    drop(line_tx);
    line_rx
}

/// Forwards lines received on `respond` (e.g. a typed Steam Guard code) to
/// the child's stdin. Takes `stdin` from `child`; panics if it was already
/// taken (always piped by `spawn`).
pub(super) fn forward_stdin(child: &mut Child, respond: async_channel::Receiver<String>) {
    let mut stdin = child.stdin.take().expect("stdin was piped");
    smol::spawn(async move {
        while let Ok(line) = respond.recv().await {
            let _ = stdin.write_all(line.as_bytes()).await;
            let _ = stdin.write_all(b"\n").await;
            let _ = stdin.flush().await;
        }
    })
    .detach();
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
