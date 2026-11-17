use std::ffi::OsString;
use std::path::PathBuf;

/// Resuelve el shell a ejecutar.
///
/// Orden: shell configurado > `$SHELL` > entrada de passwd > `/bin/sh`.
/// Solo se aceptan rutas absolutas no vacías; cualquier otra se ignora.
pub fn resolve_shell(
    configured: Option<&PathBuf>,
    env_shell: Option<&OsString>,
    passwd_shell: Option<&PathBuf>,
) -> PathBuf {
    let env_path = env_shell.map(PathBuf::from);
    for candidate in [configured, env_path.as_ref(), passwd_shell]
        .into_iter()
        .flatten()
    {
        if candidate.is_absolute() {
            return candidate.clone();
        }
    }
    PathBuf::from("/bin/sh")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    #[test]
    fn configured_wins() {
        let c = p("/usr/bin/zsh");
        let e = OsString::from("/usr/bin/bash");
        assert_eq!(resolve_shell(Some(&c), Some(&e), Some(&p("/bin/sh"))), c);
    }

    #[test]
    fn env_beats_passwd() {
        let e = OsString::from("/usr/bin/fish");
        assert_eq!(
            resolve_shell(None, Some(&e), Some(&p("/bin/bash"))),
            p("/usr/bin/fish")
        );
    }

    #[test]
    fn relative_candidates_are_ignored() {
        let e = OsString::from("zsh");
        let sh = p("/bin/bash");
        assert_eq!(resolve_shell(None, Some(&e), Some(&sh)), sh);
    }

    #[test]
    fn fallback_is_sh() {
        assert_eq!(resolve_shell(None, None, None), p("/bin/sh"));
    }
}
