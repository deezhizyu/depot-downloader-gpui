use std::sync::LazyLock;

use regex::Regex;

/// A single meaningful thing DepotDownloader's console output told us.
#[derive(Debug, Clone, PartialEq)]
pub enum DownloadEvent {
    UsingBranch {
        branch: String,
    },
    ProcessingDepot {
        depot_id: u64,
    },
    DownloadingDepotManifest {
        depot_id: u64,
    },
    DownloadingDepot {
        depot_id: u64,
    },
    /// A depot's running percentage, from the line printed once per completed file.
    FileProgress {
        percent: f32,
        path: String,
    },
    /// The overall run percentage, from DepotDownloader's terminal progress
    /// escape sequence. This tracks *uncompressed* (on-disk) bytes.
    OverallProgress {
        percent: u8,
    },
    DepotFinished {
        depot_id: u64,
        compressed_bytes: u64,
        uncompressed_bytes: u64,
    },
    TotalDownloaded {
        compressed_bytes: u64,
        uncompressed_bytes: u64,
        depot_count: u32,
    },
    SteamGuardCodeRequested,
    SteamGuardEmailCodeRequested {
        email: String,
    },
    SteamGuardConfirmationRequested,
    QrCodeReady {
        ascii_art: String,
    },
    ErrorLine {
        message: String,
    },
}

struct Patterns {
    overall_progress: Regex,
    file_progress: Regex,
    using_branch: Regex,
    processing_depot: Regex,
    downloading_depot_manifest: Regex,
    downloading_depot: Regex,
    depot_finished: Regex,
    total_downloaded: Regex,
    steam_guard_email: Regex,
}

static PATTERNS: LazyLock<Patterns> = LazyLock::new(|| Patterns {
    // ESC ]9;4;{state};{progress} BEL
    overall_progress: Regex::new(r"\x1b\]9;4;\d+;(\d{1,3})\x07").unwrap(),
    file_progress: Regex::new(r"^\s*(\d+\.\d{2})%\s+(.+)$").unwrap(),
    using_branch: Regex::new(r"^Using app branch: '(.+)'\.$").unwrap(),
    processing_depot: Regex::new(r"^Processing depot (\d+)$").unwrap(),
    downloading_depot_manifest: Regex::new(r"^Downloading depot (\d+) manifest$").unwrap(),
    downloading_depot: Regex::new(r"^Downloading depot (\d+)$").unwrap(),
    depot_finished: Regex::new(
        r"^Depot (\d+) - Downloaded (\d+) bytes \((\d+) bytes uncompressed\)$",
    )
    .unwrap(),
    total_downloaded: Regex::new(
        r"^Total downloaded: (\d+) bytes \((\d+) bytes uncompressed\) from (\d+) depots$",
    )
    .unwrap(),
    steam_guard_email: Regex::new(
        r"^STEAM GUARD! Please enter the auth code sent to the email at (.+):$",
    )
    .unwrap(),
});

/// Turns DepotDownloader's raw stdout lines into [`DownloadEvent`]s.
///
/// Kept as a small stateful struct (rather than a free function) only because
/// the QR login prompt spans multiple lines: everything after "Use the Steam
/// Mobile App to sign in with this QR code:" up to the first line that isn't
/// QR art is one ASCII-art block, and DepotDownloader never prints the login
/// URL as text. In practice DepotDownloader then blocks waiting for the scan
/// and never prints a line to mark the block's end, so the caller must also
/// call [`Self::flush_pending_qr_block`] after a short idle period.
pub struct OutputParser {
    collecting_qr_lines: Option<Vec<String>>,
}

impl OutputParser {
    pub fn new() -> Self {
        Self {
            collecting_qr_lines: None,
        }
    }

    pub fn feed(&mut self, line: &str) -> Vec<DownloadEvent> {
        if let Some(qr_lines) = &mut self.collecting_qr_lines {
            if is_qr_art_line(line) {
                qr_lines.push(line.to_string());
                return Vec::new();
            }
            let ascii_art = qr_lines.join("\n");
            self.collecting_qr_lines = None;
            let mut events = vec![DownloadEvent::QrCodeReady { ascii_art }];
            events.extend(self.feed(line));
            return events;
        }

        if line.contains("Use the Steam Mobile App to sign in with this QR code:") {
            self.collecting_qr_lines = Some(Vec::new());
            return Vec::new();
        }

        let mut events = Vec::new();
        for capture in PATTERNS.overall_progress.captures_iter(line) {
            if let Ok(percent) = capture[1].parse::<u8>() {
                events.push(DownloadEvent::OverallProgress {
                    percent: percent.min(100),
                });
            }
        }
        let text_without_escapes = PATTERNS.overall_progress.replace_all(line, "");
        let text = text_without_escapes.trim();
        if text.is_empty() {
            return events;
        }

        if let Some(event) = parse_plain_line(text) {
            events.push(event);
        }
        events
    }

    /// DepotDownloader prints the QR code and then, while it waits for the
    /// phone to confirm the scan, goes silent - it never prints a further
    /// line to signal that the block ended. The caller should invoke this
    /// after a short period with no new output, to show the QR code even
    /// though nothing marks its end textually. A no-op if no QR block is
    /// currently being collected, or nothing has arrived for it yet.
    pub fn flush_pending_qr_block(&mut self) -> Option<DownloadEvent> {
        let qr_lines = self.collecting_qr_lines.as_ref()?;
        if qr_lines.is_empty() {
            return None;
        }
        let ascii_art = qr_lines.join("\n");
        self.collecting_qr_lines = None;
        Some(DownloadEvent::QrCodeReady { ascii_art })
    }
}

/// QRCoder (which DepotDownloader uses to draw the QR code) renders every
/// dark module as the same repeated character and every light module as a
/// space, including whole quiet-zone rows that are nothing but spaces - so
/// "blank" is not a valid end-of-block signal. This deliberately does not
/// check for the literal `'█'` glyph: on some platforms DepotDownloader's
/// stdout for that character does not arrive as valid UTF-8 (observed as the
/// Unicode replacement character once decoded), so what a real QR row's dark
/// module decodes to isn't always predictable - only that it stays one single
/// consistent character throughout the row. A line with more than one
/// distinct non-space character is real text (an actual log line), which
/// ends the block.
fn is_qr_art_line(line: &str) -> bool {
    line.chars()
        .filter(|c| !c.is_whitespace())
        .collect::<std::collections::HashSet<_>>()
        .len()
        <= 1
}

fn parse_plain_line(line: &str) -> Option<DownloadEvent> {
    let patterns = &*PATTERNS;

    if let Some(captures) = patterns.using_branch.captures(line) {
        return Some(DownloadEvent::UsingBranch {
            branch: captures[1].to_string(),
        });
    }
    if let Some(captures) = patterns.downloading_depot_manifest.captures(line) {
        return Some(DownloadEvent::DownloadingDepotManifest {
            depot_id: captures[1].parse().ok()?,
        });
    }
    if let Some(captures) = patterns.processing_depot.captures(line) {
        return Some(DownloadEvent::ProcessingDepot {
            depot_id: captures[1].parse().ok()?,
        });
    }
    if let Some(captures) = patterns.downloading_depot.captures(line) {
        return Some(DownloadEvent::DownloadingDepot {
            depot_id: captures[1].parse().ok()?,
        });
    }
    if let Some(captures) = patterns.depot_finished.captures(line) {
        return Some(DownloadEvent::DepotFinished {
            depot_id: captures[1].parse().ok()?,
            compressed_bytes: captures[2].parse().ok()?,
            uncompressed_bytes: captures[3].parse().ok()?,
        });
    }
    if let Some(captures) = patterns.total_downloaded.captures(line) {
        return Some(DownloadEvent::TotalDownloaded {
            compressed_bytes: captures[1].parse().ok()?,
            uncompressed_bytes: captures[2].parse().ok()?,
            depot_count: captures[3].parse().ok()?,
        });
    }
    if let Some(captures) = patterns.steam_guard_email.captures(line) {
        return Some(DownloadEvent::SteamGuardEmailCodeRequested {
            email: captures[1].to_string(),
        });
    }
    if line.starts_with("STEAM GUARD!") && line.contains("2-factor auth code") {
        return Some(DownloadEvent::SteamGuardCodeRequested);
    }
    if line.starts_with("STEAM GUARD!") && line.contains("confirm your sign in") {
        return Some(DownloadEvent::SteamGuardConfirmationRequested);
    }
    if line.starts_with("Error") || line.contains("Encountered") && line.contains("Aborting") {
        return Some(DownloadEvent::ErrorLine {
            message: line.to_string(),
        });
    }
    if let Some(captures) = patterns.file_progress.captures(line) {
        return Some(DownloadEvent::FileProgress {
            percent: captures[1].parse().ok()?,
            path: captures[2].to_string(),
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_overall_progress_escape_sequence() {
        let mut parser = OutputParser::new();
        let line = "\x1b]9;4;1;42\x07";
        let events = parser.feed(line);
        assert_eq!(events, vec![DownloadEvent::OverallProgress { percent: 42 }]);
    }

    #[test]
    fn parses_per_file_progress_line() {
        let mut parser = OutputParser::new();
        let events = parser.feed(" 12.34% shared/pak01.vpk");
        assert_eq!(
            events,
            vec![DownloadEvent::FileProgress {
                percent: 12.34,
                path: "shared/pak01.vpk".into()
            }]
        );
    }

    #[test]
    fn parses_depot_and_total_summaries() {
        let mut parser = OutputParser::new();
        assert_eq!(
            parser.feed("Depot 228990 - Downloaded 104857600 bytes (209715200 bytes uncompressed)"),
            vec![DownloadEvent::DepotFinished {
                depot_id: 228990,
                compressed_bytes: 104857600,
                uncompressed_bytes: 209715200,
            }]
        );
        assert_eq!(
            parser.feed(
                "Total downloaded: 104857600 bytes (209715200 bytes uncompressed) from 1 depots"
            ),
            vec![DownloadEvent::TotalDownloaded {
                compressed_bytes: 104857600,
                uncompressed_bytes: 209715200,
                depot_count: 1,
            }]
        );
    }

    #[test]
    fn collects_multi_line_qr_code_block_despite_blank_quiet_zone_rows() {
        let mut parser = OutputParser::new();
        assert!(
            parser
                .feed("Use the Steam Mobile App to sign in with this QR code:")
                .is_empty()
        );
        // The QR's top quiet zone is a row made entirely of the whitespace
        // module string - this must not be mistaken for the end of the block.
        assert!(parser.feed("            ").is_empty());
        assert!(parser.feed("█████████").is_empty());
        assert!(parser.feed("██   █   ██").is_empty());
        // The first line with real text ends the block and is itself parsed.
        let events = parser.feed("Processing depot 123");
        assert_eq!(
            events,
            vec![
                DownloadEvent::QrCodeReady {
                    ascii_art: "            \n█████████\n██   █   ██".into()
                },
                DownloadEvent::ProcessingDepot { depot_id: 123 },
            ]
        );
    }

    #[test]
    fn collects_qr_code_block_whose_dark_module_is_not_the_expected_glyph() {
        // On some platforms DepotDownloader's dark-module character does not
        // survive as valid UTF-8 and decodes to the replacement character
        // instead of '█' - the block must still be recognized as QR art
        // rather than mistaken for a real log line ending it early.
        let mut parser = OutputParser::new();
        assert!(
            parser
                .feed("Use the Steam Mobile App to sign in with this QR code:")
                .is_empty()
        );
        assert!(
            parser
                .feed("\u{FFFD}\u{FFFD}   \u{FFFD}\u{FFFD}")
                .is_empty()
        );
        let events = parser.feed("Processing depot 123");
        assert_eq!(
            events,
            vec![
                DownloadEvent::QrCodeReady {
                    ascii_art: "\u{FFFD}\u{FFFD}   \u{FFFD}\u{FFFD}".into()
                },
                DownloadEvent::ProcessingDepot { depot_id: 123 },
            ]
        );
    }

    #[test]
    fn flushes_qr_block_when_depot_downloader_goes_silent() {
        // DepotDownloader prints the QR then blocks waiting for the phone
        // scan, so no line ever marks the block's end - only an explicit
        // idle-flush (driven by a timer in the process supervisor) does.
        let mut parser = OutputParser::new();
        assert!(
            parser
                .feed("Use the Steam Mobile App to sign in with this QR code:")
                .is_empty()
        );
        assert!(parser.feed("            ").is_empty());
        assert!(parser.feed("█████████").is_empty());

        assert_eq!(
            parser.flush_pending_qr_block(),
            Some(DownloadEvent::QrCodeReady {
                ascii_art: "            \n█████████".into()
            })
        );
        // Flushing is one-shot; nothing left to flush a second time.
        assert_eq!(parser.flush_pending_qr_block(), None);
    }

    #[test]
    fn does_not_flush_before_the_heading_or_before_any_line_arrives() {
        let mut parser = OutputParser::new();
        assert_eq!(parser.flush_pending_qr_block(), None);

        assert!(
            parser
                .feed("Use the Steam Mobile App to sign in with this QR code:")
                .is_empty()
        );
        assert_eq!(parser.flush_pending_qr_block(), None);
    }

    #[test]
    fn recognizes_steam_guard_prompts() {
        let mut parser = OutputParser::new();
        assert_eq!(
            parser.feed(
                "STEAM GUARD! Please enter your 2-factor auth code from your authenticator app: "
            ),
            vec![DownloadEvent::SteamGuardCodeRequested]
        );
        assert_eq!(
            parser.feed(
                "STEAM GUARD! Please enter the auth code sent to the email at ab***@example.com:"
            ),
            vec![DownloadEvent::SteamGuardEmailCodeRequested {
                email: "ab***@example.com".into()
            }]
        );
    }
}
