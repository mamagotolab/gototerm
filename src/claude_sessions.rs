//! Claude Code の過去セッションを、**起動する前に**一覧するための読み取り。
//!
//! Claude Code は会話を `~/.claude/projects/<作業フォルダを符号化した名前>/<uuid>.jsonl`
//! に残す。ランチャーはこれを読んで「前回の続き」「履歴から選ぶ」を出す。
//! 本体は数MBになるので**先頭だけ**読む（見出しに要る情報は冒頭に出てくる）。

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// 見出しを探すために読む行数。`custom-title` も最初の発言も冒頭に来る。
const HEAD_LINES: usize = 40;
/// 見出しとして持っておく最大文字数（表示側でさらに幅に合わせて詰める）。
const LABEL_CHARS: usize = 60;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Session {
    /// `claude -r <id>` に渡すセッションID（ファイル名から取る）。
    pub id: String,
    /// 一覧に出す見出し。
    pub label: String,
    /// `claude -n` で付けた名前か。false なら最初の発言から起こした代用の見出し。
    pub named: bool,
    pub modified: SystemTime,
}

/// `dir` で過去に開いたセッションを、新しい順に最大 `limit` 件返す。
/// 履歴が無い・読めないときは空（呼び出し側は行を出さない）。
pub fn recent_for_dir(dir: &Path, limit: usize) -> Vec<Session> {
    match project_dir_for(dir) {
        Some(project_dir) => sessions_in(&project_dir, limit),
        None => Vec::new(),
    }
}

/// 作業フォルダ → Claude Code が会話を置くフォルダ。
fn project_dir_for(dir: &Path) -> Option<PathBuf> {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)?;
    Some(
        home.join(".claude")
            .join("projects")
            .join(encode_dir(dir)),
    )
}

/// 作業フォルダのパスを、Claude Code が使うフォルダ名に符号化する。
/// 規則は「英数字以外をすべて `-` に置き換える」
/// （例: `/home/user/work` → `-home-user-work`、`/home/user/.claude` → `-home-user--claude`）。
fn encode_dir(dir: &Path) -> String {
    dir.to_string_lossy()
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
        .collect()
}

/// 会話フォルダの中を、更新の新しい順に読む。
fn sessions_in(project_dir: &Path, limit: usize) -> Vec<Session> {
    let Ok(entries) = std::fs::read_dir(project_dir) else {
        return Vec::new();
    };

    // 先に更新時刻だけで絞る。中身を読むのは表示する分（limit 件）だけ。
    let mut files: Vec<(SystemTime, PathBuf)> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                return None;
            }
            let modified = entry.metadata().ok()?.modified().ok()?;
            Some((modified, path))
        })
        .collect();
    files.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));

    files
        .into_iter()
        .filter_map(|(modified, path)| read_session(&path, modified))
        .take(limit)
        .collect()
}

/// 1セッション分の見出しを取り出す。見出しが何も無いもの（発言前に閉じた等）は捨てる。
fn read_session(path: &Path, modified: SystemTime) -> Option<Session> {
    let id = path.file_stem()?.to_string_lossy().into_owned();
    let file = std::fs::File::open(path).ok()?;

    let mut fallback: Option<String> = None;
    for line in BufReader::new(file).lines().take(HEAD_LINES).map_while(Result::ok) {
        let Ok(record) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        match record.get("type").and_then(|t| t.as_str()) {
            // `claude -n <名前>` で付けた名前。これがあれば即採用。
            Some("custom-title") => {
                if let Some(title) = record.get("customTitle").and_then(|t| t.as_str()) {
                    return Some(Session {
                        id,
                        label: tidy(title),
                        named: true,
                        modified,
                    });
                }
            }
            // 名前なしのセッションは、最初の発言を見出しの代わりにする。
            Some("user") if fallback.is_none() => {
                fallback = user_text(&record).map(|text| tidy(&text)).filter(|t| !t.is_empty());
            }
            _ => {}
        }
    }

    fallback.map(|label| Session {
        id,
        label,
        named: false,
        modified,
    })
}

/// ユーザー発言の本文。content は文字列のことも、ブロックの配列のこともある。
fn user_text(record: &serde_json::Value) -> Option<String> {
    let content = record.get("message")?.get("content")?;
    if let Some(text) = content.as_str() {
        return Some(text.to_owned());
    }
    content
        .as_array()?
        .iter()
        .find_map(|block| block.get("text").and_then(|t| t.as_str()))
        .map(str::to_owned)
}

/// 1行の見出しに均す（改行・連続する空白を潰して、長すぎるものは切る）。
fn tidy(text: &str) -> String {
    let mut out = String::new();
    let mut spaced = false;
    for ch in text.chars() {
        if ch.is_whitespace() {
            // 先頭の空白は捨て、途中の連続空白は1つに。
            if !out.is_empty() {
                spaced = true;
            }
            continue;
        }
        if spaced {
            out.push(' ');
            spaced = false;
        }
        out.push(ch);
        if out.chars().count() >= LABEL_CHARS {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn temp_project_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "gototerm-sessions-{}-{}",
            std::process::id(),
            name
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn encodes_the_working_directory_the_way_claude_does() {
        assert_eq!(encode_dir(Path::new("/home/user/work")), "-home-user-work");
        // ドットも区切りと同じく '-' になる（実物がそうなっている）。
        assert_eq!(
            encode_dir(Path::new("/home/user/.claude")),
            "-home-user--claude"
        );
        assert_eq!(
            encode_dir(Path::new("/home/user/my_repo.git")),
            "-home-user-my-repo-git"
        );
    }

    #[test]
    fn named_sessions_use_the_name_and_others_fall_back_to_the_first_message() {
        let dir = temp_project_dir("labels");
        std::fs::write(
            dir.join("11111111-1111-1111-1111-111111111111.jsonl"),
            "{\"type\":\"custom-title\",\"customTitle\":\"府中コンパス改修\"}\n\
             {\"type\":\"user\",\"message\":{\"content\":\"あとの発言\"}}\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("22222222-2222-2222-2222-222222222222.jsonl"),
            "{\"type\":\"user\",\"message\":{\"content\":[{\"text\":\"gototermの\\nランチャーを直す\"}]}}\n",
        )
        .unwrap();

        let mut sessions = sessions_in(&dir, 10);
        sessions.sort_by(|a, b| a.id.cmp(&b.id));

        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].label, "府中コンパス改修");
        assert!(sessions[0].named);
        // 改行は潰して1行にする。
        assert_eq!(sessions[1].label, "gototermの ランチャーを直す");
        assert!(!sessions[1].named);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn newest_first_and_capped_at_the_limit() {
        let dir = temp_project_dir("order");
        for (i, name) in ["a", "b", "c"].iter().enumerate() {
            let path = dir.join(format!("{name}.jsonl"));
            std::fs::write(&path, "{\"type\":\"user\",\"message\":{\"content\":\"x\"}}\n").unwrap();
            // 更新時刻を i 秒ずつ古くする（a が最新）。
            let when = SystemTime::now() - Duration::from_secs(i as u64 * 60);
            filetime_set(&path, when);
        }

        let sessions = sessions_in(&dir, 2);
        assert_eq!(sessions.len(), 2, "limit で打ち切る");
        assert_eq!(sessions[0].id, "a", "新しい順");
        assert_eq!(sessions[1].id, "b");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 見出しが取れないファイル（発言のないセッション）は一覧に出さない。
    #[test]
    fn sessions_without_any_message_are_skipped() {
        let dir = temp_project_dir("empty");
        std::fs::write(dir.join("dead.jsonl"), "{\"type\":\"mode\"}\n").unwrap();
        assert!(sessions_in(&dir, 10).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_history_folder_is_not_an_error() {
        assert!(sessions_in(Path::new("/no/such/place"), 10).is_empty());
    }

    /// 更新時刻を差し替える（テスト専用。filetime クレートは入れない）。
    fn filetime_set(path: &Path, when: SystemTime) {
        let file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        file.set_modified(when).unwrap();
    }
}
