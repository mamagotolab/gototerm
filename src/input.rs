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

#[cfg(test)]
mod tests {
    use super::{cursor_key_sequence, CursorKey};

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
