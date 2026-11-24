//! Runner de git que ejecuta el binario del host (Flatpak) o el local.

use std::path::Path;
use termust_git_local::{GitError, GitRunner};

use crate::i18n;

pub(crate) struct HostGitRunner;

impl GitRunner for HostGitRunner {
    fn run(
        &self,
        cwd: &Path,
        args: &[std::ffi::OsString],
    ) -> Result<std::process::Output, GitError> {
        termust_host_exec::host_command("git", args, Some(cwd))
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .map_err(|error| {
                GitError::Io(i18n::tr_args(
                    "could not run git: {error}",
                    &[("error", &error.to_string())],
                ))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::path::Path;
    use termust_git_local::GitRunner;

    #[test]
    fn host_runner_runs_git() {
        let runner = HostGitRunner;
        let args = [OsString::from("--version")];
        let output = runner.run(Path::new("."), &args).expect("git disponible");
        assert!(output.status.success());
    }
}
