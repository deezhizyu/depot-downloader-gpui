use serde::{Deserialize, Serialize};

/// One line of the fork's `-json` stdout (see the fork's `docs/json-mode.md`).
/// `t_ms` is the fork's own monotonic clock, so speeds stay correct even when
/// this app reads a batch of queued lines late.
#[derive(Debug, Clone, Deserialize)]
pub struct EventLine {
    pub t_ms: u64,
    #[serde(flatten)]
    pub event: Event,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    Log {
        level: LogLevel,
        message: String,
    },
    AuthPrompt {
        kind: AuthPromptKind,
        message: String,
    },
    Qr {
        url: String,
    },
    LoginSuccess {
        username: Option<String>,
    },
    Plan {
        total_compressed_bytes: u64,
        total_uncompressed_bytes: u64,
        total_files: u64,
    },
    DepotStart {
        depot_id: u64,
    },
    Progress {
        network_bytes: u64,
        written_bytes: u64,
        verified_bytes: u64,
        files_done: u64,
        #[serde(default)]
        current_file: Option<String>,
    },
    Done {
        network_bytes: u64,
        written_bytes: u64,
        verified_bytes: u64,
    },
    Error {
        message: String,
    },
    UserApps {
        apps: Vec<SteamApp>,
        count: u64,
    },
    Branches {
        app_id: u64,
        branches: Vec<BranchInfo>,
    },
    #[serde(other)]
    Ignored,
}

/// One entry of a `-list-user-apps` result: an app id, name, and Steam app
/// type (`Game`, `DLC`, `Tool`, `Demo`, `Application`, ...) the account's
/// licenses grant. `#[serde(default)]` tolerates a provisioned binary older
/// than the fork's `-app-type` addition, which omitted `type` entirely.
#[derive(Debug, Clone, Deserialize)]
pub struct SteamApp {
    pub app_id: u64,
    pub name: String,
    #[serde(rename = "type", default)]
    pub app_type: String,
}

impl SteamApp {
    pub fn is_type(&self, app_type: &str) -> bool {
        self.app_type.eq_ignore_ascii_case(app_type)
    }
}

/// One entry of a `-list-branches` result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BranchInfo {
    pub name: String,
    pub build_id: u64,
    pub time_updated: u64,
    pub password_required: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogLevel {
    Info,
    Warn,
    Error,
    Debug,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthPromptKind {
    Password,
    SteamGuardCode,
    EmailCode,
    DeviceConfirmation,
    Other,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(line: &str) -> EventLine {
        serde_json::from_str(line).expect("valid event line")
    }

    #[test]
    fn parses_progress_counters() {
        let line = parse(
            r#"{"event":"progress","t_ms":1500,"network_bytes":10,"written_bytes":20,"verified_bytes":30,"files_done":4,"current_file":"Squad/a.pak"}"#,
        );
        assert_eq!(line.t_ms, 1500);
        assert!(matches!(
            line.event,
            Event::Progress {
                network_bytes: 10,
                written_bytes: 20,
                verified_bytes: 30,
                files_done: 4,
                current_file: Some(_)
            }
        ));
    }

    #[test]
    fn parses_prompts_and_anonymous_login() {
        let prompt = parse(
            r#"{"event":"auth_prompt","t_ms":1,"kind":"email_code","message":"Enter the code"}"#,
        );
        assert!(matches!(
            prompt.event,
            Event::AuthPrompt {
                kind: AuthPromptKind::EmailCode,
                ..
            }
        ));
        let login = parse(r#"{"event":"login_success","t_ms":2,"username":null}"#);
        assert!(matches!(
            login.event,
            Event::LoginSuccess { username: None }
        ));
    }

    #[test]
    fn parses_user_apps_and_branches() {
        let line = parse(
            r#"{"event":"user_apps","t_ms":5,"apps":[{"app_id":730,"name":"Counter-Strike 2","type":"Game"}],"count":1}"#,
        );
        assert!(matches!(
            line.event,
            Event::UserApps { ref apps, count: 1 }
                if apps.len() == 1 && apps[0].app_id == 730 && apps[0].is_type("game")
        ));

        let line = parse(
            r#"{"event":"user_apps","t_ms":5,"apps":[{"app_id":730,"name":"Counter-Strike 2"}],"count":1}"#,
        );
        assert!(matches!(
            line.event,
            Event::UserApps { ref apps, .. } if apps[0].app_type.is_empty()
        ));

        let line = parse(
            r#"{"event":"branches","t_ms":6,"app_id":730,"branches":[{"name":"public","build_id":1,"time_updated":2,"password_required":false}]}"#,
        );
        assert!(matches!(
            line.event,
            Event::Branches { app_id: 730, ref branches } if branches.len() == 1 && branches[0].name == "public"
        ));
    }

    #[test]
    fn unknown_events_and_extra_fields_are_ignored() {
        let line = parse(r#"{"event":"app_info","t_ms":3,"app_id":440,"name":"x"}"#);
        assert!(matches!(line.event, Event::Ignored));
        let line = parse(
            r#"{"event":"error","t_ms":4,"code":"invalid_password","message":"Bad password"}"#,
        );
        assert!(matches!(line.event, Event::Error { .. }));
    }
}
