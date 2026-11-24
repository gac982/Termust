//! Remotes y push de git: validación de transportes y construcción de argv.

use std::ffi::OsString;
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transport {
    Ssh,
    Https,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteSpec {
    pub transport: Transport,
    pub url: String,
    /// Origen canónico sin usuario ni path: `https://host[:port]` o
    /// `ssh://[user@]host[:port]`.
    pub origin: String,
    pub username: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Remote {
    pub name: String,
    pub fetch_url: String,
    pub push_url: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PushRequest {
    pub remote: String,
    pub refspec: String,
    pub set_upstream: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PushFailure {
    NonFastForward,
    Authentication,
    SshAuth,
    HostKey,
    NoUpstream,
    Rejected,
    Other(String),
}

impl PushFailure {
    pub fn message(&self) -> String {
        match self {
            PushFailure::NonFastForward => {
                termust_i18n::tr("The remote has new commits (non-fast-forward). Fetch/merge and try again; the push is not forced.")
            }
            PushFailure::Authentication => {
                termust_i18n::tr("HTTPS authentication rejected. Check the username and vault token.")
            }
            PushFailure::SshAuth => {
                termust_i18n::tr("SSH rejected authentication. Make sure the agent has the key loaded (ssh-add -l).")
            }
            PushFailure::HostKey => {
                termust_i18n::tr("The SSH host key is invalid or changed. Check ~/.ssh/known_hosts on the host.")
            }
            PushFailure::NoUpstream => {
                termust_i18n::tr("The branch has no upstream. Use “set upstream” or configure the remote.")
            }
            PushFailure::Rejected => termust_i18n::tr("The remote rejected the push."),
            PushFailure::Other(message) => message.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteError(String);

impl std::fmt::Display for RemoteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for RemoteError {}

impl From<String> for RemoteError {
    fn from(value: String) -> Self {
        RemoteError(value)
    }
}

impl From<&str> for RemoteError {
    fn from(value: &str) -> Self {
        RemoteError(value.to_string())
    }
}

fn error(message: &str) -> RemoteError {
    RemoteError(message.to_string())
}

pub fn parse_remote_url(input: &str) -> Result<RemoteSpec, RemoteError> {
    if input != input.trim() {
        return Err(error(&termust_i18n::tr(
            "the URL has leading or trailing spaces",
        )));
    }
    if input.is_empty()
        || input
            .chars()
            .any(|ch| ch.is_control() || ch.is_whitespace())
    {
        return Err(error(&termust_i18n::tr("invalid URL")));
    }
    let lower = input.to_ascii_lowercase();
    for prefix in ["ext::", "file://", "http://", "git://", "ftp://", "ssh+"] {
        if lower.starts_with(prefix) {
            return Err(error(&termust_i18n::tr("transport not allowed")));
        }
    }
    if lower.starts_with("https://") {
        return parse_https(input);
    }
    if lower.starts_with("ssh://") {
        return parse_ssh_url(input);
    }
    if lower.contains("://") {
        return Err(error(&termust_i18n::tr("transport not allowed")));
    }
    parse_scp_like(input)
}

fn parse_https(input: &str) -> Result<RemoteSpec, RemoteError> {
    let rest = &input["https://".len()..];
    let (authority, path) = rest
        .split_once('/')
        .ok_or_else(|| error(&termust_i18n::tr("missing repository path")))?;
    if path.is_empty() || path.contains(['?', '#']) || path.split('/').any(|part| part == "..") {
        return Err(error(&termust_i18n::tr("invalid repository path")));
    }
    if path.split('/').any(|part| part.starts_with('-')) {
        return Err(error(&termust_i18n::tr("invalid repository path")));
    }
    let (username, host) = match authority.rsplit_once('@') {
        Some((userinfo, host)) => {
            if userinfo.contains(':') {
                return Err(error(&termust_i18n::tr(
                    "passwords are not allowed in the URL; use the vault",
                )));
            }
            if userinfo.is_empty() {
                return Err(error(&termust_i18n::tr("empty user in the URL")));
            }
            (Some(userinfo.to_string()), host)
        }
        None => (None, authority),
    };
    let host = validate_host(host)?;
    Ok(RemoteSpec {
        transport: Transport::Https,
        url: format!("https://{host}/{path}"),
        origin: format!("https://{host}"),
        username,
    })
}

fn parse_ssh_url(input: &str) -> Result<RemoteSpec, RemoteError> {
    let rest = &input["ssh://".len()..];
    let (authority, _path) = match rest.split_once('/') {
        Some((authority, path)) => (authority, Some(path)),
        None => (rest, None),
    };
    if authority.is_empty() {
        return Err(error(&termust_i18n::tr("empty SSH host")));
    }
    let (userinfo, host) = match authority.rsplit_once('@') {
        Some((user, host)) => {
            if user.is_empty() {
                return Err(error(&termust_i18n::tr("empty SSH user")));
            }
            (Some(user), host)
        }
        None => (None, authority),
    };
    let host = validate_host(host)?;
    let origin = match userinfo {
        Some(user) => format!("ssh://{user}@{host}"),
        None => format!("ssh://{host}"),
    };
    Ok(RemoteSpec {
        transport: Transport::Ssh,
        url: input.to_string(),
        origin,
        username: userinfo.map(str::to_string),
    })
}

fn parse_scp_like(input: &str) -> Result<RemoteSpec, RemoteError> {
    if input.starts_with('-') {
        return Err(error(&termust_i18n::tr("the URL cannot start with “-”")));
    }
    let (userhost, path) = input
        .split_once(':')
        .ok_or_else(|| error(&termust_i18n::tr("invalid SSH URL")))?;
    if path.is_empty() || userhost.contains('/') {
        return Err(error(&termust_i18n::tr("invalid SSH URL")));
    }
    let (username, host) = userhost.rsplit_once('@').ok_or_else(|| {
        error(&termust_i18n::tr(
            "SSH scp-like URL requires user@host:path",
        ))
    })?;
    if username.is_empty() {
        return Err(error(&termust_i18n::tr("empty SSH user")));
    }
    let host = validate_host(host)?;
    Ok(RemoteSpec {
        transport: Transport::Ssh,
        url: input.to_string(),
        origin: format!("ssh://{username}@{host}"),
        username: Some(username.to_string()),
    })
}

fn validate_host(host: &str) -> Result<String, RemoteError> {
    if host.is_empty() || host.contains('/') || host.contains('@') {
        return Err(error(&termust_i18n::tr("invalid host")));
    }
    let (name, port) = match host.rsplit_once(':') {
        Some((name, port)) => {
            if port.is_empty() || !port.chars().all(|ch| ch.is_ascii_digit()) {
                return Err(error(&termust_i18n::tr("invalid port")));
            }
            (name, Some(port))
        }
        None => (host, None),
    };
    if name.is_empty()
        || !name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '.' || ch == '-')
    {
        return Err(error(&termust_i18n::tr("invalid host")));
    }
    Ok(match port {
        Some(port) => format!("{}:{port}", name.to_ascii_lowercase()),
        None => name.to_ascii_lowercase(),
    })
}

pub fn validate_remote_name(name: &str) -> Result<(), RemoteError> {
    if name.is_empty() || name.len() > 64 || name.starts_with('-') || name == "." || name == ".." {
        return Err(error(&termust_i18n::tr("invalid remote name")));
    }
    if !name
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-'))
    {
        return Err(error(&termust_i18n::tr("invalid remote name")));
    }
    Ok(())
}

pub fn validate_refspec(refspec: &str) -> Result<(), RemoteError> {
    if refspec.is_empty()
        || refspec.starts_with(['-', '+'])
        || refspec
            .chars()
            .any(|ch| ch.is_control() || ch.is_whitespace())
        || refspec.contains("..")
        || refspec == ":"
        || refspec
            .chars()
            .any(|ch| matches!(ch, '~' | '^' | '?' | '*' | '[' | '\\'))
    {
        return Err(error(&termust_i18n::tr(
            "invalid refspec (force is not allowed)",
        )));
    }
    Ok(())
}

pub fn push_args(request: &PushRequest, transport: Transport) -> Result<Vec<String>, RemoteError> {
    validate_remote_name(&request.remote)?;
    validate_refspec(&request.refspec)?;
    let mut args = Vec::new();
    if transport == Transport::Https {
        args.push("-c".to_string());
        args.push("credential.helper=".to_string());
    }
    args.push("push".to_string());
    args.push("--porcelain".to_string());
    if request.set_upstream {
        args.push("--set-upstream".to_string());
    }
    args.push(request.remote.clone());
    args.push(request.refspec.clone());
    Ok(args)
}

pub fn push_env(
    transport: Transport,
    askpass: Option<&Path>,
    user_fifo: Option<&Path>,
    pass_fifo: Option<&Path>,
) -> Vec<(OsString, OsString)> {
    let mut env = vec![
        (OsString::from("GIT_TERMINAL_PROMPT"), OsString::from("0")),
        (OsString::from("LC_ALL"), OsString::from("C")),
    ];
    match transport {
        Transport::Ssh => {
            // Un GIT_SSH_COMMAND explícito se propaga al host; por defecto ssh.
            let ssh = std::env::var_os("GIT_SSH_COMMAND").unwrap_or_else(|| OsString::from("ssh"));
            env.push((OsString::from("GIT_SSH_COMMAND"), ssh));
        }
        Transport::Https => {
            if let Some(askpass) = askpass {
                env.push((
                    OsString::from("GIT_ASKPASS"),
                    askpass.as_os_str().to_os_string(),
                ));
            }
            if let Some(fifo) = user_fifo {
                env.push((
                    OsString::from("TERMUST_ASKPASS_USER_FIFO"),
                    fifo.as_os_str().to_os_string(),
                ));
            }
            if let Some(fifo) = pass_fifo {
                env.push((
                    OsString::from("TERMUST_ASKPASS_PASS_FIFO"),
                    fifo.as_os_str().to_os_string(),
                ));
            }
        }
    }
    env
}

pub fn map_push_error(stderr: &str) -> PushFailure {
    let lower = stderr.to_ascii_lowercase();
    if lower.contains("non-fast-forward") || lower.contains("fetch first") {
        PushFailure::NonFastForward
    } else if lower.contains("authentication failed")
        || lower.contains("could not read username")
        || lower.contains("invalid username or password")
    {
        PushFailure::Authentication
    } else if lower.contains("permission denied (publickey)") || lower.contains("no such identity")
    {
        PushFailure::SshAuth
    } else if lower.contains("host key verification failed") {
        PushFailure::HostKey
    } else if lower.contains("has no upstream branch") || lower.contains("no upstream configured") {
        PushFailure::NoUpstream
    } else if lower.contains("rejected") {
        PushFailure::Rejected
    } else {
        PushFailure::Other(stderr.trim().to_string())
    }
}

pub fn remote_add_args(name: &str, url: &str) -> Result<Vec<String>, RemoteError> {
    validate_remote_name(name)?;
    parse_remote_url(url)?;
    Ok(vec![
        "remote".to_string(),
        "add".to_string(),
        name.to_string(),
        url.to_string(),
    ])
}

pub fn remote_set_url_args(name: &str, url: &str) -> Result<Vec<String>, RemoteError> {
    validate_remote_name(name)?;
    parse_remote_url(url)?;
    Ok(vec![
        "remote".to_string(),
        "set-url".to_string(),
        name.to_string(),
        url.to_string(),
    ])
}

pub fn remote_remove_args(name: &str) -> Result<Vec<String>, RemoteError> {
    validate_remote_name(name)?;
    Ok(vec![
        "remote".to_string(),
        "remove".to_string(),
        name.to_string(),
    ])
}

pub fn parse_remotes(stdout: &str) -> Vec<Remote> {
    let mut remotes: Vec<Remote> = Vec::new();
    for line in stdout.lines() {
        let Some((name, rest)) = line.split_once('\t') else {
            continue;
        };
        let (url, kind) = if let Some(url) = rest.strip_suffix(" (fetch)") {
            (url, "fetch")
        } else if let Some(url) = rest.strip_suffix(" (push)") {
            (url, "push")
        } else {
            continue;
        };
        let index = match remotes.iter().position(|remote| remote.name == name) {
            Some(index) => index,
            None => {
                remotes.push(Remote {
                    name: name.to_string(),
                    fetch_url: String::new(),
                    push_url: String::new(),
                });
                remotes.len() - 1
            }
        };
        let remote = &mut remotes[index];
        if kind == "fetch" {
            remote.fetch_url = url.to_string();
        } else {
            remote.push_url = url.to_string();
        }
    }
    remotes
}

/// Runner inyectable: la app lo implementa sobre el `git` del host.
pub trait RemoteRunner: Send + Sync {
    fn run(
        &self,
        cwd: &Path,
        args: &[OsString],
        env: &[(OsString, OsString)],
    ) -> Result<std::process::Output, RemoteError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::path::Path;

    #[test]
    fn git_remote_parse_transports() {
        let https = parse_remote_url("https://github.com/user/repo.git").unwrap();
        assert_eq!(https.transport, Transport::Https);
        assert_eq!(https.origin, "https://github.com");
        assert_eq!(https.url, "https://github.com/user/repo.git");
        assert!(https.username.is_none());

        let with_user = parse_remote_url("https://alice@gitlab.com/group/repo").unwrap();
        assert_eq!(with_user.origin, "https://gitlab.com");
        assert_eq!(with_user.url, "https://gitlab.com/group/repo");
        assert_eq!(with_user.username.as_deref(), Some("alice"));

        let ssh_scp = parse_remote_url("git@github.com:user/repo.git").unwrap();
        assert_eq!(ssh_scp.transport, Transport::Ssh);
        assert_eq!(ssh_scp.origin, "ssh://git@github.com");

        let ssh_url = parse_remote_url("ssh://git@example.com:2222/srv/repo.git").unwrap();
        assert_eq!(ssh_url.transport, Transport::Ssh);
        assert_eq!(ssh_url.origin, "ssh://git@example.com:2222");
    }

    #[test]
    fn git_remote_rejects_ext_and_file() {
        for url in [
            "ext::sh -c whoami",
            "file:///tmp/repo",
            "http://example.com/repo",
            "git://example.com/repo",
            "ftp://example.com/repo",
            "-oProxyCommand=evil",
            "https://",
            "https://example.com/repo\n",
        ] {
            assert!(parse_remote_url(url).is_err(), "debía rechazar {url}");
        }
        assert!(parse_remote_url("https://bob:token@example.com/repo").is_err());
    }

    #[test]
    fn git_remote_name_and_refspec_validated() {
        assert!(validate_remote_name("origin").is_ok());
        assert!(validate_remote_name("up-stream_1").is_ok());
        for name in ["", ".", "..", "-x", "a/b", "a b", "a\n"] {
            assert!(validate_remote_name(name).is_err(), "nombre {name}");
        }
        assert!(validate_refspec("main").is_ok());
        assert!(validate_refspec("main:main").is_ok());
        for spec in [
            "",
            "+main",
            "--force",
            "main..other",
            "a b",
            "a\n",
            ":",
            "-x",
        ] {
            assert!(validate_refspec(spec).is_err(), "refspec {spec}");
        }
    }

    #[test]
    fn git_push_never_force() {
        let request = PushRequest {
            remote: "origin".into(),
            refspec: "main".into(),
            set_upstream: true,
        };
        let args = push_args(&request, Transport::Ssh).unwrap();
        assert!(!args
            .iter()
            .any(|arg| arg.contains("--force") || arg.starts_with('+')));
        assert!(args.contains(&"--set-upstream".to_string()));
        assert_eq!(args.last().map(String::as_str), Some("main"));
        let forced = PushRequest {
            remote: "origin".into(),
            refspec: "+main:main".into(),
            set_upstream: false,
        };
        assert!(push_args(&forced, Transport::Ssh).is_err());
    }

    #[test]
    fn git_https_secret_not_in_argv_or_logs() {
        let request = PushRequest {
            remote: "origin".into(),
            refspec: "main".into(),
            set_upstream: false,
        };
        let askpass = Path::new("/home/u/.local/state/termust/git/askpass.sh");
        let user_fifo = Path::new("/home/u/.local/state/termust/git/user.fifo");
        let pass_fifo = Path::new("/home/u/.local/state/termust/git/pass.fifo");
        let args = push_args(&request, Transport::Https).unwrap();
        let env = push_env(
            Transport::Https,
            Some(askpass),
            Some(user_fifo),
            Some(pass_fifo),
        );
        let secret = "TOKEN-SUPER-SECRETO";
        assert!(!args.iter().any(|arg| arg.contains(secret)));
        assert!(!env.iter().any(|(key, value)| {
            key.to_string_lossy().contains(secret) || value.to_string_lossy().contains(secret)
        }));
        let joined = format!("{args:?}");
        assert!(!joined.contains(secret));
        assert!(env.contains(&(
            OsString::from("GIT_ASKPASS"),
            askpass.as_os_str().to_os_string()
        )));
        assert!(env.contains(&(OsString::from("GIT_TERMINAL_PROMPT"), OsString::from("0"))));
    }

    #[test]
    fn git_ssh_never_reads_dot_ssh() {
        let env = push_env(Transport::Ssh, None, None, None);
        assert!(env.contains(&(OsString::from("GIT_SSH_COMMAND"), OsString::from("ssh"))));
        assert!(!env.iter().any(|(key, value)| {
            key.to_string_lossy().contains(".ssh") || value.to_string_lossy().contains(".ssh")
        }));
        let source = include_str!("../src/lib.rs");
        let production = source.split("#[cfg(test)]").next().unwrap_or("");
        assert!(!production.contains("id_rsa"));
        assert!(!production.contains("id_ed25519"));
    }

    #[test]
    fn git_push_error_mapping() {
        assert_eq!(
            map_push_error("! [rejected] main -> main (non-fast-forward)"),
            PushFailure::NonFastForward
        );
        assert_eq!(
            map_push_error("fatal: Authentication failed for 'https://example.com/'"),
            PushFailure::Authentication
        );
        assert_eq!(
            map_push_error("git@host: Permission denied (publickey)."),
            PushFailure::SshAuth
        );
        assert_eq!(
            map_push_error("Host key verification failed."),
            PushFailure::HostKey
        );
        assert!(matches!(
            map_push_error("remote: something weird"),
            PushFailure::Other(_)
        ));
        assert!(PushFailure::NonFastForward
            .message()
            .contains("fast-forward"));
    }

    #[test]
    fn git_remote_crud_args() {
        assert_eq!(
            remote_add_args("origin", "https://example.com/r.git").unwrap(),
            vec!["remote", "add", "origin", "https://example.com/r.git"]
        );
        assert_eq!(
            remote_remove_args("origin").unwrap(),
            vec!["remote", "remove", "origin"]
        );
        assert!(remote_add_args("-x", "https://example.com/r").is_err());
        let parsed = parse_remotes(
            "origin\thttps://example.com/r.git (fetch)\norigin\thttps://example.com/r.git (push)\n",
        );
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].name, "origin");
        assert_eq!(parsed[0].fetch_url, "https://example.com/r.git");
        assert_eq!(parsed[0].push_url, "https://example.com/r.git");
    }
}
