use serde::Deserialize;

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
    #[serde(other)]
    Ignored,
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
    fn unknown_events_and_extra_fields_are_ignored() {
        let line = parse(r#"{"event":"app_info","t_ms":3,"app_id":440,"name":"x"}"#);
        assert!(matches!(line.event, Event::Ignored));
        let line = parse(
            r#"{"event":"error","t_ms":4,"code":"invalid_password","message":"Bad password"}"#,
        );
        assert!(matches!(line.event, Event::Error { .. }));
    }
}
