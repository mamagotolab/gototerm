//! キー入力から PTY へ送るバイト列を組み立てる。
//!
//! 修飾キー（Shift/Alt/Ctrl）を落とすと TUI のキー割り当てが効かなくなる。
//! 実際 gototerm では Ctrl+矢印が完全に無反応、Home/End は分岐すら無く無反応、
//! Ctrl+Backspace や Alt+PageUp なども送られていなかった。ここに xterm 準拠の
//! 組み立てを集約して、同じ穴が空かないようにする。

/// 押されている修飾キー。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(crate) struct Mods {
    pub shift: bool,
    pub alt: bool,
    pub ctrl: bool,
}

impl Mods {
    pub(crate) fn new(shift: bool, alt: bool, ctrl: bool) -> Self {
        Self { shift, alt, ctrl }
    }

    /// xterm の修飾コード。1 + Shift(1) + Alt(2) + Ctrl(4)。
    fn code(self) -> u8 {
        1 + u8::from(self.shift) + 2 * u8::from(self.alt) + 4 * u8::from(self.ctrl)
    }

    fn is_empty(self) -> bool {
        self.code() == 1
    }
}

/// カーソル系のキー。修飾なしのときだけ application cursor mode の SS3 形式を使う。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CursorKey {
    Up,
    Down,
    Right,
    Left,
    Home,
    End,
}

impl CursorKey {
    fn final_byte(self) -> char {
        match self {
            CursorKey::Up => 'A',
            CursorKey::Down => 'B',
            CursorKey::Right => 'C',
            CursorKey::Left => 'D',
            CursorKey::Home => 'H',
            CursorKey::End => 'F',
        }
    }
}

/// 修飾なしのカーソル系キー。
pub(crate) fn cursor_key_sequence(key: CursorKey, application: bool) -> &'static [u8] {
    match (application, key) {
        (false, CursorKey::Up) => b"\x1b[A",
        (false, CursorKey::Down) => b"\x1b[B",
        (false, CursorKey::Right) => b"\x1b[C",
        (false, CursorKey::Left) => b"\x1b[D",
        (false, CursorKey::Home) => b"\x1b[H",
        (false, CursorKey::End) => b"\x1b[F",
        (true, CursorKey::Up) => b"\x1bOA",
        (true, CursorKey::Down) => b"\x1bOB",
        (true, CursorKey::Right) => b"\x1bOC",
        (true, CursorKey::Left) => b"\x1bOD",
        (true, CursorKey::Home) => b"\x1bOH",
        (true, CursorKey::End) => b"\x1bOF",
    }
}

/// カーソル系キー（矢印・Home・End）。修飾ありは `CSI 1 ; <修飾> <終端>`。
pub(crate) fn cursor_key_bytes(key: CursorKey, application: bool, mods: Mods) -> Vec<u8> {
    if mods.is_empty() {
        return cursor_key_sequence(key, application).to_vec();
    }
    format!("\x1b[1;{}{}", mods.code(), key.final_byte()).into_bytes()
}

/// チルダ系のキー。`CSI <番号> ~`（修飾ありは `CSI <番号> ; <修飾> ~`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TildeKey {
    Insert,
    Delete,
    PageUp,
    PageDown,
}

impl TildeKey {
    fn number(self) -> u8 {
        match self {
            TildeKey::Insert => 2,
            TildeKey::Delete => 3,
            TildeKey::PageUp => 5,
            TildeKey::PageDown => 6,
        }
    }
}

pub(crate) fn tilde_key_bytes(key: TildeKey, mods: Mods) -> Vec<u8> {
    let n = key.number();
    if mods.is_empty() {
        format!("\x1b[{n}~").into_bytes()
    } else {
        format!("\x1b[{n};{}~", mods.code()).into_bytes()
    }
}

/// ファンクションキー。F1〜F4 は SS3 系、F5 以降はチルダ系（番号は歴史的に不連続）。
pub(crate) fn function_key_bytes(n: u8, mods: Mods) -> Option<Vec<u8>> {
    let bytes = match n {
        1..=4 => {
            let final_byte = match n {
                1 => 'P',
                2 => 'Q',
                3 => 'R',
                _ => 'S',
            };
            if mods.is_empty() {
                format!("\x1bO{final_byte}").into_bytes()
            } else {
                format!("\x1b[1;{}{final_byte}", mods.code()).into_bytes()
            }
        }
        5..=12 => {
            // VT220 由来の番号。15,16 と 22 は欠番。
            let code = match n {
                5 => 15,
                6 => 17,
                7 => 18,
                8 => 19,
                9 => 20,
                10 => 21,
                11 => 23,
                _ => 24,
            };
            if mods.is_empty() {
                format!("\x1b[{code}~").into_bytes()
            } else {
                format!("\x1b[{code};{}~", mods.code()).into_bytes()
            }
        }
        _ => return None,
    };
    Some(bytes)
}

/// Backspace。素は DEL、Ctrl は BS、Alt は ESC 前置（readline の単語削除）。
pub(crate) fn backspace_bytes(mods: Mods) -> Vec<u8> {
    let base: u8 = if mods.ctrl { 0x08 } else { 0x7f };
    if mods.alt {
        vec![0x1b, base]
    } else {
        vec![base]
    }
}

/// Tab。Shift+Tab は back-tab（CSI Z）。
pub(crate) fn tab_bytes(mods: Mods) -> Vec<u8> {
    if mods.shift {
        b"\x1b[Z".to_vec()
    } else if mods.alt {
        vec![0x1b, b'\t']
    } else {
        vec![b'\t']
    }
}

/// Space。
///
/// Ctrl+Space は**何も送らない**。xterm は NUL を送るが、多くの環境で
/// Ctrl+Space は IME の切り替えに割り当てられており、端末が NUL を送ると
/// アプリ側に余計な入力が届く。実際 v0.6.11 で NUL を送るようにしたところ、
/// mutt からの vim で「日本語入力に切り替えると同時に挿入モードを抜ける」
/// 不具合になった（vim の i_CTRL-@ は「直前の挿入テキストを入れて挿入を終える」）。
/// NUL を必要とするアプリより、IME 切り替えを壊さないことを優先する。
pub(crate) fn space_bytes(mods: Mods) -> Option<Vec<u8>> {
    if mods.ctrl {
        return None;
    }
    Some(if mods.alt {
        vec![0x1b, b' ']
    } else {
        vec![b' ']
    })
}

/// Enter。Shift+Enter は ESC+CR（Claude Code 等が「送信せず改行」に使う）。
pub(crate) fn enter_bytes(mods: Mods) -> Vec<u8> {
    if mods.shift || mods.alt {
        vec![0x1b, b'\r']
    } else {
        vec![b'\r']
    }
}

/// Escape。Alt+Esc は ESC を2つ。
pub(crate) fn escape_bytes(mods: Mods) -> Vec<u8> {
    if mods.alt {
        vec![0x1b, 0x1b]
    } else {
        vec![0x1b]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONE: Mods = Mods {
        shift: false,
        alt: false,
        ctrl: false,
    };
    fn m(shift: bool, alt: bool, ctrl: bool) -> Mods {
        Mods::new(shift, alt, ctrl)
    }

    #[test]
    fn modifier_code_follows_xterm() {
        assert_eq!(NONE.code(), 1);
        assert_eq!(m(true, false, false).code(), 2); // Shift
        assert_eq!(m(false, true, false).code(), 3); // Alt
        assert_eq!(m(true, true, false).code(), 4); // Shift+Alt
        assert_eq!(m(false, false, true).code(), 5); // Ctrl
        assert_eq!(m(true, true, true).code(), 8); // 全部
    }

    #[test]
    fn unmodified_cursor_keys_follow_application_mode() {
        assert_eq!(cursor_key_bytes(CursorKey::Up, false, NONE), b"\x1b[A");
        assert_eq!(cursor_key_bytes(CursorKey::Up, true, NONE), b"\x1bOA");
        assert_eq!(cursor_key_bytes(CursorKey::Home, false, NONE), b"\x1b[H");
        assert_eq!(cursor_key_bytes(CursorKey::End, false, NONE), b"\x1b[F");
        assert_eq!(cursor_key_bytes(CursorKey::Home, true, NONE), b"\x1bOH");
    }

    #[test]
    fn modified_cursor_keys_use_csi_with_modifier() {
        // readline の単語移動
        assert_eq!(
            cursor_key_bytes(CursorKey::Right, false, m(false, false, true)),
            b"\x1b[1;5C"
        );
        assert_eq!(
            cursor_key_bytes(CursorKey::Left, false, m(false, false, true)),
            b"\x1b[1;5D"
        );
        // 選択
        assert_eq!(
            cursor_key_bytes(CursorKey::Up, false, m(true, false, false)),
            b"\x1b[1;2A"
        );
        // Home/End も修飾できる
        assert_eq!(
            cursor_key_bytes(CursorKey::End, false, m(true, false, false)),
            b"\x1b[1;2F"
        );
        // 修飾ありは application mode でも CSI 形式（xterm 準拠）
        assert_eq!(
            cursor_key_bytes(CursorKey::Left, true, m(false, false, true)),
            b"\x1b[1;5D"
        );
    }

    #[test]
    fn tilde_keys_carry_their_numbers_and_modifiers() {
        assert_eq!(tilde_key_bytes(TildeKey::Insert, NONE), b"\x1b[2~");
        assert_eq!(tilde_key_bytes(TildeKey::Delete, NONE), b"\x1b[3~");
        assert_eq!(tilde_key_bytes(TildeKey::PageUp, NONE), b"\x1b[5~");
        assert_eq!(tilde_key_bytes(TildeKey::PageDown, NONE), b"\x1b[6~");
        // 以前は Ctrl+PageUp が完全に無反応だった
        assert_eq!(
            tilde_key_bytes(TildeKey::PageUp, m(false, false, true)),
            b"\x1b[5;5~"
        );
        assert_eq!(
            tilde_key_bytes(TildeKey::Delete, m(true, false, false)),
            b"\x1b[3;2~"
        );
    }

    #[test]
    fn function_keys_split_between_ss3_and_tilde_forms() {
        assert_eq!(function_key_bytes(1, NONE).unwrap(), b"\x1bOP");
        assert_eq!(function_key_bytes(4, NONE).unwrap(), b"\x1bOS");
        assert_eq!(function_key_bytes(5, NONE).unwrap(), b"\x1b[15~");
        assert_eq!(function_key_bytes(12, NONE).unwrap(), b"\x1b[24~");
        // 欠番を踏まないこと（F5=15、F6=17。16 は欠番）
        assert_eq!(function_key_bytes(6, NONE).unwrap(), b"\x1b[17~");
        // 修飾つき
        assert_eq!(
            function_key_bytes(1, m(true, false, false)).unwrap(),
            b"\x1b[1;2P"
        );
        assert_eq!(
            function_key_bytes(5, m(false, false, true)).unwrap(),
            b"\x1b[15;5~"
        );
        assert!(function_key_bytes(13, NONE).is_none());
        assert!(function_key_bytes(0, NONE).is_none());
    }

    #[test]
    fn backspace_distinguishes_ctrl_and_alt() {
        assert_eq!(backspace_bytes(NONE), vec![0x7f]);
        // Ctrl+Backspace は BS
        assert_eq!(backspace_bytes(m(false, false, true)), vec![0x08]);
        // Alt+Backspace は readline の backward-kill-word
        assert_eq!(backspace_bytes(m(false, true, false)), vec![0x1b, 0x7f]);
    }

    #[test]
    fn tab_shift_is_back_tab() {
        assert_eq!(tab_bytes(NONE), vec![b'\t']);
        assert_eq!(tab_bytes(m(true, false, false)), b"\x1b[Z");
        assert_eq!(tab_bytes(m(false, true, false)), vec![0x1b, b'\t']);
    }

    #[test]
    fn ctrl_space_sends_nothing_so_ime_toggle_keeps_working() {
        assert_eq!(space_bytes(NONE), Some(vec![b' ']));
        assert_eq!(space_bytes(m(false, true, false)), Some(vec![0x1b, b' ']));
        // NUL を送ると vim の i_CTRL-@ が発動して挿入モードを抜けてしまう。
        // Ctrl+Space は IME 切り替えに使われるので端末からは何も送らない。
        assert_eq!(space_bytes(m(false, false, true)), None);
        assert_eq!(space_bytes(m(true, false, true)), None);
    }

    #[test]
    fn shift_enter_sends_escape_and_cr() {
        assert_eq!(enter_bytes(NONE), vec![b'\r']);
        // Claude Code 等が「送信せず改行」として扱う
        assert_eq!(enter_bytes(m(true, false, false)), vec![0x1b, b'\r']);
    }

    #[test]
    fn alt_escape_sends_two_escapes() {
        assert_eq!(escape_bytes(NONE), vec![0x1b]);
        assert_eq!(escape_bytes(m(false, true, false)), vec![0x1b, 0x1b]);
    }

    #[test]
    fn cursor_key_sequence_covers_every_variant() {
        // 分岐漏れがあると無反応キーが生まれるので、全組み合わせで空でないこと。
        for key in [
            CursorKey::Up,
            CursorKey::Down,
            CursorKey::Right,
            CursorKey::Left,
            CursorKey::Home,
            CursorKey::End,
        ] {
            for application in [false, true] {
                assert!(!cursor_key_sequence(key, application).is_empty());
                for mods in [NONE, m(true, false, false), m(false, true, false), m(false, false, true)] {
                    assert!(!cursor_key_bytes(key, application, mods).is_empty());
                }
            }
        }
    }
}
