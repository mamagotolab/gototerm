use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Target {
    Url(String),
    File(PathBuf),
}

#[derive(Clone, Debug)]
pub(crate) struct Hint {
    pub row: usize,
    pub col: usize,
    pub label: String,
    pub target: Target,
}

pub(crate) struct LinkHints {
    pub hints: Vec<Hint>,
    pub prefix: String,
}

impl LinkHints {
    pub fn new(targets: Vec<(usize, usize, Target)>) -> Option<Self> {
        if targets.is_empty() {
            return None;
        }
        // Fixed-length labels are prefix-free; never let an early match hide a later one.
        let two = targets.len() > 26;
        let hints = targets
            .into_iter()
            .take(26 * 26)
            .enumerate()
            .map(|(i, (row, col, target))| {
                let label = if two {
                    format!(
                        "{}{}",
                        (b'a' + (i / 26) as u8) as char,
                        (b'a' + (i % 26) as u8) as char
                    )
                } else {
                    ((b'a' + i as u8) as char).to_string()
                };
                Hint {
                    row,
                    col,
                    label,
                    target,
                }
            })
            .collect();
        Some(Self {
            hints,
            prefix: String::new(),
        })
    }

    pub fn input(&mut self, ch: char) -> Result<Option<Target>, ()> {
        self.prefix.push(ch.to_ascii_lowercase());
        if let Some(hint) = self.hints.iter().find(|hint| hint.label == self.prefix) {
            return Ok(Some(hint.target.clone()));
        }
        if self
            .hints
            .iter()
            .any(|hint| hint.label.starts_with(&self.prefix))
        {
            Ok(None)
        } else {
            Err(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn every_hint_is_reachable_without_prefix_collision() {
        for count in [1, 26, 27, 676, 700] {
            let targets = (0..count)
                .map(|i| (i, 0, Target::Url(format!("https://example.org/{i}"))))
                .collect();
            let hints = LinkHints::new(targets).unwrap();
            for hint in &hints.hints {
                let targets = (0..count)
                    .map(|i| (i, 0, Target::Url(format!("https://example.org/{i}"))))
                    .collect();
                let mut state = LinkHints::new(targets).unwrap();
                let mut result = None;
                for ch in hint.label.chars() {
                    result = state.input(ch).unwrap();
                }
                assert_eq!(result, Some(hint.target.clone()));
            }
        }
        assert!(LinkHints::new(vec![]).is_none());
    }
}
