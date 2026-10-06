//! Native hook protocol. Only agent and lifecycle state cross the pane channel.
#[cfg(any(windows, test))]
use crate::gt::{AgentSignal, GtMessage};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::Path;

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StateEvent {
    agent: String,
    state: String,
}

#[cfg(any(windows, test))]
pub(crate) fn parse_event(bytes: &[u8]) -> Option<GtMessage> {
    if bytes.len() > 128 {
        return None;
    }
    let event: StateEvent = serde_json::from_slice(bytes).ok()?;
    let agent = match event.agent.as_str() {
        "claude" => "Claude Code",
        "codex" => "Codex",
        _ => return None,
    };
    let signal = match event.state.as_str() {
        "session_start" => AgentSignal::SessionStart,
        "blocked" => AgentSignal::Blocked,
        "done" => AgentSignal::Done,
        "session_end" => AgentSignal::SessionEnd,
        _ => return None,
    };
    Some(GtMessage::State {
        agent: agent.into(),
        signal,
        detail: None,
    })
}

/// stdin is parsed but never copied into the outbound message or logs.
pub fn event_from_hook(agent: &str, bytes: &[u8]) -> Option<Vec<u8>> {
    if !matches!(agent, "claude" | "codex") || bytes.len() > 256 * 1024 {
        return None;
    }
    let input: Value = serde_json::from_slice(bytes).ok()?;
    let state = match input.get("hook_event_name")?.as_str()? {
        "SessionStart" => "session_start",
        "PermissionRequest" => "blocked",
        "Notification" if agent == "claude" => match input.get("notification_type")?.as_str()? {
            "permission_prompt" | "idle_prompt" | "elicitation_dialog" => "blocked",
            _ => return None,
        },
        "Stop" => "done",
        "SessionEnd" => "session_end",
        _ => return None,
    };
    serde_json::to_vec(&StateEvent {
        agent: agent.into(),
        state: state.into(),
    })
    .ok()
}

fn merge_hooks(mut value: Value, agent: &str, exe: &Path, remove: bool) -> Result<Value, String> {
    let root = value
        .as_object_mut()
        .ok_or("設定のルートはJSONオブジェクトである必要があります")?;
    let hooks = root
        .entry("hooks")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or("hooksがオブジェクトではありません")?;
    let exe = exe.to_str().ok_or("実行ファイルのパスが不正です")?;
    let command = if agent == "claude" {
        format!("& '{}' claude", exe.replace('\'', "''"))
    } else {
        format!("\"{exe}\" codex")
    };
    let events: &[&str] = if agent == "claude" {
        &[
            "SessionStart",
            "PermissionRequest",
            "Notification",
            "Stop",
            "SessionEnd",
        ]
    } else {
        &["SessionStart", "PermissionRequest", "Stop", "SessionEnd"]
    };
    for &event in events {
        let groups = hooks
            .entry(event)
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .ok_or_else(|| format!("{event}のフックが配列ではありません"))?;
        for group in groups.iter_mut() {
            let handlers = group
                .get_mut("hooks")
                .and_then(Value::as_array_mut)
                .ok_or("フック定義が不正です")?;
            if remove {
                handlers.retain(|handler| {
                    let field = if agent == "claude" {
                        "command"
                    } else {
                        "commandWindows"
                    };
                    handler.get(field).and_then(Value::as_str) != Some(command.as_str())
                });
            }
        }
        if remove {
            groups.retain(|group| !group["hooks"].as_array().unwrap().is_empty());
        }
        let field = if agent == "claude" {
            "command"
        } else {
            "commandWindows"
        };
        let exists =
            groups.iter().any(|group| {
                group["hooks"].as_array().unwrap().iter().any(|handler| {
                    handler.get(field).and_then(Value::as_str) == Some(command.as_str())
                })
            });
        if !remove && !exists {
            let handler = if agent == "claude" {
                json!({"type":"command", "shell":"powershell", "command":command, "timeout":2})
            } else {
                json!({"type":"command", "command":"true", "commandWindows":command, "timeout":2})
            };
            groups.push(if event == "Notification" { json!({"matcher":"permission_prompt|idle_prompt|elicitation_dialog", "hooks":[handler]}) } else { json!({"hooks":[handler]}) });
        }
    }
    Ok(value)
}

/// Explicit, per-project opt-in. Preflight both files before writing either.
pub fn setup(project: &Path, exe: &Path, agents: &[&str], remove: bool) -> Result<(), String> {
    let mut writes = Vec::new();
    for &agent in agents {
        let relative = match agent {
            "claude" => ".claude/settings.local.json",
            "codex" => ".codex/hooks.json",
            _ => return Err("agentはclaudeかcodexです".into()),
        };
        let path = project.join(relative);
        let original = match std::fs::read(&path) {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.to_string()),
        };
        if remove && original.is_none() {
            continue;
        }
        let value = match &original {
            Some(bytes) => serde_json::from_slice(bytes)
                .map_err(|_| format!("{}のJSONが壊れています。変更しません", path.display()))?,
            None => json!({}),
        };
        let merged = merge_hooks(value.clone(), agent, exe, remove)?;
        if merged != value {
            writes.push((
                path,
                original,
                serde_json::to_vec_pretty(&merged).map_err(|error| error.to_string())?,
            ));
        }
    }
    for (path, original, bytes) in writes {
        std::fs::create_dir_all(path.parent().unwrap()).map_err(|error| error.to_string())?;
        if let Some(original) = original {
            // Never overwrite a previous backup.
            let backup = path.with_extension("json.gototerm-backup");
            if !backup.exists() {
                std::fs::write(backup, original).map_err(|error| error.to_string())?;
            }
        }
        let temp = path.with_extension("json.gototerm-tmp");
        std::fs::write(&temp, bytes)
            .and_then(|_| std::fs::rename(temp, path))
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hook_payload_does_not_forward_private_fields_or_decisions() {
        let bytes = event_from_hook("codex", br#"{"hook_event_name":"PermissionRequest","tool_input":{"command":"secret"},"prompt":"private"}"#).unwrap();
        assert_eq!(
            std::str::from_utf8(&bytes).unwrap(),
            r#"{"agent":"codex","state":"blocked"}"#
        );
        assert!(matches!(
            parse_event(&bytes),
            Some(GtMessage::State {
                signal: AgentSignal::Blocked,
                detail: None,
                ..
            })
        ));
        assert!(
            parse_event(br#"{"agent":"codex","state":"blocked","detail":"private"}"#).is_none()
        );
        assert!(parse_event(br#"{"agent":"other","state":"done"}"#).is_none());
        assert!(event_from_hook(
            "claude",
            br#"{"hook_event_name":"Notification","notification_type":"auth_success"}"#
        )
        .is_none());
    }
    #[test]
    fn setup_preserves_user_hooks_is_idempotent_and_removes_only_ours() {
        let original = json!({"permissions":{"allow":["Read"]},"hooks":{"Stop":[{"matcher":"", "hooks":[{"type":"command","command":"custom-hook"}]}]}});
        for agent in ["claude", "codex"] {
            let exe = Path::new("C:\\Program Files\\gototerm\\gototerm-hook.exe");
            let merged = merge_hooks(original.clone(), agent, exe, false).unwrap();
            assert_eq!(merged["permissions"], original["permissions"]);
            assert_eq!(merged["hooks"]["Stop"][0], original["hooks"]["Stop"][0]);
            assert_eq!(
                merge_hooks(merged.clone(), agent, exe, false).unwrap(),
                merged
            );
            let removed = merge_hooks(merged, agent, exe, true).unwrap();
            assert_eq!(removed["hooks"]["Stop"], original["hooks"]["Stop"]);
        }
        assert!(merge_hooks(
            json!({"hooks":{"Stop":42}}),
            "codex",
            Path::new("hook.exe"),
            false
        )
        .is_err());
    }
}
