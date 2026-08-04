#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CursorKey {
    Up,
    Down,
    Right,
    Left,
}

pub(crate) fn cursor_key_sequence(key: CursorKey, application: bool) -> &'static [u8] {
    match (application, key) {
        (false, CursorKey::Up) => b"\x1b[A",
        (false, CursorKey::Down) => b"\x1b[B",
        (false, CursorKey::Right) => b"\x1b[C",
        (false, CursorKey::Left) => b"\x1b[D",
        (true, CursorKey::Up) => b"\x1bOA",
        (true, CursorKey::Down) => b"\x1bOB",
        (true, CursorKey::Right) => b"\x1bOC",
        (true, CursorKey::Left) => b"\x1bOD",
    }
}

/// 修飾キー付きの矢印キーを送る（xterm 準拠の `CSI 1 ; <修飾> <終端>`）。
///
/// 修飾キーを落とすと、単語単位のカーソル移動（readline の Ctrl+←/→）や
/// TUI の Shift+矢印による選択が効かなくなる。実際 Ctrl+矢印は、修飾ありの
/// 分岐が無かったため何も送られず完全に無反応だった。
///
/// 修飾コードは 1 + Shift(1) + Alt(2) + Ctrl(4)。修飾が無いときだけ
/// application cursor mode の SS3 形式を使う（xterm も修飾ありでは CSI 形式）。
pub(crate) fn cursor_key_bytes(
    key: CursorKey,
    application: bool,
    shift: bool,
    alt: bool,
    ctrl: bool,
) -> Vec<u8> {
    let modifier = 1 + u8::from(shift) + 2 * u8::from(alt) + 4 * u8::from(ctrl);
    if modifier == 1 {
        return cursor_key_sequence(key, application).to_vec();
    }
    let final_byte = match key {
        CursorKey::Up => 'A',
        CursorKey::Down => 'B',
        CursorKey::Right => 'C',
        CursorKey::Left => 'D',
    };
    format!("\x1b[1;{modifier}{final_byte}").into_bytes()
}

#[cfg(test)]
mod tests {
    use super::{cursor_key_bytes, cursor_key_sequence, CursorKey};

    #[test]
    fn unmodified_arrows_keep_the_plain_sequences() {
        assert_eq!(
            cursor_key_bytes(CursorKey::Up, false, false, false, false),
            b"\x1b[A"
        );
        assert_eq!(
            cursor_key_bytes(CursorKey::Up, true, false, false, false),
            b"\x1bOA"
        );
    }

    #[test]
    fn ctrl_arrows_send_word_movement_sequences() {
        // readline の Ctrl+←/→（単語単位の移動）。修飾コード 5 = 1+Ctrl(4)。
        assert_eq!(
            cursor_key_bytes(CursorKey::Right, false, false, false, true),
            b"\x1b[1;5C"
        );
        assert_eq!(
            cursor_key_bytes(CursorKey::Left, false, false, false, true),
            b"\x1b[1;5D"
        );
    }

    #[test]
    fn shift_and_alt_arrows_use_their_own_modifier_codes() {
        assert_eq!(
            cursor_key_bytes(CursorKey::Up, false, true, false, false),
            b"\x1b[1;2A"
        );
        assert_eq!(
            cursor_key_bytes(CursorKey::Up, false, false, true, false),
            b"\x1b[1;3A"
        );
        // 1 + Shift(1) + Alt(2) + Ctrl(4) = 8
        assert_eq!(
            cursor_key_bytes(CursorKey::Down, false, true, true, true),
            b"\x1b[1;8B"
        );
    }

    #[test]
    fn modified_arrows_use_csi_even_in_application_cursor_mode() {
        // xterm は修飾ありでは SS3 ではなく CSI 形式を送る。
        assert_eq!(
            cursor_key_bytes(CursorKey::Left, true, false, false, true),
            b"\x1b[1;5D"
        );
    }

    #[test]
    fn cursor_keys_use_csi_in_normal_mode() {
        assert_eq!(cursor_key_sequence(CursorKey::Up, false), b"\x1b[A");
        assert_eq!(cursor_key_sequence(CursorKey::Down, false), b"\x1b[B");
        assert_eq!(cursor_key_sequence(CursorKey::Right, false), b"\x1b[C");
        assert_eq!(cursor_key_sequence(CursorKey::Left, false), b"\x1b[D");
    }

    #[test]
    fn cursor_keys_use_ss3_in_application_mode() {
        assert_eq!(cursor_key_sequence(CursorKey::Up, true), b"\x1bOA");
        assert_eq!(cursor_key_sequence(CursorKey::Down, true), b"\x1bOB");
        assert_eq!(cursor_key_sequence(CursorKey::Right, true), b"\x1bOC");
        assert_eq!(cursor_key_sequence(CursorKey::Left, true), b"\x1bOD");
    }
}
