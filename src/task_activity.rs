use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use crate::gt::{AgentSignal, GtMessage};

static NEXT_PANE_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub(crate) struct PaneId(pub(crate) u64);

impl PaneId {
    pub(crate) fn allocate() -> Self {
        let id = NEXT_PANE_ID
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .expect("pane ID space exhausted");
        Self(id)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ActivityKind {
    NoSignal,
    SessionStarted,
    FileChanged,
    Blocked,
    ResponseEnded,
    SessionEnded,
}

#[derive(Clone, Debug)]
pub(crate) struct PaneActivity {
    pub(crate) kind: ActivityKind,
    pub(crate) agent: Option<String>,
    pub(crate) detail: Option<String>,
    pub(crate) received_at: Option<Instant>,
}

impl Default for PaneActivity {
    fn default() -> Self {
        Self {
            kind: ActivityKind::NoSignal,
            agent: None,
            detail: None,
            received_at: None,
        }
    }
}

impl PaneActivity {
    pub(crate) fn apply(&mut self, message: &GtMessage, now: Instant) {
        match message {
            GtMessage::FileChunk { .. } => return,
            GtMessage::Event { kind, path, tool } => {
                if matches!(
                    self.kind,
                    ActivityKind::NoSignal | ActivityKind::SessionEnded
                ) {
                    self.agent = None;
                }
                let mut parts = vec![path.to_string_lossy().into_owned(), format!("{kind:?}")];
                if let Some(tool) = tool {
                    parts.push(tool.clone());
                }
                self.kind = ActivityKind::FileChanged;
                self.detail = Some(sanitize_display_text(&parts.join(" / "), 512));
            }
            GtMessage::State {
                agent,
                signal,
                detail,
            } => {
                self.kind = match signal {
                    AgentSignal::SessionStart => ActivityKind::SessionStarted,
                    AgentSignal::Blocked => ActivityKind::Blocked,
                    AgentSignal::Done => ActivityKind::ResponseEnded,
                    AgentSignal::SessionEnd => ActivityKind::SessionEnded,
                };
                self.agent = Some(sanitize_display_text(agent, 80));
                self.detail = match signal {
                    AgentSignal::Blocked => detail
                        .as_deref()
                        .map(|text| sanitize_display_text(text, 512)),
                    AgentSignal::SessionStart | AgentSignal::Done | AgentSignal::SessionEnd => None,
                };
            }
        }
        self.received_at = Some(now);
    }
}

pub(crate) fn sanitize_display_text(text: &str, max_chars: usize) -> String {
    text.chars()
        .map(|ch| if is_unsafe_display_char(ch) { ' ' } else { ch })
        .take(max_chars)
        .collect()
}

fn is_unsafe_display_char(ch: char) -> bool {
    ch.is_control()
        || matches!(
            ch,
            '\u{061c}'
                | '\u{200e}'
                | '\u{200f}'
                | '\u{202a}'..='\u{202e}'
                | '\u{2066}'..='\u{2069}'
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gt::{AgentSignal, GtMessage};
    use crate::watcher::ChangeKind;
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    #[test]
    fn default_reports_no_signal() {
        let activity = PaneActivity::default();
        assert_eq!(activity.kind, ActivityKind::NoSignal);
        assert_eq!(activity.agent, None);
        assert_eq!(activity.detail, None);
        assert_eq!(activity.received_at, None);
    }

    #[test]
    fn done_replaces_blocked_and_clears_reason() {
        let now = Instant::now();
        let mut activity = PaneActivity::default();
        activity.apply(
            &GtMessage::State {
                agent: "example".into(),
                signal: AgentSignal::Blocked,
                detail: Some("確認が必要".into()),
            },
            now,
        );
        activity.apply(
            &GtMessage::State {
                agent: "example".into(),
                signal: AgentSignal::Done,
                detail: None,
            },
            now + Duration::from_secs(1),
        );
        assert_eq!(activity.kind, ActivityKind::ResponseEnded);
        assert_eq!(activity.detail, None);
        assert_eq!(activity.received_at, Some(now + Duration::from_secs(1)));
    }

    #[test]
    fn file_transfer_does_not_imply_activity() {
        let mut activity = PaneActivity::default();
        activity.apply(
            &GtMessage::FileChunk {
                path: "x.md".into(),
                seq: 0,
                last: true,
                data: b"text".to_vec(),
            },
            Instant::now(),
        );
        assert_eq!(activity.kind, ActivityKind::NoSignal);
        assert_eq!(activity.received_at, None);
    }

    #[test]
    fn state_signals_replace_kind_agent_and_detail() {
        let now = Instant::now();
        let cases = [
            (
                AgentSignal::SessionStart,
                ActivityKind::SessionStarted,
                None,
            ),
            (
                AgentSignal::Blocked,
                ActivityKind::Blocked,
                Some("入力してください"),
            ),
            (AgentSignal::Done, ActivityKind::ResponseEnded, None),
            (AgentSignal::SessionEnd, ActivityKind::SessionEnded, None),
        ];

        for (offset, (signal, expected, detail)) in cases.into_iter().enumerate() {
            let mut activity = PaneActivity::default();
            let received = now + Duration::from_secs(offset as u64);
            activity.apply(
                &GtMessage::State {
                    agent: "tool".into(),
                    signal,
                    detail: detail.map(str::to_owned),
                },
                received,
            );
            assert_eq!(activity.kind, expected);
            assert_eq!(activity.agent.as_deref(), Some("tool"));
            assert_eq!(activity.detail.as_deref(), detail);
            assert_eq!(activity.received_at, Some(received));
        }
    }

    #[test]
    fn event_preserves_active_agent_and_records_event_detail() {
        let now = Instant::now();
        let mut activity = PaneActivity::default();
        activity.apply(
            &GtMessage::State {
                agent: "codex".into(),
                signal: AgentSignal::SessionStart,
                detail: None,
            },
            now,
        );
        activity.apply(
            &GtMessage::Event {
                kind: ChangeKind::Modified,
                path: PathBuf::from("src/main.rs"),
                tool: Some("Edit".into()),
            },
            now + Duration::from_secs(4),
        );
        assert_eq!(activity.kind, ActivityKind::FileChanged);
        assert_eq!(activity.agent.as_deref(), Some("codex"));
        assert_eq!(
            activity.detail.as_deref(),
            Some("src/main.rs / Modified / Edit")
        );
        assert_eq!(activity.received_at, Some(now + Duration::from_secs(4)));
    }

    #[test]
    fn event_without_active_session_does_not_guess_agent() {
        let now = Instant::now();
        for after_end in [false, true] {
            let mut activity = PaneActivity::default();
            if after_end {
                activity.apply(
                    &GtMessage::State {
                        agent: "claude".into(),
                        signal: AgentSignal::SessionEnd,
                        detail: None,
                    },
                    now,
                );
            }
            activity.apply(
                &GtMessage::Event {
                    kind: ChangeKind::New,
                    path: PathBuf::from("new.txt"),
                    tool: None,
                },
                now + Duration::from_secs(1),
            );
            assert_eq!(activity.agent, None);
        }
    }

    #[test]
    fn repeated_signal_refreshes_received_time() {
        let now = Instant::now();
        let mut activity = PaneActivity::default();
        let message = GtMessage::State {
            agent: "codex".into(),
            signal: AgentSignal::Done,
            detail: None,
        };
        activity.apply(&message, now);
        activity.apply(&message, now + Duration::from_secs(8));
        assert_eq!(activity.received_at, Some(now + Duration::from_secs(8)));
    }

    #[test]
    fn display_text_is_sanitized_and_limited() {
        let mut activity = PaneActivity::default();
        activity.apply(
            &GtMessage::State {
                agent: format!("a\n\u{202e}{}", "x".repeat(100)),
                signal: AgentSignal::Blocked,
                detail: Some(format!("d\u{009b}\r{}", "y".repeat(600))),
            },
            Instant::now(),
        );
        let agent = activity.agent.unwrap();
        let detail = activity.detail.unwrap();
        assert_eq!(agent.chars().count(), 80);
        assert_eq!(detail.chars().count(), 512);
        assert!(!agent.chars().any(is_unsafe_display_char));
        assert!(!detail.chars().any(is_unsafe_display_char));
        assert!(agent.starts_with("a  "));
        assert!(detail.starts_with("d  "));
    }

    #[test]
    fn pane_ids_are_unique_and_monotonic() {
        let first = PaneId::allocate();
        let second = PaneId::allocate();
        assert!(second.0 > first.0);
    }
}
