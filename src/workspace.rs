use std::path::{Path, PathBuf};
use std::process::Command;

pub struct GitSummary {
    pub branch: String,
    pub staged: usize,
    pub modified: usize,
    pub untracked: usize,
    pub deleted: usize,
}

pub struct WorkspaceInfo {
    pub cwd: PathBuf,
    pub git: Option<GitSummary>,
}

pub fn collect(cwd: &Path) -> WorkspaceInfo {
    let mut cmd = Command::new("git");
    cmd.args(["status", "--porcelain=v2", "--branch"])
        .current_dir(cwd);
    // Windows でコンソール窓を出さない。これが無いと git を呼ぶたび（5秒ごと・
    // cd ごと）に一瞬コマンドプロンプトが開き、分割で cwd 切替が増えると
    // 「画面が何度も開く」ように見える。
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    let git = cmd
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|text| parse_porcelain_v2(&text));

    WorkspaceInfo {
        cwd: cwd.to_path_buf(),
        git,
    }
}

fn parse_porcelain_v2(text: &str) -> GitSummary {
    let mut summary = GitSummary {
        branch: String::new(),
        staged: 0,
        modified: 0,
        untracked: 0,
        deleted: 0,
    };

    for line in text.lines() {
        if let Some(branch) = line.strip_prefix("# branch.head ") {
            summary.branch = branch.to_owned();
            continue;
        }

        if line.starts_with("? ") {
            summary.untracked += 1;
            continue;
        }

        if !(line.starts_with("1 ") || line.starts_with("2 ")) {
            continue;
        }

        let Some(xy) = line.split_whitespace().nth(1) else {
            continue;
        };
        let mut chars = xy.chars();
        let x = chars.next().unwrap_or('.');
        let y = chars.next().unwrap_or('.');

        if x != '.' {
            summary.staged += 1;
        }
        if y != '.' {
            summary.modified += 1;
        }
        if y == 'D' {
            summary.deleted += 1;
        }
    }

    summary
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_clean_status() {
        let summary = parse_porcelain_v2("# branch.oid abc\n# branch.head main\n");

        assert_eq!(summary.branch, "main");
        assert_eq!(summary.staged, 0);
        assert_eq!(summary.modified, 0);
        assert_eq!(summary.untracked, 0);
        assert_eq!(summary.deleted, 0);
    }

    #[test]
    fn parses_modified_worktree_files() {
        let summary =
            parse_porcelain_v2("# branch.head main\n1 .M N... 100644 100644 100644 a b file.rs\n");

        assert_eq!(summary.staged, 0);
        assert_eq!(summary.modified, 1);
        assert_eq!(summary.deleted, 0);
    }

    #[test]
    fn parses_staged_and_unstaged_mix() {
        let summary = parse_porcelain_v2(
            "# branch.head feature\n\
             1 M. N... 100644 100644 100644 a b staged.rs\n\
             1 MM N... 100644 100644 100644 a b both.rs\n",
        );

        assert_eq!(summary.branch, "feature");
        assert_eq!(summary.staged, 2);
        assert_eq!(summary.modified, 1);
    }

    #[test]
    fn parses_untracked_files() {
        let summary = parse_porcelain_v2("# branch.head main\n? new.rs\n? docs/new.md\n");

        assert_eq!(summary.untracked, 2);
    }

    #[test]
    fn parses_worktree_deleted_files() {
        let summary =
            parse_porcelain_v2("# branch.head main\n1 .D N... 100644 100644 000000 a b gone.rs\n");

        assert_eq!(summary.modified, 1);
        assert_eq!(summary.deleted, 1);
    }

    #[test]
    fn parses_rename_lines() {
        let summary = parse_porcelain_v2(
            "# branch.head main\n\
             2 RM N... 100644 100644 100644 a b R100 new.rs\told.rs\n",
        );

        assert_eq!(summary.staged, 1);
        assert_eq!(summary.modified, 1);
    }
}
