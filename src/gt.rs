use std::path::PathBuf;

use crate::watcher::ChangeKind;

pub const GT_FILE_MAX_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GtMessage {
    Event {
        kind: ChangeKind,
        path: PathBuf,
        tool: Option<String>,
    },
    FileChunk {
        path: PathBuf,
        seq: u32,
        last: bool,
        data: Vec<u8>,
    },
}

pub fn parse_gt_message(payload: &str) -> Option<GtMessage> {
    let mut fields = payload.split(';');
    let typ = fields.next()?;
    let mut kind = None;
    let mut path = None;
    let mut tool = None;
    let mut seq = None;
    let mut last = None;
    let mut data = None;

    for field in fields {
        let (key, value) = field.split_once('=')?;
        match key {
            "kind" => kind = Some(parse_kind(value)?),
            "path" => path = Some(path_from_b64(value)?),
            "tool" => tool = Some(String::from_utf8(decode_base64(value)?).ok()?),
            "seq" => seq = Some(value.parse::<u32>().ok()?),
            "last" => {
                last = Some(match value {
                    "0" => false,
                    "1" => true,
                    _ => return None,
                });
            }
            "data" => data = Some(decode_base64(value)?),
            _ => {}
        }
    }

    match typ {
        "event" => Some(GtMessage::Event {
            kind: kind?,
            path: path?,
            tool,
        }),
        "file" => Some(GtMessage::FileChunk {
            path: path?,
            seq: seq?,
            last: last?,
            data: data?,
        }),
        _ => None,
    }
}

fn parse_kind(value: &str) -> Option<ChangeKind> {
    match value {
        "new" => Some(ChangeKind::New),
        "mod" => Some(ChangeKind::Modified),
        "del" => Some(ChangeKind::Deleted),
        _ => None,
    }
}

fn path_from_b64(value: &str) -> Option<PathBuf> {
    String::from_utf8(decode_base64(value)?)
        .ok()
        .map(PathBuf::from)
}

pub fn decode_base64(input: &str) -> Option<Vec<u8>> {
    let bytes = input.as_bytes();
    if bytes.is_empty() || bytes.len() % 4 != 0 {
        return None;
    }

    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    let chunks = bytes.chunks_exact(4);
    let chunk_count = chunks.len();

    for (index, chunk) in bytes.chunks_exact(4).enumerate() {
        let last_chunk = index + 1 == chunk_count;
        let pad = usize::from(chunk[2] == b'=') + usize::from(chunk[3] == b'=');
        if pad > 0 && !last_chunk {
            return None;
        }
        if chunk[2] == b'=' && chunk[3] != b'=' {
            return None;
        }

        let a = b64_value(chunk[0])?;
        let b = b64_value(chunk[1])?;
        let c = if chunk[2] == b'=' {
            0
        } else {
            b64_value(chunk[2])?
        };
        let d = if chunk[3] == b'=' {
            0
        } else {
            b64_value(chunk[3])?
        };

        // パディングで捨てられる下位ビットが立っている表現は不正として捨てる。
        if pad == 2 && (b & 0x0f) != 0 {
            return None;
        }
        if pad == 1 && (c & 0x03) != 0 {
            return None;
        }

        out.push((a << 2) | (b >> 4));
        if pad < 2 {
            out.push((b << 4) | (c >> 2));
        }
        if pad == 0 {
            out.push((c << 6) | d);
        }
    }

    Some(out)
}

fn b64_value(byte: u8) -> Option<u8> {
    match byte {
        b'A'..=b'Z' => Some(byte - b'A'),
        b'a'..=b'z' => Some(byte - b'a' + 26),
        b'0'..=b'9' => Some(byte - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

#[derive(Default)]
pub struct GtFileAssembler {
    path: Option<PathBuf>,
    next_seq: u32,
    data: Vec<u8>,
    broken: bool,
}

impl GtFileAssembler {
    pub fn push(
        &mut self,
        path: PathBuf,
        seq: u32,
        last: bool,
        mut chunk: Vec<u8>,
    ) -> Option<(PathBuf, Vec<u8>)> {
        if seq == 0 {
            self.path = Some(path.clone());
            self.next_seq = 0;
            self.data.clear();
            self.broken = false;
        }

        let current = self.path.as_ref()?;
        if self.broken || current != &path || seq != self.next_seq {
            self.reset();
            return None;
        }

        if self.data.len().saturating_add(chunk.len()) > GT_FILE_MAX_BYTES {
            self.reset();
            return None;
        }

        self.data.append(&mut chunk);
        self.next_seq = self.next_seq.checked_add(1).unwrap_or_else(|| {
            self.broken = true;
            0
        });

        if last && !self.broken {
            let done_path = self.path.take()?;
            let done_data = std::mem::take(&mut self.data);
            self.next_seq = 0;
            Some((done_path, done_data))
        } else {
            None
        }
    }

    fn reset(&mut self) {
        self.path = None;
        self.next_seq = 0;
        self.data.clear();
        self.broken = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_decodes_standard_padded_values() {
        assert_eq!(decode_base64("Zg=="), Some(b"f".to_vec()));
        assert_eq!(decode_base64("Zm8="), Some(b"fo".to_vec()));
        assert_eq!(decode_base64("Zm9v"), Some(b"foo".to_vec()));
    }

    #[test]
    fn base64_rejects_invalid_values() {
        assert_eq!(decode_base64("Zg"), None);
        assert_eq!(decode_base64("Z==="), None);
        assert_eq!(decode_base64("Zg=A"), None);
        assert_eq!(decode_base64("Zm$="), None);
    }

    #[test]
    fn parses_event_with_tool() {
        assert_eq!(
            parse_gt_message("event;kind=mod;path=c3JjL21haW4ucnM=;tool=RWRpdA=="),
            Some(GtMessage::Event {
                kind: ChangeKind::Modified,
                path: PathBuf::from("src/main.rs"),
                tool: Some("Edit".to_owned()),
            })
        );
    }

    #[test]
    fn parse_rejects_missing_key_unknown_type_and_huge_seq() {
        assert_eq!(parse_gt_message("event;kind=mod"), None);
        assert_eq!(parse_gt_message("wat;kind=mod;path=YQ=="), None);
        assert_eq!(
            parse_gt_message("file;path=YQ==;seq=4294967296;last=1;data=Yg=="),
            None
        );
    }

    #[test]
    fn parses_file_chunk() {
        assert_eq!(
            parse_gt_message("file;path=YS50eHQ=;seq=0;last=1;data=aGVsbG8="),
            Some(GtMessage::FileChunk {
                path: PathBuf::from("a.txt"),
                seq: 0,
                last: true,
                data: b"hello".to_vec(),
            })
        );
    }

    #[test]
    fn assembler_completes_two_chunks() {
        let mut a = GtFileAssembler::default();
        assert_eq!(
            a.push(PathBuf::from("a.txt"), 0, false, b"he".to_vec()),
            None
        );
        assert_eq!(
            a.push(PathBuf::from("a.txt"), 1, true, b"llo".to_vec()),
            Some((PathBuf::from("a.txt"), b"hello".to_vec()))
        );
    }

    #[test]
    fn assembler_drops_seq_gap() {
        let mut a = GtFileAssembler::default();
        assert_eq!(
            a.push(PathBuf::from("a.txt"), 0, false, b"a".to_vec()),
            None
        );
        assert_eq!(a.push(PathBuf::from("a.txt"), 2, true, b"b".to_vec()), None);
    }

    #[test]
    fn assembler_drops_interrupted_file() {
        let mut a = GtFileAssembler::default();
        assert_eq!(
            a.push(PathBuf::from("a.txt"), 0, false, b"a".to_vec()),
            None
        );
        assert_eq!(a.push(PathBuf::from("b.txt"), 1, true, b"b".to_vec()), None);
    }

    #[test]
    fn assembler_drops_oversized_transfer() {
        let mut a = GtFileAssembler::default();
        assert_eq!(
            a.push(
                PathBuf::from("a.txt"),
                0,
                true,
                vec![b'x'; GT_FILE_MAX_BYTES + 1]
            ),
            None
        );
    }
}
