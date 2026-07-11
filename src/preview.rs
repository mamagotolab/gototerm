use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

const TAIL_BYTES: u64 = 64 * 1024;
const BINARY_SCAN_BYTES: usize = 8 * 1024;
const READ_DEBOUNCE: Duration = Duration::from_millis(100);
/// これより大きい画像ファイルはデコードしない（メモリ・時間の暴発を防ぐ）。
const IMAGE_MAX_BYTES: u64 = 32 * 1024 * 1024;
const DIFF_MAX_BYTES: usize = 512 * 1024;

pub enum PreviewContent {
    Text(Vec<String>),
    Diff(Vec<String>),
    /// プレビュー領域に収まるよう縮小済みの RGB 画像（3バイト/画素・行0＝上）。
    Image {
        rgb: Vec<u8>,
        w: u32,
        h: u32,
    },
    Binary,
    Deleted,
    Empty,
}

/// 拡張子から画像ファイルか判定する。
pub(crate) fn is_image_path(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .as_deref(),
        Some("png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp")
    )
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
    /// 画像を収める表示領域（px）。ペインのリサイズで更新される。
    fit: (u32, u32),
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
            fit: (640, 480),
        }
    }

    /// 画像プレビューの収まる領域（px）を設定する。変わったら true。
    pub fn set_fit(&mut self, w: u32, h: u32) -> bool {
        let next = (w.max(1), h.max(1));
        if self.fit != next {
            self.fit = next;
            true
        } else {
            false
        }
    }

    /// 現在プレビュー中が画像なら (RGB, 幅, 高さ) を返す。
    pub fn image(&self) -> Option<(&[u8], u32, u32)> {
        match &self.content {
            PreviewContent::Image { rgb, w, h } => Some((rgb, *w, *h)),
            _ => None,
        }
    }

    pub fn is_diff(&self) -> bool {
        matches!(self.content, PreviewContent::Diff(_))
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
            PreviewContent::Diff(lines) => PreviewLines::Diff(lines.as_slice()),
            PreviewContent::Image { .. } => PreviewLines::Message("(画像)"),
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
        let fit = self.fit;
        self.pending = Some(rx);
        self.last_read = Instant::now();

        std::thread::spawn(move || {
            let content = if is_image_path(&abs_path) {
                read_image(&abs_path, fit.0, fit.1)
            } else if let Some(diff) = read_diff(&abs_path) {
                PreviewContent::Diff(diff)
            } else {
                read_tail(&abs_path)
            };
            let _ = tx.send(PreviewRead {
                target: thread_target,
                content,
            });
        });
    }
}

/// 画像を表示領域 (fit_w × fit_h) に収まるよう縮小して RGB で返す（拡大はしない）。
fn read_image(path: &Path, fit_w: u32, fit_h: u32) -> PreviewContent {
    let Ok(meta) = std::fs::metadata(path) else {
        return PreviewContent::Deleted;
    };
    if meta.len() > IMAGE_MAX_BYTES {
        return PreviewContent::Text(vec!["(画像が大きすぎるため表示しません)".to_owned()]);
    }

    // 拡張子ではなく中身（マジックバイト）で形式を判定する。拡張子と実体が
    // 食い違うファイル（.jpg なのに実は png 等）でも開けるようにするため。
    let reader = match image::ImageReader::open(path).and_then(|r| r.with_guessed_format()) {
        Ok(reader) => reader,
        Err(_) => return PreviewContent::Deleted,
    };
    let img = match reader.decode() {
        Ok(img) => img,
        Err(e) => {
            // 未対応形式・壊れたデータは理由を出す（どの形式がダメか分かるように）。
            return PreviewContent::Text(vec![
                "(この画像は表示できませんでした)".to_owned(),
                format!("  {e}"),
            ]);
        }
    };

    // thumbnail は縦横比を保って fit 内に収める（元より大きくはしない）。
    let rgb = img.thumbnail(fit_w.max(1), fit_h.max(1)).to_rgb8();
    let (w, h) = rgb.dimensions();
    PreviewContent::Image {
        rgb: rgb.into_raw(),
        w,
        h,
    }
}

pub enum PreviewLines<'a> {
    Text(&'a [String]),
    Diff(&'a [String]),
    Message(&'static str),
}

fn read_diff(abs_path: &Path) -> Option<Vec<String>> {
    let parent = abs_path.parent()?;
    let output = Command::new("git")
        .arg("-C")
        .arg(parent)
        .arg("diff")
        .arg("HEAD")
        .arg("--no-color")
        .arg("--")
        .arg(abs_path)
        .output()
        .ok()?;

    if !output.status.success() || output.stdout.is_empty() {
        return None;
    }

    // diff は先頭（ファイル見出し・最初のハンク）から見せたい。tail_lines は
    // 末尾 max 行を返すので、全行を得てから先頭 5000 行に切る。
    let limit = output.stdout.len().min(DIFF_MAX_BYTES);
    let mut lines = tail_lines(&output.stdout[..limit], usize::MAX);
    lines.truncate(5000);
    Some(lines)
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

    #[test]
    fn preview_lines_exposes_diff_content() {
        let mut preview = FilePreview::new();
        preview.content = PreviewContent::Diff(vec!["+added".to_owned()]);

        match preview.lines() {
            PreviewLines::Diff(lines) => assert_eq!(lines, &["+added".to_owned()]),
            _ => panic!("expected diff preview lines"),
        }
        assert!(preview.is_diff());
    }

    #[test]
    fn detects_image_extensions_case_insensitively() {
        assert!(is_image_path(Path::new("a.png")));
        assert!(is_image_path(Path::new("b.JPG")));
        assert!(is_image_path(Path::new("dir/c.jpeg")));
        assert!(is_image_path(Path::new("d.WebP")));
        assert!(!is_image_path(Path::new("e.txt")));
        assert!(!is_image_path(Path::new("f.rs")));
        assert!(!is_image_path(Path::new("noext")));
    }
}
