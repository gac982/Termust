//! Ejecución en el host y materialización de la integración de shells.
//!
//! Es el único crate que puede construir `flatpak-spawn` (§5.3).

use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use termust_core::shell::resolve_shell;

const BASH_RC: &str = include_str!("../../../data/shell-integration/bash/rc.bash");
const ZSH_ENV: &str = include_str!("../../../data/shell-integration/zsh/.zshenv");
const ZSH_PROFILE: &str = include_str!("../../../data/shell-integration/zsh/.zprofile");
const ZSH_RC: &str = include_str!("../../../data/shell-integration/zsh/.zshrc");

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostError {
    NoHome,
    Io(String),
}

impl std::fmt::Display for HostError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HostError::NoHome => write!(f, "HOME is not set"),
            HostError::Io(e) => write!(f, "io: {e}"),
        }
    }
}

impl std::error::Error for HostError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellIntegration {
    pub bash_rc: PathBuf,
    pub zsh_dir: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellCommand {
    pub argv: Vec<OsString>,
    pub env: Vec<(OsString, OsString)>,
}

pub fn is_flatpak() -> bool {
    Path::new("/.flatpak-info").exists()
}

pub fn default_shell() -> PathBuf {
    if is_flatpak() {
        if let Some(shell) = host_login_shell() {
            return shell;
        }
    }
    resolve_shell(
        None,
        std::env::var_os("SHELL").as_ref(),
        Some(&PathBuf::from("/bin/sh")),
    )
}

/// Lee el shell de login del usuario en el host (no el $SHELL del sandbox).
fn host_login_shell() -> Option<PathBuf> {
    let uid = unsafe { libc::getuid() }.to_string();
    let out = std::process::Command::new("flatpak-spawn")
        .args(["--host", "getent", "passwd", &uid])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let line = String::from_utf8_lossy(&out.stdout);
    let shell = line.trim().rsplit(':').next()?;
    if shell.starts_with('/') {
        Some(PathBuf::from(shell))
    } else {
        None
    }
}

pub fn materialize() -> Result<ShellIntegration, HostError> {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))
        .ok_or(HostError::NoHome)?;
    materialize_in(&base.join("termust/shell-integration"))
}

pub fn materialize_in(root: &Path) -> Result<ShellIntegration, HostError> {
    let hash = content_hash();
    let dir = root.join(hash);
    let bash_rc = dir.join("bash.bashrc");
    let zsh_dir = dir.join("zsh");

    if bash_rc.exists() && zsh_dir.join(".zshrc").exists() {
        return Ok(ShellIntegration { bash_rc, zsh_dir });
    }

    fs::create_dir_all(&zsh_dir).map_err(|e| HostError::Io(e.to_string()))?;
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))
        .map_err(|e| HostError::Io(e.to_string()))?;
    fs::set_permissions(&zsh_dir, fs::Permissions::from_mode(0o700))
        .map_err(|e| HostError::Io(e.to_string()))?;

    write_atomic(&bash_rc, BASH_RC)?;
    write_atomic(&zsh_dir.join(".zshenv"), ZSH_ENV)?;
    write_atomic(&zsh_dir.join(".zprofile"), ZSH_PROFILE)?;
    write_atomic(&zsh_dir.join(".zshrc"), ZSH_RC)?;

    Ok(ShellIntegration { bash_rc, zsh_dir })
}

pub fn shell_command(shell: &Path, integration: &ShellIntegration, flatpak: bool) -> ShellCommand {
    let name = shell
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("sh")
        .to_string();

    let mut env = vec![
        ("TERM".into(), "xterm-256color".into()),
        ("COLORTERM".into(), "truecolor".into()),
        ("TERM_PROGRAM".into(), "Termust".into()),
        (
            "TERM_PROGRAM_VERSION".into(),
            env!("CARGO_PKG_VERSION").into(),
        ),
    ];

    let mut inner: Vec<OsString> = Vec::new();
    if name == "zsh" {
        inner.push(shell.as_os_str().to_owned());
        inner.push("-i".into());
        if let Some(home) = std::env::var_os("HOME") {
            env.push(("TERMUST_ORIGINAL_ZDOTDIR".into(), home));
        }
        env.push(("ZDOTDIR".into(), integration.zsh_dir.as_os_str().to_owned()));
    } else if name == "bash" {
        inner.push(shell.as_os_str().to_owned());
        inner.push("--rcfile".into());
        inner.push(integration.bash_rc.as_os_str().to_owned());
        inner.push("-i".into());
    } else {
        inner.push(shell.as_os_str().to_owned());
        inner.push("-i".into());
    }

    if !flatpak {
        return ShellCommand { argv: inner, env };
    }

    let mut argv: Vec<OsString> = vec!["flatpak-spawn".into(), "--host".into()];
    for (k, v) in &env {
        let mut pair = OsString::from("--env=");
        pair.push(k);
        pair.push("=");
        pair.push(v);
        argv.push(pair);
    }
    argv.push("--".into());
    argv.extend(inner);
    ShellCommand {
        argv,
        env: Vec::new(),
    }
}

fn write_atomic(path: &Path, content: &str) -> Result<(), HostError> {
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, content).map_err(|e| HostError::Io(e.to_string()))?;
    fs::set_permissions(&tmp, fs::Permissions::from_mode(0o644))
        .map_err(|e| HostError::Io(e.to_string()))?;
    fs::rename(&tmp, path).map_err(|e| HostError::Io(e.to_string()))?;
    Ok(())
}

fn content_hash() -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for part in ["v1", BASH_RC, ZSH_ENV, ZSH_PROFILE, ZSH_RC] {
        for b in part.bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
        h ^= 0xff;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn templates_contain_osc7_and_osc133() {
        assert!(BASH_RC.contains("]7;"));
        assert!(BASH_RC.contains("]133;"));
        assert!(ZSH_RC.contains("]7;"));
        assert!(ZSH_RC.contains("]133;"));
        assert!(ZSH_ENV.contains("TERMUST_ORIGINAL_ZDOTDIR"));
        assert!(ZSH_RC.contains("export ZDOTDIR"));
    }

    #[test]
    fn materialize_writes_private_files() {
        let root = std::env::temp_dir().join(format!("termust-si-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let si = materialize_in(&root).unwrap();
        assert!(si.bash_rc.exists());
        assert!(si.zsh_dir.join(".zshenv").exists());
        let mode = fs::metadata(&si.bash_rc).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o644);
        let dir_mode = fs::metadata(si.bash_rc.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(dir_mode, 0o700);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn direct_bash_argv_has_rcfile() {
        let si = ShellIntegration {
            bash_rc: PathBuf::from("/tmp/rc"),
            zsh_dir: PathBuf::from("/tmp/zsh"),
        };
        let cmd = shell_command(Path::new("/usr/bin/bash"), &si, false);
        assert_eq!(cmd.argv[0], OsString::from("/usr/bin/bash"));
        assert!(cmd.argv.iter().any(|a| a == "--rcfile"));
        assert!(cmd
            .env
            .iter()
            .any(|(k, v)| k == "TERM_PROGRAM" && v == "Termust"));
    }

    #[test]
    fn flatpak_zsh_argv_uses_zdotdir() {
        let si = ShellIntegration {
            bash_rc: PathBuf::from("/tmp/rc"),
            zsh_dir: PathBuf::from("/tmp/zsh"),
        };
        let cmd = shell_command(Path::new("/usr/bin/zsh"), &si, true);
        assert_eq!(cmd.argv[0], OsString::from("flatpak-spawn"));
        assert!(cmd.argv.iter().any(|a| a == "--host"));
        assert!(cmd
            .argv
            .iter()
            .any(|a| a.to_string_lossy().starts_with("--env=ZDOTDIR=/tmp/zsh")));
        assert!(!cmd.argv.iter().any(|a| a == "--env"));
        assert!(cmd.env.is_empty());
    }

    #[test]
    fn unknown_shell_runs_without_integration() {
        let si = ShellIntegration {
            bash_rc: PathBuf::from("/tmp/rc"),
            zsh_dir: PathBuf::from("/tmp/zsh"),
        };
        let cmd = shell_command(Path::new("/usr/bin/fish"), &si, false);
        assert_eq!(
            cmd.argv,
            vec![OsString::from("/usr/bin/fish"), OsString::from("-i")]
        );
    }
}
