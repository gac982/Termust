use crate::tabs::Tab;
use std::path::Path;

pub const MAX_TITLE_CHARS: usize = 256;
pub const DISPLAY_TITLE_CHARS: usize = 48;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TitleSpec {
    /// Texto corto para la pestaña/barra.
    pub text: String,
    /// Título completo saneado (sin recortar).
    pub full: String,
    /// Título completo + cwd + comando para el tooltip.
    pub tooltip: String,
}

/// Prioridad: override manual > OSC 0/2 > proceso en foreground > basename(cwd).
pub fn display_title(tab: &Tab, foreground: Option<&str>) -> TitleSpec {
    let candidate = tab
        .title_override
        .clone()
        .or_else(|| tab.last_osc_title.clone())
        .or_else(|| foreground.map(str::to_string))
        .unwrap_or_else(|| basename(&tab.cwd));

    let mut full = sanitize(shorten_shell_title(&candidate));
    if full.is_empty() {
        full = basename(&tab.cwd);
    }
    if full.chars().count() > MAX_TITLE_CHARS {
        full = full.chars().take(MAX_TITLE_CHARS).collect();
    }

    let text = truncate_middle(&full, DISPLAY_TITLE_CHARS);
    let tooltip = format!(
        "{full}\n{}\n{}",
        tab.cwd.display(),
        foreground.unwrap_or("")
    )
    .trim_end()
    .to_string();

    TitleSpec {
        text,
        full,
        tooltip,
    }
}

/// Texto de la pestaña: nombre manual, o el programa en ejecución cuando hay
/// uno (btop, vim, etc.), o el directorio actual. Nunca la ruta completa ni el
/// título OSC del shell.
pub fn tab_label(tab: &Tab, foreground: Option<&str>) -> String {
    let mut full = tab
        .title_override
        .as_deref()
        .map(sanitize)
        .unwrap_or_default();
    if full.is_empty() {
        if let Some(command) = foreground.and_then(command_name) {
            full = sanitize(command);
        }
    }
    if full.is_empty() {
        full = sanitize(&basename(&tab.cwd));
    }
    if full.is_empty() {
        full = "Terminal".to_string();
    }
    truncate_middle(&full, DISPLAY_TITLE_CHARS)
}

/// `argv` del proceso en foreground → nombre del ejecutable.
fn command_name(argv: &str) -> Option<&str> {
    let first = argv.split_whitespace().next()?;
    let name = first.rsplit('/').next().unwrap_or(first);
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

/// p10k/oh-my-zsh suelen poner `user@host:~/ruta` como título; la ruta sola
/// es más útil y ocupa menos en la pestaña.
fn shorten_shell_title(input: &str) -> &str {
    if let Some((head, tail)) = input.split_once(':') {
        if head.contains('@') && !tail.is_empty() {
            return tail.trim_start();
        }
    }
    input
}

pub fn sanitize(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut last_space = false;
    for ch in input.chars() {
        let code = ch as u32;
        let bidi = (0x202A..=0x202E).contains(&code)
            || (0x2066..=0x2069).contains(&code)
            || code == 0x200E
            || code == 0x200F;
        if ch.is_control() || bidi {
            continue;
        }
        if ch.is_whitespace() {
            if !last_space && !out.is_empty() {
                out.push(' ');
                last_space = true;
            }
        } else {
            out.push(ch);
            last_space = false;
        }
    }
    out.trim_end().to_string()
}

pub fn truncate_middle(input: &str, max_chars: usize) -> String {
    let chars: Vec<char> = input.chars().collect();
    if chars.len() <= max_chars || max_chars == 0 {
        return input.to_string();
    }
    if max_chars == 1 {
        return "…".to_string();
    }
    let keep = max_chars - 1;
    let left = keep.div_ceil(2);
    let right = keep / 2;
    let head: String = chars[..left].iter().collect();
    let tail: String = chars[chars.len() - right..].iter().collect();
    format!("{head}…{tail}")
}

fn basename(path: &Path) -> String {
    path.file_name()
        .and_then(|s| s.to_str())
        .map(str::to_string)
        .unwrap_or_else(|| path.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tabs::TabModel;
    use std::path::PathBuf;

    fn tab(cwd: &str) -> Tab {
        TabModel::new(cwd).active().clone()
    }

    #[test]
    fn priority_override_wins() {
        let mut m = TabModel::new("/repo");
        let id = m.active_id();
        m.rename(id, Some("manual".into()));
        m.set_osc_title(id, "osc");
        let spec = display_title(m.active(), Some("cargo"));
        assert_eq!(spec.full, "manual");
    }

    #[test]
    fn osc_wins_over_process() {
        let mut m = TabModel::new("/repo");
        let id = m.active_id();
        m.set_osc_title(id, "cargo test");
        assert_eq!(display_title(m.active(), Some("cargo")).full, "cargo test");
    }

    #[test]
    fn process_wins_over_cwd() {
        let t = tab("/repo");
        assert_eq!(display_title(&t, Some("vim")).full, "vim");
    }

    #[test]
    fn cwd_basename_fallback() {
        let t = tab("/home/u/repo");
        assert_eq!(display_title(&t, None).full, "repo");
    }

    #[test]
    fn tab_label_prefers_directory_over_osc() {
        let mut m = TabModel::new("/home/u/repo");
        let id = m.active_id();
        m.set_osc_title(id, "gustavo@host:~/repo");
        assert_eq!(tab_label(m.active(), None), "repo");
    }

    #[test]
    fn tab_label_shows_running_program() {
        let m = TabModel::new("/home/u/repo");
        assert_eq!(tab_label(m.active(), Some("/usr/bin/btop")), "btop");
        assert_eq!(tab_label(m.active(), Some("nvim lib.rs")), "nvim");
        assert_eq!(tab_label(m.active(), Some("")), "repo");
        assert_eq!(tab_label(m.active(), None), "repo");
    }

    #[test]
    fn tab_label_override_wins_over_program() {
        let mut m = TabModel::new("/home/u/repo");
        let id = m.active_id();
        m.rename(id, Some("manual".into()));
        assert_eq!(tab_label(m.active(), Some("btop")), "manual");
    }

    #[test]
    fn tab_label_override_wins() {
        let mut m = TabModel::new("/home/u/repo");
        let id = m.active_id();
        m.rename(id, Some("manual".into()));
        assert_eq!(tab_label(m.active(), None), "manual");
    }

    #[test]
    fn tab_label_empty_override_falls_back_to_cwd() {
        let mut m = TabModel::new("/home/u/repo");
        let id = m.active_id();
        m.rename(id, Some("   ".into()));
        assert_eq!(tab_label(m.active(), None), "repo");
    }

    #[test]
    fn controls_and_bidi_removed() {
        assert_eq!(sanitize("a\u{1b}\u{202e}b\n c"), "ab c");
        assert_eq!(sanitize("   x   "), "x");
    }

    #[test]
    fn long_title_truncated_in_the_middle() {
        let long = "a".repeat(100);
        let out = truncate_middle(&long, 20);
        assert_eq!(out.chars().count(), 20);
        assert!(out.contains('…'));
        assert!(out.starts_with("aaaaaaaaa"));
        assert!(out.ends_with("aaaaaaaaa"));
    }

    #[test]
    fn shell_prefix_is_shortened() {
        assert_eq!(shorten_shell_title("gustavo@host:~/repo"), "~/repo");
        assert_eq!(shorten_shell_title("cargo test"), "cargo test");
        assert_eq!(shorten_shell_title("a@b:"), "a@b:");
    }

    #[test]
    fn tooltip_has_cwd_and_command() {
        let t = tab("/home/u/repo");
        let spec = display_title(&t, Some("htop"));
        assert!(spec.tooltip.contains("/home/u/repo"));
        assert!(spec.tooltip.contains("htop"));
    }

    #[test]
    fn empty_override_falls_back_to_cwd() {
        let mut m = TabModel::new("/repo");
        let id = m.active_id();
        m.rename(id, Some("   ".into()));
        assert_eq!(display_title(m.active(), None).full, "repo");
    }

    #[test]
    fn path_root_uses_full_path() {
        let t = tab("/");
        assert_eq!(
            display_title(&t, None).full,
            PathBuf::from("/").display().to_string()
        );
    }
}
