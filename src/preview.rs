use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

const TAIL_BYTES: u64 = 64 * 1024;
const BINARY_SCAN_BYTES: usize = 8 * 1024;
const READ_DEBOUNCE: Duration = Duration::from_millis(100);

pub enum PreviewContent {
    Text(Vec<String>),
    Binary,
    Deleted,
    Empty,
}

struct PreviewRead {
    target: PathBuf,
    content: PreviewContent,
}

/// プレビュー本文の状態。読込はバックグラウンドで行い、結果を try_recv で受ける。
pub struct FilePreview {
    target: Option<PathBuf>,
    target_abs: Option<PathBuf>,
    content: PreviewContent,
    pending: Option<Receiver<PreviewRead>>,
    last_read: Instant,
    queued: Option<(PathBuf, PathBuf)>,
}

impl FilePreview {
    pub fn new() -> Self {
        Self {
            target: None,
            target_abs: None,
            content: PreviewContent::Empty,
            pending: None,
            last_read: Instant::now() - READ_DEBOUNCE,
            queued: None,
        }
    }

    pub fn target(&self) -> Option<&Path> {
        self.target.as_deref()
    }

    pub fn target_abs(&self) -> Option<&Path> {
        self.target_abs.as_deref()
    }

    pub fn set_target_abs(&mut self, abs_path: PathBuf, display_path: PathBuf) {
        self.target = Some(display_path.clone());
        self.target_abs = Some(abs_path.clone());
        self.content = PreviewContent::Empty;
        self.request_read(abs_path, display_path, true);
    }

    pub fn notify_target_abs(&mut self, abs_path: PathBuf, display_path: PathBuf) {
        self.target = Some(display_path.clone());
        self.target_abs = Some(abs_path.clone());
        self.request_read(abs_path, display_path, false);
    }

    pub fn set_memory_content(&mut self, display_path: PathBuf, bytes: Vec<u8>) {
        self.target = Some(display_path);
        self.target_abs = None;
        self.pending = None;
        self.queued = None;
        self.content = if looks_binary(&bytes) {
            PreviewContent::Binary
        } else {
            PreviewContent::Text(tail_lines(&bytes, usize::MAX))
        };
    }

    pub fn refresh_current(&mut self) {
        if let (Some(abs_path), Some(display_path)) = (&self.target_abs, &self.target) {
            self.request_read(abs_path.clone(), display_path.clone(), true);
        }
    }

    pub fn poll(&mut self) -> bool {
        let mut changed = false;

        if let Some(rx) = &self.pending {
            match rx.try_recv() {
                Ok(result) => {
                    if self.target.as_deref() == Some(result.target.as_path()) {
                        self.content = result.content;
                        changed = true;
                    }
                    self.pending = None;
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.pending = None;
                    changed = true;
                }
            }
        }

        if self.pending.is_none() && self.last_read.elapsed() >= READ_DEBOUNCE {
            if let Some((abs_path, display_path)) = self.queued.take() {
                self.spawn_read(abs_path, display_path);
            }
        }

        changed
    }

    pub fn lines(&self) -> PreviewLines<'_> {
        match &self.content {
            PreviewContent::Text(lines) => PreviewLines::Text(lines.as_slice()),
            PreviewContent::Binary => PreviewLines::Message("(バイナリファイル)"),
            PreviewContent::Deleted => PreviewLines::Message("(削除されました)"),
            PreviewContent::Empty => {
                PreviewLines::Message("(AIやコマンドがファイルを変更すると、ここに中身が流れます)")
            }
        }
    }

    fn request_read(&mut self, abs_path: PathBuf, display_path: PathBuf, immediate: bool) {
        if immediate || self.last_read.elapsed() >= READ_DEBOUNCE {
            self.spawn_read(abs_path, display_path);
        } else {
            self.queued = Some((abs_path, display_path));
        }
    }

    fn spawn_read(&mut self, abs_path: PathBuf, display_path: PathBuf) {
        let (tx, rx) = mpsc::channel();
        let thread_target = display_path.clone();
        self.pending = Some(rx);
        self.last_read = Instant::now();

        std::thread::spawn(move || {
            let content = read_tail(&abs_path);
            let _ = tx.send(PreviewRead {
                target: thread_target,
                content,
            });
        });
    }
}

pub enum PreviewLines<'a> {
    Text(&'a [String]),
    Message(&'static str),
}

fn read_tail(path: &Path) -> PreviewContent {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(_) => return PreviewContent::Deleted,
    };

    let len = file.metadata().map(|metadata| metadata.len()).unwrap_or(0);
    if len > TAIL_BYTES && file.seek(SeekFrom::End(-(TAIL_BYTES as i64))).is_err() {
        return PreviewContent::Deleted;
    }

    let mut bytes = Vec::new();
    if file.read_to_end(&mut bytes).is_err() {
        return PreviewContent::Deleted;
    }

    if looks_binary(&bytes) {
        PreviewContent::Binary
    } else {
        PreviewContent::Text(tail_lines(&bytes, usize::MAX))
    }
}

pub(crate) fn tail_lines(bytes: &[u8], max: usize) -> Vec<String> {
    if bytes.is_empty() || max == 0 {
        return Vec::new();
    }

    let text = String::from_utf8_lossy(bytes).replace('\t', "    ");
    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
    if text.ends_with('\n') {
        lines.push(String::new());
    }

    if lines.len() > max {
        lines.split_off(lines.len() - max)
    } else {
        lines
    }
}

pub(crate) fn looks_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(BINARY_SCAN_BYTES).any(|byte| *byte == 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tail_lines_reads_valid_utf8() {
        assert_eq!(
            tail_lines("one\ntwo\nthree".as_bytes(), 2),
            vec!["two".to_owned(), "three".to_owned()]
        );
    }

    #[test]
    fn tail_lines_uses_lossy_utf8() {
        assert_eq!(tail_lines(b"a\xffb", 10), vec!["a\u{fffd}b".to_owned()]);
    }

    #[test]
    fn detects_nul_in_binary_scan_window() {
        assert!(looks_binary(b"abc\0def"));
    }

    #[test]
    fn empty_input_has_no_lines() {
        assert!(tail_lines(b"", 10).is_empty());
    }

    #[test]
    fn preserves_trailing_newline_as_empty_last_line() {
        assert_eq!(
            tail_lines(b"one\ntwo\n", 10),
            vec!["one".to_owned(), "two".to_owned(), String::new()]
        );
    }

    #[test]
    fn handles_without_trailing_newline() {
        assert_eq!(
            tail_lines(b"one\ntwo", 10),
            vec!["one".to_owned(), "two".to_owned()]
        );
    }

    #[test]
    fn expands_tabs_to_four_spaces() {
        assert_eq!(tail_lines(b"a\tb", 10), vec!["a    b".to_owned()]);
    }
}
