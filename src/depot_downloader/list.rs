use std::path::PathBuf;

use super::AuthPromptKind;
use super::child;
use super::event::{BranchInfo, Event, EventLine, SteamApp};
use super::process::{LoginMethod, push_login_args};

/// A login prompt surfacing during `run_list_user_apps` - the exact same
/// shapes `depot_downloader::process::run` produces mid-download, reused
/// here so `app::RunState`'s existing `ShowingQrCode`/`AwaitingSteamGuardCode`/
/// `AwaitingSteamGuardConfirmation` variants cover this flow with no new UI.
#[derive(Debug, Clone)]
pub enum ListPrompt {
    Qr { url: String },
    AuthPrompt { kind: AuthPromptKind, message: String },
}

pub enum UserAppsEvent {
    Prompt(ListPrompt),
    LoginSuccess { username: Option<String> },
    /// Steam asked for a password although a saved login was used - mirrors
    /// `DownloadStats::login_expired`.
    LoginExpired,
    Apps(Vec<SteamApp>),
    Error(String),
    Exited,
    FailedToStart(String),
}

/// Runs `-list-user-apps`, forwarding login prompts on `updates` exactly
/// like a download would (see `ListPrompt`) so the login UI can be reused
/// verbatim, and answering them with lines received on `respond`. Terminal
/// events are `Apps`, `Error`, or `Exited`. Intended to be driven from
/// `cx.background_executor()`.
pub async fn run_list_user_apps(
    depot_downloader_binary: PathBuf,
    login: LoginMethod,
    updates: async_channel::Sender<UserAppsEvent>,
    respond: async_channel::Receiver<String>,
) {
    let mut args = vec!["-list-user-apps".to_string(), "-json".to_string()];
    // -remember-password only errors without -username and without -qr,
    // which never happens here (Anonymous only fetches the free-apps list,
    // which never persists a login).
    if !matches!(login, LoginMethod::Anonymous) {
        args.push("-remember-password".to_string());
    }
    push_login_args(&mut args, &login);

    eprintln!(
        "[depot-downloader-gpui] launching {} {} (list user apps)",
        depot_downloader_binary.display(),
        args.join(" ")
    );

    let mut child = match child::spawn(&depot_downloader_binary, args) {
        Ok(child) => child,
        Err(error) => {
            eprintln!("[depot-downloader-gpui] failed to launch DepotDownloader: {error}");
            let _ = updates
                .send(UserAppsEvent::FailedToStart(format!(
                    "Could not start DepotDownloader: {error}"
                )))
                .await;
            return;
        }
    };

    let line_rx = child::stream_output(&mut child);
    child::forward_stdin(&mut child, respond);

    while let Ok(line) = line_rx.recv().await {
        let Ok(event_line) = serde_json::from_str::<EventLine>(&line) else {
            continue;
        };
        let outcome = match event_line.event {
            Event::Qr { url } => Some(UserAppsEvent::Prompt(ListPrompt::Qr { url })),
            Event::AuthPrompt { kind, message } => {
                if kind == AuthPromptKind::Password {
                    Some(UserAppsEvent::LoginExpired)
                } else {
                    Some(UserAppsEvent::Prompt(ListPrompt::AuthPrompt { kind, message }))
                }
            }
            Event::LoginSuccess { username } => Some(UserAppsEvent::LoginSuccess { username }),
            Event::UserApps { apps, count } => {
                eprintln!("[depot-downloader-gpui] account has {count} app(s)");
                Some(UserAppsEvent::Apps(apps))
            }
            Event::Error { message } => Some(UserAppsEvent::Error(message)),
            _ => None,
        };
        let Some(outcome) = outcome else { continue };
        let is_terminal = matches!(
            outcome,
            UserAppsEvent::Apps(_) | UserAppsEvent::Error(_) | UserAppsEvent::LoginExpired
        );
        if updates.send(outcome).await.is_err() || is_terminal {
            // Wait for the kill to actually take the process down (not just
            // request it) before returning - `run_credentialed` (app/
            // session.rs) releases its same-account lock the moment this
            // future resolves, and a process that's merely been asked to
            // die can still hold its Steam session open for a moment,
            // colliding with whatever real-account run comes next.
            let _ = child.kill();
            let _ = child.status().await;
            return;
        }
    }

    let status = child.status().await;
    eprintln!("[depot-downloader-gpui] DepotDownloader (list user apps) exited with {status:?}");
    let _ = updates.send(UserAppsEvent::Exited).await;
}

/// Runs `-list-branches` for `app_id` and returns the branch list, or `None`
/// on anything other than a clean `Branches` event (unexpected prompt,
/// error, early exit). Deliberately soft-fail and non-interactive: this only
/// ever reuses a login that's already good (remembered username or
/// anonymous), so the caller falls back to a single `"public"` entry rather
/// than this needing its own prompt UI.
pub async fn run_list_branches(
    depot_downloader_binary: PathBuf,
    app_id: u64,
    login: LoginMethod,
) -> Option<Vec<BranchInfo>> {
    let mut args = vec![
        "-app".to_string(),
        app_id.to_string(),
        "-list-branches".to_string(),
        "-json".to_string(),
    ];
    // A `RememberedUsername` login only reconnects silently with
    // `-remember-password` reasserted; without it DepotDownloader falls back
    // to an interactive password prompt (see `process::run`'s doc comment),
    // which this soft-fail path then treats as "give up" - so branches
    // silently fell back to a single "public" entry on every remembered
    // login.
    if !matches!(login, LoginMethod::Anonymous) {
        args.push("-remember-password".to_string());
    }
    push_login_args(&mut args, &login);

    eprintln!(
        "[depot-downloader-gpui] launching {} {} (list branches)",
        depot_downloader_binary.display(),
        args.join(" ")
    );

    let mut child = child::spawn(&depot_downloader_binary, args).ok()?;
    let line_rx = child::stream_output(&mut child);

    while let Ok(line) = line_rx.recv().await {
        let Ok(event_line) = serde_json::from_str::<EventLine>(&line) else {
            continue;
        };
        match event_line.event {
            Event::Branches { app_id: reported_app_id, branches } => {
                eprintln!(
                    "[depot-downloader-gpui] app {reported_app_id} has {} branch(es)",
                    branches.len()
                );
                for branch in &branches {
                    eprintln!(
                        "[depot-downloader-gpui]   {} (build {}, updated {}{})",
                        branch.name,
                        branch.build_id,
                        branch.time_updated,
                        if branch.password_required { ", password required" } else { "" }
                    );
                }
                // See `run_list_user_apps`'s matching comment: wait for the
                // process to actually die, not just for the kill request,
                // before handing the same-account lock back.
                let _ = child.kill();
                let _ = child.status().await;
                return Some(branches);
            }
            // `LoginSuccess` always arrives for a real-account login before
            // `Branches` - it is not a failure, unlike everything else the
            // wildcard arm below gives up on.
            Event::Log { .. } | Event::Ignored | Event::LoginSuccess { .. } => continue,
            // Anything else (a prompt, an error) means this login can't list
            // branches unattended - give up rather than hang waiting for
            // input nobody will provide.
            _ => {
                let _ = child.kill();
                let _ = child.status().await;
                return None;
            }
        }
    }
    None
}
