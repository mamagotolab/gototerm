//! gototask（Markdown のタスクファイル）との連携。
//!
//! - タブバーに出す「残N 優先M」の集計（ファイルの更新時刻が変わったときだけ読み直す）
//! - open_tasks キーで開く URL の組み立て
//! - 外部から「このファイルをエディタで開いて」を受けるソケット（Linux のみ）
//!
//! タスクファイルの書式は `- [ ] タスク`（未完了）／`- [x] タスク`（完了）、
//! 先頭が `! ` なら優先。字下げされた行（メモ等）は数えない。
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

/// 未完了の件数と、そのうち優先の件数。
pub(crate) fn summarize(text: &str) -> (usize, usize) {
    let mut open = 0;
    let mut important = 0;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("- [ ] ") {
            open += 1;
            if rest.starts_with("! ") {
                important += 1;
            }
        }
    }
    (open, important)
}

/// タスクファイルを見張って集計を持つ。読み直しは数秒に1回、更新時刻が変わったときだけ。
pub(crate) struct TaskSummary {
    path: PathBuf,
    mtime: Option<SystemTime>,
    checked: Option<Instant>,
    counts: Option<(usize, usize)>,
}

impl TaskSummary {
    pub(crate) fn from_config(task_file: &str) -> Option<Self> {
        if task_file.trim().is_empty() {
            return None;
        }
        Some(TaskSummary {
            path: expand_home(task_file.trim()),
            mtime: None,
            checked: None,
            counts: None,
        })
    }

    /// 必要なら読み直す。集計が変わったら true。
    pub(crate) fn refresh(&mut self) -> bool {
        if self
            .checked
            .is_some_and(|t| t.elapsed() < Duration::from_secs(2))
        {
            return false;
        }
        self.checked = Some(Instant::now());
        let mtime = std::fs::metadata(&self.path).and_then(|m| m.modified()).ok();
        if mtime.is_some() && mtime == self.mtime {
            return false;
        }
        self.mtime = mtime;
        let counts = std::fs::read_to_string(&self.path)
            .ok()
            .map(|text| summarize(&text));
        let changed = counts != self.counts;
        self.counts = counts;
        changed
    }

    pub(crate) fn counts(&self) -> Option<(usize, usize)> {
        self.counts
    }
}

fn expand_home(path: &str) -> PathBuf {
    match (path.strip_prefix("~/"), dirs_home()) {
        (Some(rest), Some(home)) => home.join(rest),
        _ => PathBuf::from(path),
    }
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

/// `base?v=dir:<フォルダ>` を作る。フォルダが無ければ全体。
pub(crate) fn tasks_url(base: &str, dir: Option<&Path>) -> String {
    match dir {
        Some(dir) => {
            let sep = if base.contains('?') { '&' } else { '?' };
            format!(
                "{base}{sep}v={}",
                percent_encode(&format!("dir:{}", dir.to_string_lossy()))
            )
        }
        None => base.to_owned(),
    }
}

fn percent_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// ソケットに届く1行 `open\t<絶対パス>\t<行>` を解釈する。行は 0 = 指定なし。
#[cfg(unix)]
pub(crate) fn parse_open_request(line: &str) -> Option<(PathBuf, u32)> {
    let mut parts = line.trim_end_matches(['\r', '\n']).split('\t');
    if parts.next()? != "open" {
        return None;
    }
    let path = PathBuf::from(parts.next()?);
    if !path.is_absolute() {
        return None;
    }
    let line = parts.next().and_then(|n| n.parse().ok()).unwrap_or(0);
    Some((path, line))
}

/// エディタに渡す引数。vi 系・nano・emacs は `+行` で行へ飛べる。
pub(crate) fn editor_args(editor: &[String], path: &Path, line: u32) -> Vec<String> {
    let mut command = editor.to_vec();
    let name = Path::new(&command[0])
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    if line > 0 && ["nvim", "vim", "vi", "nano", "emacs", "hx", "kak"].contains(&name.as_str()) {
        command.push(format!("+{line}"));
    }
    command.push(path.to_string_lossy().into_owned());
    command
}

#[cfg(unix)]
pub(crate) use unix_socket::OpenSocket;

#[cfg(unix)]
mod unix_socket {
    use std::io::Read;
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::PathBuf;
    use std::time::Duration;

    /// `$XDG_RUNTIME_DIR/gototerm.sock`。最初に起動したウィンドウだけが持つ。
    pub(crate) struct OpenSocket {
        listener: UnixListener,
        path: PathBuf,
    }

    impl OpenSocket {
        pub(crate) fn bind() -> Option<Self> {
            let dir = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from)?;
            let path = dir.join("gototerm.sock");
            if path.exists() {
                // 生きている別ウィンドウが持っていれば譲る。応答が無ければ残骸なので消す。
                if UnixStream::connect(&path).is_ok() {
                    return None;
                }
                let _ = std::fs::remove_file(&path);
            }
            let listener = UnixListener::bind(&path).ok()?;
            listener.set_nonblocking(true).ok()?;
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
            Some(OpenSocket { listener, path })
        }

        /// 届いている依頼をすべて取り出す（ブロックしない）。
        pub(crate) fn poll(&self) -> Vec<(PathBuf, u32)> {
            let mut requests = Vec::new();
            while let Ok((stream, _)) = self.listener.accept() {
                let _ = stream.set_nonblocking(false);
                let _ = stream.set_read_timeout(Some(Duration::from_millis(200)));
                let mut buf = Vec::new();
                let _ = stream.take(4096).read_to_end(&mut buf);
                let text = String::from_utf8_lossy(&buf);
                requests.extend(text.lines().filter_map(super::parse_open_request));
            }
            requests
        }
    }

    impl Drop for OpenSocket {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summarize_counts_top_level_open_tasks() {
        let text = "# P\n## C\n- [ ] ! a\n- [ ] b\n  メモ: - [ ] 字下げは数えない\n- [x] c\n- [x] ! d\n";
        assert_eq!(summarize(text), (2, 1));
    }

    #[test]
    fn tasks_url_encodes_dir() {
        assert_eq!(
            tasks_url("http://localhost:8765/", Some(Path::new("/home/a/作業 x"))),
            "http://localhost:8765/?v=dir%3A%2Fhome%2Fa%2F%E4%BD%9C%E6%A5%AD%20x"
        );
        assert_eq!(tasks_url("http://h/", None), "http://h/");
    }

    #[test]
    #[cfg(unix)]
    fn parse_open_request_accepts_only_absolute_open() {
        assert_eq!(
            parse_open_request("open\t/a/b.md\t12\n"),
            Some((PathBuf::from("/a/b.md"), 12))
        );
        assert_eq!(parse_open_request("open\t/a/b.md"), Some((PathBuf::from("/a/b.md"), 0)));
        assert_eq!(parse_open_request("open\trel.md\t1"), None);
        assert_eq!(parse_open_request("exec\t/bin/sh"), None);
    }

    #[test]
    fn editor_args_adds_line_only_for_known_editors() {
        let p = Path::new("/a.md");
        assert_eq!(editor_args(&["nvim".into()], p, 5), ["nvim", "+5", "/a.md"]);
        assert_eq!(editor_args(&["code".into(), "-w".into()], p, 5), ["code", "-w", "/a.md"]);
        assert_eq!(editor_args(&["nvim".into()], p, 0), ["nvim", "/a.md"]);
    }
}
