use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct WorkspaceSet {
    pub name: String,
    pub tabs: Vec<SavedNode>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum SavedNode {
    Pane {
        cwd: PathBuf,
    },
    Split {
        vertical: bool,
        ratio: f64,
        first: Box<SavedNode>,
        second: Box<SavedNode>,
    },
}

#[derive(Serialize, Deserialize)]
struct Store {
    version: u32,
    sets: Vec<WorkspaceSet>,
}

pub(crate) fn validate_name(name: &str) -> Result<(), String> {
    if name.trim().is_empty()
        || name.chars().count() > 80
        || name
            .chars()
            .any(|ch| ch.is_control() || matches!(ch, '/' | '\\'))
    {
        Err("名前は1〜80文字で、改行やパス区切りを含めないでください".into())
    } else {
        Ok(())
    }
}

fn path() -> Result<PathBuf, String> {
    crate::config::find_config_file()
        .and_then(|path| path.parent().map(|dir| dir.join("workspace-sets.json")))
        .ok_or_else(|| "設定フォルダが見つかりません".into())
}

pub(crate) fn load_all() -> Result<Vec<WorkspaceSet>, String> {
    load(&path()?)
}

fn valid_tree(node: &SavedNode, depth: usize, panes: &mut usize) -> bool {
    if depth > 16 || *panes >= 128 {
        return false;
    }
    match node {
        SavedNode::Pane { cwd } => {
            *panes += 1;
            cwd.is_absolute()
        }
        SavedNode::Split {
            ratio,
            first,
            second,
            ..
        } => {
            ratio.is_finite()
                && valid_tree(first, depth + 1, panes)
                && valid_tree(second, depth + 1, panes)
        }
    }
}

fn load(path: &Path) -> Result<Vec<WorkspaceSet>, String> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(error) => return Err(format!("作業セットを読めません: {error}")),
    };
    if bytes.len() > 1024 * 1024 {
        return Err("作業セットファイルが大きすぎます".into());
    }
    let store: Store =
        serde_json::from_slice(&bytes).map_err(|_| "作業セットのJSONが壊れています".to_string())?;
    let mut names = std::collections::HashSet::new();
    if store.version != 1
        || store.sets.len() > 100
        || store.sets.iter().any(|set| {
            let mut panes = 0;
            validate_name(&set.name).is_err()
                || !names.insert(&set.name)
                || set.tabs.is_empty()
                || set.tabs.len() > 32
                || !set.tabs.iter().all(|node| valid_tree(node, 0, &mut panes))
        })
    {
        return Err("作業セットの形式が不正です".into());
    }
    Ok(store.sets)
}

fn write(path: &Path, sets: Vec<WorkspaceSet>) -> Result<(), String> {
    let dir = path.parent().ok_or("保存先が不正です")?;
    std::fs::create_dir_all(dir).map_err(|error| error.to_string())?;
    let tmp = dir.join(format!(".workspace-sets-{}.tmp", std::process::id()));
    let bytes = serde_json::to_vec_pretty(&Store { version: 1, sets })
        .map_err(|error| error.to_string())?;
    if bytes.len() > 1024 * 1024 {
        return Err("作業セットファイルは1MBまでです".into());
    }
    std::fs::write(&tmp, bytes)
        .and_then(|_| std::fs::rename(&tmp, path))
        .map_err(|error| error.to_string())
}

pub(crate) fn save(set: WorkspaceSet) -> Result<(), String> {
    save_at(&path()?, set)
}

fn save_at(path: &Path, set: WorkspaceSet) -> Result<(), String> {
    validate_name(&set.name)?;
    let mut sets = load(path)?;
    if sets.iter().any(|item| item.name == set.name) {
        return Err("同じ名前が存在します。別名で保存してください".into());
    }
    let mut panes = 0;
    if set.tabs.is_empty()
        || set.tabs.len() > 32
        || !set.tabs.iter().all(|node| valid_tree(node, 0, &mut panes))
    {
        return Err("保存できるローカル配置がありません".into());
    }
    if sets.len() >= 100 {
        return Err("作業セットは100件まで保存できます".into());
    }
    sets.push(set);
    write(path, sets)
}

pub(crate) fn delete(name: &str) -> Result<(), String> {
    let path = path()?;
    let mut sets = load(&path)?;
    sets.retain(|set| set.name != name);
    write(&path, sets)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_rejects_duplicates_and_preserves_corrupt_file() {
        let dir =
            std::env::temp_dir().join(format!("gototerm-workspace-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("sets.json");
        let set = WorkspaceSet {
            name: "テスト".into(),
            tabs: vec![SavedNode::Pane {
                cwd: std::env::temp_dir(),
            }],
        };
        save_at(&file, set.clone()).unwrap();
        assert_eq!(load(&file).unwrap(), vec![set.clone()]);
        let before = std::fs::read(&file).unwrap();
        assert!(save_at(&file, set.clone()).is_err());
        assert_eq!(std::fs::read(&file).unwrap(), before);
        std::fs::write(&file, b"{partial").unwrap();
        assert!(load(&file).is_err());
        assert!(save_at(&file, set).is_err());
        assert_eq!(std::fs::read(&file).unwrap(), b"{partial");
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn workspace_tree_round_trip_and_validation() {
        let set = WorkspaceSet {
            name: "開発用".into(),
            tabs: vec![SavedNode::Split {
                vertical: true,
                ratio: 0.4,
                first: Box::new(SavedNode::Pane {
                    cwd: std::env::temp_dir(),
                }),
                second: Box::new(SavedNode::Pane {
                    cwd: std::env::temp_dir(),
                }),
            }],
        };
        let json = serde_json::to_string(&set).unwrap();
        assert_eq!(serde_json::from_str::<WorkspaceSet>(&json).unwrap(), set);
        assert!(!json.contains("command"));
        for name in ["", " ", "../bad", "a\\b", "a\nb"] {
            assert!(validate_name(name).is_err());
        }
        assert!(validate_name("作業 1").is_ok());
        let mut panes = 0;
        assert!(valid_tree(&set.tabs[0], 0, &mut panes));
        assert!(!valid_tree(
            &SavedNode::Pane {
                cwd: "relative".into()
            },
            0,
            &mut panes
        ));
    }
}
