//! Adaptador git local sobre el binario `git` del host.
//!
//! Usa `git status --porcelain=v2 -z` para tener un formato estable (renames,
//! unicode y espacios incluidos). Todas las operaciones son de solo lectura
//! salvo stage/unstage/commit explícitos.

use std::ffi::OsString;
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Output};
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GitError {
    NotARepository(PathBuf),
    Io(String),
    Exit { code: Option<i32>, stderr: String },
    InvalidPath(String),
    EmptyMessage,
}

impl std::fmt::Display for GitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GitError::NotARepository(path) => {
                write!(f, "No es un repositorio git: {}", path.display())
            }
            GitError::Io(message) => write!(f, "{message}"),
            GitError::Exit { code, stderr } => {
                write!(f, "{} ({code:?}): {stderr}", termust_i18n::tr("git failed"))
            }
            GitError::InvalidPath(path) => write!(f, "Ruta no permitida: {path}"),
            GitError::EmptyMessage => {
                write!(f, "{}", termust_i18n::tr("The commit message is empty"))
            }
        }
    }
}

impl std::error::Error for GitError {}

/// Ejecuta git en un directorio. La app lo implementa sobre `host-exec`.
pub trait GitRunner: Send + Sync {
    fn run(&self, cwd: &Path, args: &[OsString]) -> Result<Output, GitError>;
}

/// Runner directo para tests y para uso local fuera de Flatpak.
pub struct ProcessGitRunner;

impl GitRunner for ProcessGitRunner {
    fn run(&self, cwd: &Path, args: &[OsString]) -> Result<Output, GitError> {
        Command::new("git")
            .current_dir(cwd)
            .args(args)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .map_err(|error| GitError::Io(format!("no se pudo ejecutar git: {error}")))
    }
}

#[derive(Clone)]
pub struct Git {
    runner: Arc<dyn GitRunner>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Repo {
    pub root: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Branch {
    pub head: String,
    pub upstream: Option<String>,
    pub ahead: usize,
    pub behind: usize,
    pub detached: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeKind {
    Modified,
    Added,
    Deleted,
    Renamed,
    Copied,
    TypeChanged,
    Untracked,
    Ignored,
    Conflict,
}

impl ChangeKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Modified => "M",
            Self::Added => "A",
            Self::Deleted => "D",
            Self::Renamed => "R",
            Self::Copied => "C",
            Self::TypeChanged => "T",
            Self::Untracked => "?",
            Self::Ignored => "!",
            Self::Conflict => "U",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangedFile {
    pub path: PathBuf,
    pub orig_path: Option<PathBuf>,
    pub index: char,
    pub worktree: char,
    pub kind: ChangeKind,
}

impl ChangedFile {
    pub fn is_staged(&self) -> bool {
        !matches!(
            self.kind,
            ChangeKind::Untracked | ChangeKind::Ignored | ChangeKind::Conflict
        ) && self.index != '.'
            && self.index != '?'
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepoStatus {
    pub branch: Branch,
    pub files: Vec<ChangedFile>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffLineKind {
    File,
    Hunk,
    Add,
    Del,
    Context,
    Meta,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: DiffLineKind,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diff {
    pub lines: Vec<DiffLine>,
    pub truncated: bool,
    pub binary: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitResult {
    pub oid: String,
    pub summary: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommitHint {
    pub subject_len: usize,
    pub subject_too_long: bool,
    pub body_line_too_long: bool,
}

impl Git {
    pub fn new(runner: Arc<dyn GitRunner>) -> Self {
        Self { runner }
    }

    pub fn discover(&self, cwd: &Path) -> Result<Repo, GitError> {
        let output = self.run_str(cwd, &["rev-parse", "--show-toplevel"])?;
        let output = match output.status.success() {
            true => output,
            false => return Err(GitError::NotARepository(cwd.to_path_buf())),
        };
        let root = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if root.is_empty() {
            return Err(GitError::NotARepository(cwd.to_path_buf()));
        }
        Ok(Repo {
            root: PathBuf::from(root),
        })
    }

    pub fn status(&self, repo: &Repo) -> Result<RepoStatus, GitError> {
        let output = self.run_str(
            &repo.root,
            &[
                "status",
                "--porcelain=v2",
                "--branch",
                "--untracked-files=normal",
                "--ignored=matching",
                "-z",
            ],
        )?;
        let output = ensure_success(output)?;
        Ok(parse_status(&output.stdout))
    }

    pub fn diff(
        &self,
        repo: &Repo,
        path: Option<&Path>,
        staged: bool,
        cap: usize,
    ) -> Result<Diff, GitError> {
        let mut args: Vec<OsString> = [
            "diff",
            "--no-color",
            "--no-ext-diff",
            "-U3",
            if staged { "--cached" } else { "" },
        ]
        .iter()
        .filter(|arg| !arg.is_empty())
        .map(OsString::from)
        .collect();
        if let Some(path) = path {
            let relative = validate_pathspec(&repo.root, path)?;
            args.push(OsString::from("--"));
            args.push(relative.into_os_string());
        }
        let output = self.runner.run(&repo.root, &args)?;
        let output = ensure_success(output)?;
        let text = String::from_utf8_lossy(&output.stdout);
        let mut lines = Vec::new();
        let mut binary = false;
        for line in text.lines() {
            if line.starts_with("Binary files ") || line.contains("GIT binary patch") {
                binary = true;
                break;
            }
            let kind = if line.starts_with("@@") {
                DiffLineKind::Hunk
            } else if line.starts_with("diff --git")
                || line.starts_with("index ")
                || line.starts_with("--- ")
                || line.starts_with("+++ ")
            {
                DiffLineKind::File
            } else if line.starts_with('+') {
                DiffLineKind::Add
            } else if line.starts_with('-') {
                DiffLineKind::Del
            } else if line.starts_with(' ') {
                DiffLineKind::Context
            } else {
                DiffLineKind::Meta
            };
            lines.push(DiffLine {
                kind,
                text: line.to_string(),
            });
        }
        if binary {
            lines.clear();
        }
        let truncated = lines.len() > cap;
        if truncated {
            lines.truncate(cap);
        }
        Ok(Diff {
            lines,
            truncated,
            binary,
        })
    }

    pub fn stage(&self, repo: &Repo, paths: &[PathBuf]) -> Result<(), GitError> {
        let mut args = vec![
            OsString::from("add"),
            OsString::from("-A"),
            OsString::from("--"),
        ];
        for path in paths {
            args.push(validate_pathspec(&repo.root, path)?.into_os_string());
        }
        let output = self.runner.run(&repo.root, &args)?;
        ensure_success(output)?;
        Ok(())
    }

    pub fn unstage(&self, repo: &Repo, paths: &[PathBuf]) -> Result<(), GitError> {
        let mut args = vec![
            OsString::from("reset"),
            OsString::from("-q"),
            OsString::from("HEAD"),
            OsString::from("--"),
        ];
        for path in paths {
            args.push(validate_pathspec(&repo.root, path)?.into_os_string());
        }
        let output = self.runner.run(&repo.root, &args)?;
        if output.status.success() {
            return Ok(());
        }
        let mut args = vec![
            OsString::from("rm"),
            OsString::from("--cached"),
            OsString::from("-q"),
            OsString::from("--"),
        ];
        for path in paths {
            args.push(validate_pathspec(&repo.root, path)?.into_os_string());
        }
        let output = self.runner.run(&repo.root, &args)?;
        ensure_success(output)?;
        Ok(())
    }

    pub fn commit(&self, repo: &Repo, message: &str) -> Result<CommitResult, GitError> {
        let message = message.trim();
        if message.is_empty() || message.contains('\0') {
            return Err(GitError::EmptyMessage);
        }
        let args = [
            OsString::from("commit"),
            OsString::from("-m"),
            OsString::from(message),
        ];
        let output = self.runner.run(&repo.root, &args)?;
        ensure_success(output)?;
        let output = self.run_str(&repo.root, &["rev-parse", "HEAD"])?;
        let output = ensure_success(output)?;
        Ok(CommitResult {
            oid: String::from_utf8_lossy(&output.stdout).trim().to_string(),
            summary: message.lines().next().unwrap_or_default().to_string(),
        })
    }

    fn run_str(&self, cwd: &Path, args: &[&str]) -> Result<Output, GitError> {
        let args: Vec<OsString> = args.iter().map(OsString::from).collect();
        self.runner.run(cwd, &args)
    }
}

/// Valida y normaliza un pathspec relativo al repositorio.
pub fn validate_pathspec(root: &Path, path: &Path) -> Result<PathBuf, GitError> {
    let raw = path.as_os_str().as_bytes();
    if raw.is_empty() || raw.contains(&0) {
        return Err(GitError::InvalidPath(path.display().to_string()));
    }
    if path == Path::new(".") {
        return Ok(PathBuf::from("."));
    }
    let relative = if path.is_absolute() {
        path.strip_prefix(root)
            .map_err(|_| GitError::InvalidPath(path.display().to_string()))?
            .to_path_buf()
    } else {
        path.to_path_buf()
    };
    let mut out = PathBuf::new();
    for component in relative.components() {
        match component {
            Component::Normal(part) => {
                let text = part.to_string_lossy();
                if text == ".git" || text.contains('\0') {
                    return Err(GitError::InvalidPath(path.display().to_string()));
                }
                if out.as_os_str().is_empty() && text.starts_with('-') {
                    return Err(GitError::InvalidPath(path.display().to_string()));
                }
                out.push(part);
            }
            Component::CurDir => {}
            _ => return Err(GitError::InvalidPath(path.display().to_string())),
        }
    }
    if out.as_os_str().is_empty() {
        return Err(GitError::InvalidPath(path.display().to_string()));
    }
    Ok(out)
}

pub fn commit_hint(message: &str) -> CommitHint {
    let mut lines = message.lines();
    let subject = lines.next().unwrap_or("");
    let subject_len = subject.chars().count();
    let body_line_too_long = lines
        .skip_while(|line| line.trim().is_empty())
        .any(|line| line.chars().count() > 72);
    CommitHint {
        subject_len,
        subject_too_long: subject_len > 50,
        body_line_too_long,
    }
}

fn ensure_success(output: Output) -> Result<Output, GitError> {
    if output.status.success() {
        return Ok(output);
    }
    Err(GitError::Exit {
        code: output.status.code(),
        stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
    })
}

fn parse_status(bytes: &[u8]) -> RepoStatus {
    let parts: Vec<String> = bytes
        .split(|byte| *byte == 0)
        .map(|part| String::from_utf8_lossy(part).to_string())
        .collect();
    let mut branch = Branch {
        head: String::new(),
        upstream: None,
        ahead: 0,
        behind: 0,
        detached: false,
    };
    let mut files = Vec::new();
    let mut index = 0;
    while index < parts.len() {
        let record = &parts[index];
        index += 1;
        if record.is_empty() {
            continue;
        }
        if let Some(value) = record.strip_prefix("# branch.head ") {
            branch.head = value.to_string();
            branch.detached = value == "(detached)";
        } else if let Some(value) = record.strip_prefix("# branch.upstream ") {
            branch.upstream = Some(value.to_string());
        } else if let Some(value) = record.strip_prefix("# branch.ab ") {
            let mut fields = value.split_whitespace();
            if let Some(ahead) = fields.next().and_then(|v| v.strip_prefix('+')) {
                branch.ahead = ahead.parse().unwrap_or(0);
            }
            if let Some(behind) = fields.next().and_then(|v| v.strip_prefix('-')) {
                branch.behind = behind.parse().unwrap_or(0);
            }
        } else if let Some(rest) = record.strip_prefix("1 ") {
            let fields: Vec<&str> = rest.splitn(8, ' ').collect();
            if fields.len() == 8 {
                let status = fields[0];
                let (index_status, worktree_status) = status_chars(status);
                files.push(ChangedFile {
                    path: PathBuf::from(fields[7]),
                    orig_path: None,
                    index: index_status,
                    worktree: worktree_status,
                    kind: kind_for(index_status, worktree_status, false),
                });
            }
        } else if let Some(rest) = record.strip_prefix("2 ") {
            let fields: Vec<&str> = rest.splitn(9, ' ').collect();
            if fields.len() == 9 {
                let status = fields[0];
                let (index_status, worktree_status) = status_chars(status);
                let orig = parts.get(index).cloned();
                index += 1;
                files.push(ChangedFile {
                    path: PathBuf::from(fields[8]),
                    orig_path: orig.map(PathBuf::from),
                    index: index_status,
                    worktree: worktree_status,
                    kind: if index_status == 'C' || worktree_status == 'C' {
                        ChangeKind::Copied
                    } else {
                        ChangeKind::Renamed
                    },
                });
            }
        } else if let Some(rest) = record.strip_prefix("u ") {
            let fields: Vec<&str> = rest.splitn(10, ' ').collect();
            if fields.len() == 10 {
                let status = fields[0];
                let (index_status, worktree_status) = status_chars(status);
                files.push(ChangedFile {
                    path: PathBuf::from(fields[9]),
                    orig_path: None,
                    index: index_status,
                    worktree: worktree_status,
                    kind: ChangeKind::Conflict,
                });
            }
        } else if let Some(path) = record.strip_prefix("? ") {
            files.push(ChangedFile {
                path: PathBuf::from(path),
                orig_path: None,
                index: '?',
                worktree: '?',
                kind: ChangeKind::Untracked,
            });
        } else if let Some(path) = record.strip_prefix("! ") {
            files.push(ChangedFile {
                path: PathBuf::from(path),
                orig_path: None,
                index: '!',
                worktree: '!',
                kind: ChangeKind::Ignored,
            });
        }
    }
    RepoStatus { branch, files }
}

fn status_chars(status: &str) -> (char, char) {
    let mut chars = status.chars();
    (chars.next().unwrap_or('.'), chars.next().unwrap_or('.'))
}

fn kind_for(index: char, worktree: char, conflict: bool) -> ChangeKind {
    if conflict {
        return ChangeKind::Conflict;
    }
    if index == 'R' || worktree == 'R' {
        ChangeKind::Renamed
    } else if index == 'C' || worktree == 'C' {
        ChangeKind::Copied
    } else if index == 'D' || worktree == 'D' {
        ChangeKind::Deleted
    } else if index == 'A' || worktree == 'A' {
        ChangeKind::Added
    } else if index == 'T' || worktree == 'T' {
        ChangeKind::TypeChanged
    } else {
        ChangeKind::Modified
    }
}

/// Para la UI: separa las entradas en staged y no staged.
pub fn split_staged(files: &[ChangedFile]) -> (Vec<&ChangedFile>, Vec<&ChangedFile>) {
    files
        .iter()
        .partition(|file| file.is_staged() && file.kind != ChangeKind::Conflict)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Output};
    use std::sync::Arc;

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "termust-git-{label}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            fs::create_dir_all(&path).unwrap();
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn output(dir: &Path, args: &[&str]) -> Output {
        Command::new("git")
            .current_dir(dir)
            .args(args)
            .env("GIT_AUTHOR_NAME", "Termust")
            .env("GIT_AUTHOR_EMAIL", "termust@example.com")
            .env("GIT_COMMITTER_NAME", "Termust")
            .env("GIT_COMMITTER_EMAIL", "termust@example.com")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .expect("git")
    }

    fn ok(dir: &Path, args: &[&str]) -> String {
        let out = output(dir, args);
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).to_string()
    }

    fn repo(label: &str) -> TempDir {
        let dir = TempDir::new(label);
        ok(dir.path(), &["init", "-q", "-b", "main"]);
        ok(dir.path(), &["config", "user.name", "Termust"]);
        ok(dir.path(), &["config", "user.email", "termust@example.com"]);
        ok(dir.path(), &["config", "commit.gpgsign", "false"]);
        fs::write(dir.path().join("a.txt"), b"a\n").unwrap();
        fs::write(dir.path().join("ñandú file.txt"), b"hola\n").unwrap();
        fs::write(dir.path().join("bin.dat"), [0u8, 1, 2, 3]).unwrap();
        ok(dir.path(), &["add", "-A"]);
        ok(dir.path(), &["commit", "-q", "-m", "init"]);
        dir
    }

    fn git() -> Git {
        Git::new(Arc::new(ProcessGitRunner))
    }

    #[test]
    fn git_discover_from_active_pane() {
        let dir = repo("discover");
        let nested = dir.path().join("src/deep");
        fs::create_dir_all(&nested).unwrap();
        let repo = git().discover(&nested).expect("repo");
        assert_eq!(
            fs::canonicalize(&repo.root).unwrap(),
            fs::canonicalize(dir.path()).unwrap()
        );
        let outside = TempDir::new("outside");
        assert!(matches!(
            git().discover(outside.path()),
            Err(GitError::NotARepository(_))
        ));
    }

    #[test]
    fn git_status_handles_rename_conflict_binary_unicode() {
        let dir = repo("status");
        ok(dir.path(), &["mv", "a.txt", "b.txt"]);
        fs::write(dir.path().join("ñandú file.txt"), b"hola mundo\n").unwrap();
        let repo = git().discover(dir.path()).expect("repo");
        let status = git().status(&repo).expect("status");
        assert!(!status.branch.head.is_empty());
        let renamed = status
            .files
            .iter()
            .find(|file| file.path == Path::new("b.txt"))
            .expect("rename");
        assert_eq!(renamed.kind, ChangeKind::Renamed);
        assert_eq!(renamed.orig_path.as_deref(), Some(Path::new("a.txt")));
        let unicode = status
            .files
            .iter()
            .find(|file| file.path == Path::new("ñandú file.txt"))
            .expect("unicode");
        assert_eq!(unicode.kind, ChangeKind::Modified);

        fs::write(dir.path().join("bin.dat"), [0u8, 9, 8, 7]).unwrap();
        let diff = git()
            .diff(&repo, Some(Path::new("bin.dat")), false, 5_000)
            .expect("diff");
        assert!(diff.binary);

        ok(dir.path(), &["checkout", "-q", "-b", "other"]);
        fs::write(dir.path().join("ñandú file.txt"), b"other\n").unwrap();
        ok(dir.path(), &["commit", "-qam", "other"]);
        ok(dir.path(), &["checkout", "-q", "main"]);
        fs::write(dir.path().join("ñandú file.txt"), b"main\n").unwrap();
        ok(dir.path(), &["commit", "-qam", "main"]);
        let merge = output(dir.path(), &["merge", "other"]);
        assert!(!merge.status.success(), "el merge debe conflictuar");
        let status = git().status(&repo).expect("status conflicto");
        let conflict = status
            .files
            .iter()
            .find(|file| file.path == Path::new("ñandú file.txt"))
            .expect("conflicto");
        assert_eq!(conflict.kind, ChangeKind::Conflict);
    }

    #[test]
    fn git_stage_pathspec_validated() {
        let dir = repo("stage");
        fs::write(dir.path().join("ok.txt"), b"nuevo\n").unwrap();
        let repo = git().discover(dir.path()).expect("repo");
        let root = fs::canonicalize(dir.path()).unwrap();

        assert!(validate_pathspec(&root, Path::new("../etc/passwd")).is_err());
        assert!(validate_pathspec(&root, Path::new("/etc/passwd")).is_err());
        assert!(validate_pathspec(&root, Path::new("-rf")).is_err());
        assert!(validate_pathspec(&root, Path::new("")).is_err());
        assert_eq!(
            validate_pathspec(&root, Path::new("ok.txt")).unwrap(),
            PathBuf::from("ok.txt")
        );

        git()
            .stage(&repo, &[PathBuf::from("ok.txt")])
            .expect("stage");
        let status = git().status(&repo).expect("status staged");
        let staged = status
            .files
            .iter()
            .find(|file| file.path == Path::new("ok.txt"))
            .expect("ok.txt");
        assert_eq!(staged.kind, ChangeKind::Added);
        assert_eq!(staged.index, 'A');

        git()
            .unstage(&repo, &[PathBuf::from("ok.txt")])
            .expect("unstage");
        let status = git().status(&repo).expect("status unstaged");
        let unstaged = status
            .files
            .iter()
            .find(|file| file.path == Path::new("ok.txt"))
            .expect("ok.txt");
        assert_eq!(unstaged.kind, ChangeKind::Untracked);

        assert!(git().stage(&repo, &[PathBuf::from("../escape")]).is_err());
    }

    #[test]
    fn git_commit_requires_message() {
        let dir = repo("commit");
        fs::write(dir.path().join("a.txt"), b"changed\n").unwrap();
        let repo = git().discover(dir.path()).expect("repo");
        git()
            .stage(&repo, &[PathBuf::from("a.txt")])
            .expect("stage");
        let before = ok(dir.path(), &["rev-list", "--count", "HEAD"]);
        assert!(matches!(
            git().commit(&repo, "   "),
            Err(GitError::EmptyMessage)
        ));
        assert_eq!(ok(dir.path(), &["rev-list", "--count", "HEAD"]), before);
        let result = git().commit(&repo, "feat: cambio").expect("commit");
        assert!(!result.oid.is_empty());
        assert!(ok(dir.path(), &["log", "-1", "--format=%s"]).contains("feat: cambio"));

        let hint = commit_hint(&"x".repeat(60));
        assert!(hint.subject_too_long);
        let hint = commit_hint("feat: ok\n\ncuerpo");
        assert!(!hint.subject_too_long);
        let hint = commit_hint(&format!("feat: ok\n\n{}", "y".repeat(80)));
        assert!(hint.body_line_too_long);
    }

    #[test]
    fn git_diff_caps_lines_and_marks_lines() {
        let dir = repo("diff");
        let mut content = String::from("base\n");
        for index in 0..6_000 {
            content.push_str(&format!("linea {index}\n"));
        }
        fs::write(dir.path().join("a.txt"), content).unwrap();
        ok(dir.path(), &["add", "a.txt"]);
        let repo = git().discover(dir.path()).expect("repo");
        let diff = git()
            .diff(&repo, Some(Path::new("a.txt")), true, 100)
            .expect("diff cap");
        assert!(diff.truncated);
        assert!(diff.lines.len() <= 100);
        assert!(diff.lines.iter().any(|line| line.kind == DiffLineKind::Add));
        let full = git()
            .diff(&repo, Some(Path::new("a.txt")), true, 20_000)
            .expect("diff full");
        assert!(!full.truncated);
        assert!(full.lines.len() > 6_000);
    }

    #[test]
    fn git_ignored_status() {
        let dir = repo("ignored");
        fs::write(dir.path().join(".gitignore"), b"*.log\n").unwrap();
        fs::write(dir.path().join("debug.log"), b"x").unwrap();
        let repo = git().discover(dir.path()).expect("repo");
        let status = git().status(&repo).expect("status");
        let ignored = status
            .files
            .iter()
            .find(|file| file.path == Path::new("debug.log"))
            .expect("ignorado");
        assert_eq!(ignored.kind, ChangeKind::Ignored);
    }
}
