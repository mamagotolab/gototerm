//! gototask（プロジェクトごとの TASKS.md）との連携。
//!
//! - タブバーに出す「残N 優先M」の集計。今いるフォルダから上へたどって最初に見つかった
//!   `TASKS.md`、無ければ設定の task_file（ファイルの更新時刻が変わったときだけ読み直す）
//! - サイドバー [tasks] 用の一覧（未完了のタスクと区分）と「済にする」
//! - open_tasks キーで開く URL の組み立て
//! - 外部から「このファイルをエディタで開いて」を受けるソケット（Linux のみ）
//!
//! 書式は Obsidian Tasks と同じ：`- [ ] 内容 @担当 ⏫ ⏳ 2026-10-11 📅 2026-10-20`、
//! 済は `- [x] … ✅ 日付`、やらないは `- [-] … ❌ 日付`。旧書式の先頭 `! `（優先）も読む。
//! 字下げされた行（メモ等）は数えない。
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

pub(crate) const TASKS_FILE: &str = "TASKS.md";
const PRIORITY: [&str; 2] = ["⏫", "🔺"];
const DATE_FIELDS: [&str; 6] = ["➕", "🛫", "⏳", "📅", "❌", "✅"];

fn open_body(line: &str) -> Option<&str> {
    line.strip_prefix("- [ ] ")
        .or_else(|| line.strip_prefix("- [/] "))
}

fn is_important(body: &str) -> bool {
    body.starts_with("! ") || PRIORITY.iter().any(|p| body.contains(p))
}

/// 未完了の件数と、そのうち優先の件数。
pub(crate) fn summarize(text: &str) -> (usize, usize) {
    let mut open = 0;
    let mut important = 0;
    for body in text.lines().filter_map(open_body) {
        open += 1;
        if is_important(body) {
            important += 1;
        }
    }
    (open, important)
}

/// 未完了のタスク1件（サイドバー表示用）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TaskItem {
    /// ファイル内の行番号（0 始まり）。
    pub(crate) line: usize,
    /// 元の行。書き換えるときに、裏で変わっていないかの確認に使う。
    pub(crate) raw: String,
    pub(crate) title: String,
    pub(crate) section: String,
    pub(crate) important: bool,
    pub(crate) who: Vec<String>,
    pub(crate) due: Option<String>,
    pub(crate) scheduled: Option<String>,
}

/// 行末の属性（⏫ や `📅 日付`）を外して (本文, 優先, 期限, 今日やる日) にする。
fn split_fields(body: &str) -> (String, bool, Option<String>, Option<String>) {
    let mut words: Vec<&str> = body.split_whitespace().collect();
    let (mut important, mut due, mut scheduled) = (false, None, None);
    loop {
        let n = words.len();
        if n >= 1 && PRIORITY.contains(&words[n - 1])
            || n >= 1 && ["🔼", "🔽", "⏬"].contains(&words[n - 1])
        {
            important |= PRIORITY.contains(&words[n - 1]);
            words.pop();
        } else if n >= 2
            && DATE_FIELDS.contains(&words[n - 2].trim_end_matches('\u{FE0F}'))
            && is_date(words[n - 1])
        {
            match words[n - 2].trim_end_matches('\u{FE0F}') {
                "📅" => due = due.or(Some(words[n - 1].to_owned())),
                "⏳" => scheduled = scheduled.or(Some(words[n - 1].to_owned())),
                _ => {}
            }
            words.truncate(n - 2);
        } else {
            break;
        }
    }
    let mut text = words.join(" ");
    if let Some(rest) = text.strip_prefix("! ") {
        important = true;
        text = rest.to_owned();
    }
    (text, important, due, scheduled)
}

fn is_date(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b.iter()
            .enumerate()
            .all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit())
}

/// 未完了のタスクと、済・やらないの件数。
pub(crate) fn parse_tasks(text: &str) -> (Vec<TaskItem>, usize) {
    let mut items = Vec::new();
    let mut closed = 0;
    let mut section = String::new();
    for (line, raw) in text.lines().enumerate() {
        if raw.starts_with("# ") {
            section.clear();
        } else if let Some(name) = raw.strip_prefix("## ") {
            section = name.trim().to_owned();
        } else if let Some(body) = open_body(raw) {
            let (desc, important, due, scheduled) = split_fields(body);
            let who = desc
                .split_whitespace()
                .filter_map(|w| w.strip_prefix('@'))
                .filter(|w| !w.is_empty())
                .map(str::to_owned)
                .collect();
            let title = desc
                .split_whitespace()
                .filter(|w| !w.starts_with('@'))
                .collect::<Vec<_>>()
                .join(" ");
            items.push(TaskItem {
                line,
                raw: raw.to_owned(),
                title: if title.is_empty() { desc } else { title },
                section: section.clone(),
                important,
                who,
                due,
                scheduled,
            });
        } else if raw.starts_with("- [x] ")
            || raw.starts_with("- [X] ")
            || raw.starts_with("- [-] ")
        {
            closed += 1;
        }
    }
    (items, closed)
}

/// `line` 行目（0 始まり）が `raw` のままなら済にして ✅ 日付を付ける。書き換えたら true。
pub(crate) fn mark_done(path: &Path, line: usize, raw: &str, today: &str) -> std::io::Result<bool> {
    let text = std::fs::read_to_string(path)?;
    let mut lines: Vec<&str> = text.split('\n').collect();
    // CRLF のファイルでも比べられるように末尾の \r は外して比べる。
    let Some(current) = lines.get(line) else {
        return Ok(false);
    };
    let cr = current.ends_with('\r');
    if current.trim_end_matches('\r') != raw || open_body(raw).is_none() {
        return Ok(false);
    }
    let done = format!(
        "- [x] {} ✅ {today}{}",
        &raw[6..],
        if cr { "\r" } else { "" }
    );
    lines[line] = &done;
    std::fs::write(path, lines.join("\n"))?;
    Ok(true)
}

/// 今日の日付（YYYY-MM-DD・ローカル時刻）。依存を増やさないため、Unix は `date` を使い、
/// 取れなければ UTC の日付にする。
pub(crate) fn today() -> String {
    // サイドバーは描画のたびに呼ぶので、プロセス起動は1分に1回まで。
    static CACHE: std::sync::Mutex<Option<(Instant, String)>> = std::sync::Mutex::new(None);
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((at, day)) = cache.as_ref() {
        if at.elapsed() < Duration::from_secs(60) {
            return day.clone();
        }
    }
    let day = today_uncached();
    *cache = Some((Instant::now(), day.clone()));
    day
}

fn today_uncached() -> String {
    #[cfg(unix)]
    if let Ok(out) = std::process::Command::new("date").arg("+%F").output() {
        let s = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        if is_date(&s) {
            return s;
        }
    }
    let days = SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() / 86_400)
        .unwrap_or(0) as i64;
    civil_from_days(days)
}

/// 1970-01-01 からの日数 → YYYY-MM-DD（Howard Hinnant の civil_from_days）。
fn civil_from_days(z: i64) -> String {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

/// `dir` から上へたどって最初に見つかった TASKS.md。ホームとルートは見ない
/// （ホーム直下のファイルはプロジェクトではないため）。
pub(crate) fn find_tasks_file(dir: &Path) -> Option<PathBuf> {
    let home = dirs_home();
    for d in dir.ancestors() {
        if Some(d) == home.as_deref() || d.parent().is_none() {
            return None;
        }
        let f = d.join(TASKS_FILE);
        if f.is_file() {
            return Some(f);
        }
    }
    None
}

/// タスクファイルを見張って集計を持つ。読み直しは数秒に1回、対象か更新時刻が変わったときだけ。
pub(crate) struct TaskSummary {
    /// 今いるフォルダに TASKS.md が無いときに使うファイル（設定の task_file）。
    fallback: Option<PathBuf>,
    path: Option<PathBuf>,
    mtime: Option<SystemTime>,
    checked: Option<Instant>,
    counts: Option<(usize, usize)>,
}

impl TaskSummary {
    pub(crate) fn from_config(task_file: &str) -> Self {
        let task_file = task_file.trim();
        TaskSummary {
            fallback: (!task_file.is_empty()).then(|| expand_home(task_file)),
            path: None,
            mtime: None,
            checked: None,
            counts: None,
        }
    }

    /// 必要なら読み直す。集計が変わったら true。`cwd` は今いるフォルダ。
    pub(crate) fn refresh(&mut self, cwd: Option<&Path>) -> bool {
        if self
            .checked
            .is_some_and(|t| t.elapsed() < Duration::from_secs(2))
        {
            return false;
        }
        self.checked = Some(Instant::now());
        let target = cwd
            .and_then(find_tasks_file)
            .or_else(|| self.fallback.clone());
        if target != self.path {
            self.path = target;
            self.mtime = None;
        }
        let mtime = self
            .path
            .as_ref()
            .and_then(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok());
        if mtime.is_some() && mtime == self.mtime {
            return false;
        }
        self.mtime = mtime;
        let counts = self
            .path
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
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
        let text = "# P\n## C\n- [ ] ! a\n- [ ] b\n  メモ: - [ ] 字下げは数えない\n- [x] c\n- [x] ! d\n- [ ] e ⏫ 📅 2026-10-20\n- [/] f\n";
        assert_eq!(summarize(text), (4, 2));
    }

    #[test]
    fn parse_tasks_strips_obsidian_fields() {
        let text = "# P\n- [ ] 先頭\n## 営業\n- [ ] 見積もりを送る @丸山さん ⏫ ⏳ 2026-10-11 📅 2026-10-20\n  背景: x\n- [x] 済 ✅ 2026-10-11\n- [-] やめ ❌ 2026-10-11\n";
        let (items, closed) = parse_tasks(text);
        assert_eq!(closed, 2);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].section, "");
        let t = &items[1];
        assert_eq!(
            (t.line, t.title.as_str(), t.section.as_str()),
            (3, "見積もりを送る", "営業")
        );
        assert!(t.important);
        assert_eq!(t.who, ["丸山さん"]);
        assert_eq!(t.due.as_deref(), Some("2026-10-20"));
        assert_eq!(t.scheduled.as_deref(), Some("2026-10-11"));
    }

    #[test]
    fn mark_done_checks_raw_and_appends_date() {
        let dir = std::env::temp_dir().join(format!("gototerm-task-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("TASKS.md");
        std::fs::write(&f, "# P\n- [ ] a ⏫\n- [ ] b\n").unwrap();
        assert!(!mark_done(&f, 1, "- [ ] 違う", "2026-10-11").unwrap());
        assert!(mark_done(&f, 1, "- [ ] a ⏫", "2026-10-11").unwrap());
        assert_eq!(
            std::fs::read_to_string(&f).unwrap(),
            "# P\n- [x] a ⏫ ✅ 2026-10-11\n- [ ] b\n"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn civil_from_days_matches_known_dates() {
        assert_eq!(civil_from_days(0), "1970-01-01");
        assert_eq!(civil_from_days(20_372), "2025-10-11");
        assert_eq!(civil_from_days(11_016), "2000-02-29");
    }

    #[test]
    fn find_tasks_file_walks_up_but_not_to_home() {
        let base = std::env::temp_dir().join(format!("gototerm-find-{}", std::process::id()));
        let sub = base.join("a/b");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(base.join("a/TASKS.md"), "").unwrap();
        assert_eq!(find_tasks_file(&sub), Some(base.join("a/TASKS.md")));
        std::fs::remove_dir_all(&base).unwrap();
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
        assert_eq!(
            parse_open_request("open\t/a/b.md"),
            Some((PathBuf::from("/a/b.md"), 0))
        );
        assert_eq!(parse_open_request("open\trel.md\t1"), None);
        assert_eq!(parse_open_request("exec\t/bin/sh"), None);
    }

    #[test]
    fn editor_args_adds_line_only_for_known_editors() {
        let p = Path::new("/a.md");
        assert_eq!(editor_args(&["nvim".into()], p, 5), ["nvim", "+5", "/a.md"]);
        assert_eq!(
            editor_args(&["code".into(), "-w".into()], p, 5),
            ["code", "-w", "/a.md"]
        );
        assert_eq!(editor_args(&["nvim".into()], p, 0), ["nvim", "/a.md"]);
    }
}
