//! Detección de agentes por reglas TOML, estado y título de tarea.
#![forbid(unsafe_code)]

use regex::Regex;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::time::Duration;

const OPENCODE: &str = include_str!("../../../data/agents/opencode.toml");
const PI: &str = include_str!("../../../data/agents/pi.toml");
const AGY: &str = include_str!("../../../data/agents/agy.toml");

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentRule {
    pub id: String,
    pub display_name: String,
    pub icon: String,
    pub match_argv_regex: String,
    pub match_title_regex: Option<String>,
    pub title_sources: Vec<String>,
    pub output_task_regex: Option<String>,
    pub waiting_input_regex: Option<String>,
    pub active_window_ms: u64,
}

#[derive(Deserialize)]
struct RuleFile {
    schema_version: u32,
    id: String,
    display_name: String,
    #[serde(default = "default_icon")]
    icon: String,
    match_argv_regex: String,
    #[serde(default)]
    match_title_regex: Option<String>,
    #[serde(default)]
    title_sources: Vec<String>,
    #[serde(default)]
    output_task_regex: Option<String>,
    #[serde(default)]
    waiting_input_regex: Option<String>,
    #[serde(default = "default_active_window")]
    active_window_ms: u64,
}

fn default_icon() -> String {
    "avatar-default-symbolic".into()
}

fn default_active_window() -> u64 {
    3000
}

pub struct CompiledRule {
    pub rule: AgentRule,
    argv: Regex,
    title: Option<Regex>,
    task: Option<Regex>,
    waiting: Option<Regex>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentState {
    Active,
    Idle,
    WaitingInput,
    Exited { code: Option<i32> },
}

impl AgentState {
    pub fn label(&self) -> &'static str {
        match self {
            AgentState::Active => "Activo",
            AgentState::Idle => "Inactivo",
            AgentState::WaitingInput => "Esperando entrada",
            AgentState::Exited { .. } => "Terminado",
        }
    }

    pub fn css_class(&self) -> &'static str {
        match self {
            AgentState::Active => "termust-agent-active",
            AgentState::Idle => "termust-agent-idle",
            AgentState::WaitingInput => "termust-agent-waiting",
            AgentState::Exited { .. } => "termust-agent-exited",
        }
    }

    pub fn is_green(&self) -> bool {
        matches!(self, AgentState::Active)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentStatus {
    pub id: String,
    pub display_name: String,
    pub icon: String,
    pub task: String,
    pub state: AgentState,
}

pub struct AgentCatalog {
    rules: Vec<CompiledRule>,
    pub errors: Vec<String>,
}

pub struct AgentInput<'a> {
    pub title: Option<&'a str>,
    pub recent_lines: &'a [String],
    pub alive: bool,
    pub exit_code: Option<i32>,
    pub activity_age: Option<Duration>,
    pub cwd: &'a Path,
}

impl AgentCatalog {
    pub fn load() -> Self {
        let mut sources = vec![("<opencode>", OPENCODE), ("<pi>", PI), ("<agy>", AGY)];
        let mut owned: Vec<(String, String)> = Vec::new();
        if let Some(home) = std::env::var_os("HOME") {
            let dir = PathBuf::from(home).join(".config/termust/agents");
            if let Ok(entries) = std::fs::read_dir(&dir) {
                let mut files: Vec<_> = entries.flatten().map(|entry| entry.path()).collect();
                files.sort();
                for path in files {
                    if path.extension().and_then(|ext| ext.to_str()) != Some("toml") {
                        continue;
                    }
                    if let Ok(text) = std::fs::read_to_string(&path) {
                        owned.push((path.display().to_string(), text));
                    }
                }
            }
        }
        for (name, text) in &owned {
            sources.push((name.as_str(), text.as_str()));
        }
        Self::from_sources(&sources)
    }

    pub fn from_sources(sources: &[(&str, &str)]) -> Self {
        let mut rules = Vec::new();
        let mut errors = Vec::new();
        for (name, source) in sources {
            let parsed: RuleFile = match toml::from_str(source) {
                Ok(parsed) => parsed,
                Err(error) => {
                    errors.push(format!("{name}: {error}"));
                    continue;
                }
            };
            if parsed.schema_version != 1 {
                errors.push(format!("{name}: schema_version no soportado"));
                continue;
            }
            let argv = match Regex::new(&parsed.match_argv_regex) {
                Ok(regex) => regex,
                Err(error) => {
                    errors.push(format!("{name}: match_argv_regex: {error}"));
                    continue;
                }
            };
            let compile = |pattern: &Option<String>, field: &str| match pattern {
                Some(pattern) => match Regex::new(pattern) {
                    Ok(regex) => Ok(Some(regex)),
                    Err(error) => Err(format!("{name}: {field}: {error}")),
                },
                None => Ok(None),
            };
            let title = match compile(&parsed.match_title_regex, "match_title_regex") {
                Ok(regex) => regex,
                Err(error) => {
                    errors.push(error);
                    continue;
                }
            };
            let task = match compile(&parsed.output_task_regex, "output_task_regex") {
                Ok(regex) => regex,
                Err(error) => {
                    errors.push(error);
                    continue;
                }
            };
            let waiting = match compile(&parsed.waiting_input_regex, "waiting_input_regex") {
                Ok(regex) => regex,
                Err(error) => {
                    errors.push(error);
                    continue;
                }
            };
            rules.push(CompiledRule {
                rule: AgentRule {
                    id: parsed.id,
                    display_name: parsed.display_name,
                    icon: parsed.icon,
                    match_argv_regex: parsed.match_argv_regex,
                    match_title_regex: parsed.match_title_regex,
                    title_sources: parsed.title_sources,
                    output_task_regex: parsed.output_task_regex,
                    waiting_input_regex: parsed.waiting_input_regex,
                    active_window_ms: parsed.active_window_ms,
                },
                argv,
                title,
                task,
                waiting,
            });
        }
        Self { rules, errors }
    }

    pub fn rules(&self) -> impl Iterator<Item = &CompiledRule> {
        self.rules.iter()
    }

    pub fn detect(&self, argv: Option<&str>, title: Option<&str>) -> Option<&CompiledRule> {
        if let Some(argv) = argv {
            if let Some(rule) = self.rules.iter().find(|rule| rule.argv.is_match(argv)) {
                return Some(rule);
            }
        }
        if let Some(title) = title {
            if let Some(rule) = self.rules.iter().find(|rule| {
                rule.title
                    .as_ref()
                    .is_some_and(|regex| regex.is_match(title))
            }) {
                return Some(rule);
            }
        }
        None
    }

    pub fn evaluate(&self, rule: &CompiledRule, input: &AgentInput<'_>) -> AgentStatus {
        let task = self.task(rule, input);
        let state = if !input.alive {
            AgentState::Exited {
                code: input.exit_code,
            }
        } else if rule.waiting.as_ref().is_some_and(|regex| {
            input
                .recent_lines
                .iter()
                .rev()
                .take(5)
                .any(|line| regex.is_match(line))
                || input.title.is_some_and(|title| regex.is_match(title))
        }) {
            AgentState::WaitingInput
        } else if input
            .activity_age
            .map(|age| age <= Duration::from_millis(rule.rule.active_window_ms))
            .unwrap_or(false)
        {
            AgentState::Active
        } else {
            AgentState::Idle
        };
        AgentStatus {
            id: rule.rule.id.clone(),
            display_name: rule.rule.display_name.clone(),
            icon: rule.rule.icon.clone(),
            task,
            state,
        }
    }

    fn task(&self, rule: &CompiledRule, input: &AgentInput<'_>) -> String {
        for source in &rule.rule.title_sources {
            if source == "osc_title" {
                if let Some(title) = input.title {
                    let title = title.trim();
                    if !title.is_empty() && title != input.cwd.to_string_lossy() {
                        return title.to_string();
                    }
                }
            } else if source == "output_task_regex" {
                if let Some(regex) = &rule.task {
                    for line in input.recent_lines.iter().rev().take(50) {
                        if let Some(captures) = regex.captures(line) {
                            if let Some(capture) = captures.get(1) {
                                let task = capture.as_str().trim();
                                if !task.is_empty() {
                                    return task.to_string();
                                }
                            }
                        }
                    }
                }
            }
        }
        format!("{} · {}", rule.rule.display_name, input.cwd.display())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn catalog() -> AgentCatalog {
        AgentCatalog::from_sources(&[("<opencode>", OPENCODE), ("<pi>", PI), ("<agy>", AGY)])
    }

    fn input<'a>(cwd: &'a Path, lines: &'a [String]) -> AgentInput<'a> {
        AgentInput {
            title: None,
            recent_lines: lines,
            alive: true,
            exit_code: None,
            activity_age: Some(Duration::from_millis(100)),
            cwd,
        }
    }

    #[test]
    fn agent_detects_opencode_pi_agy() {
        let catalog = catalog();
        assert_eq!(
            catalog
                .detect(Some("/usr/bin/opencode"), None)
                .unwrap()
                .rule
                .id,
            "opencode"
        );
        assert_eq!(
            catalog
                .detect(Some("/home/u/.nvm/bin/pi"), None)
                .unwrap()
                .rule
                .id,
            "pi"
        );
        assert_eq!(
            catalog
                .detect(Some("/home/u/.local/bin/agy"), None)
                .unwrap()
                .rule
                .id,
            "antigravity"
        );
        assert!(catalog.detect(Some("antigravity"), None).is_none());
        assert_eq!(
            catalog
                .detect(None, Some("opencode — main"))
                .unwrap()
                .rule
                .id,
            "opencode"
        );
    }

    #[test]
    fn agent_dot_green_only_when_active_and_alive() {
        let catalog = catalog();
        let rule = catalog.detect(Some("opencode"), None).unwrap();
        let cwd = Path::new("/tmp/proyecto");
        let lines = vec![];
        let mut input = input(cwd, &lines);
        let status = catalog.evaluate(rule, &input);
        assert_eq!(status.state, AgentState::Active);
        assert!(status.state.is_green());
        assert!(!AgentState::Idle.is_green());
        input.alive = false;
        input.exit_code = Some(1);
        let status = catalog.evaluate(rule, &input);
        assert_eq!(status.state, AgentState::Exited { code: Some(1) });
        assert!(!status.state.is_green());
    }

    #[test]
    fn agent_silent_process_is_idle() {
        let catalog = catalog();
        let rule = catalog.detect(Some("opencode"), None).unwrap();
        let cwd = Path::new("/tmp/proyecto");
        let lines = vec![];
        let mut input = input(cwd, &lines);
        input.activity_age = Some(Duration::from_secs(10));
        let status = catalog.evaluate(rule, &input);
        assert_eq!(status.state, AgentState::Idle);
        assert!(!status.state.is_green());
        input.alive = false;
        let status = catalog.evaluate(rule, &input);
        assert!(matches!(status.state, AgentState::Exited { .. }));
    }

    #[test]
    fn agent_title_falls_back_to_cwd() {
        let catalog = catalog();
        let rule = catalog.detect(Some("opencode"), None).unwrap();
        let cwd = Path::new("/tmp/mi-proyecto");
        let lines = vec![];
        let status = catalog.evaluate(rule, &input(cwd, &lines));
        assert_eq!(status.task, "OpenCode · /tmp/mi-proyecto");
        let output = vec!["Task: arreglar el parser".to_string()];
        let status = catalog.evaluate(rule, &input(cwd, &output));
        assert_eq!(status.task, "arreglar el parser");
    }

    #[test]
    fn agent_waiting_input_state() {
        let catalog = catalog();
        let rule = catalog.detect(Some("opencode"), None).unwrap();
        let cwd = Path::new("/tmp");
        let lines = vec!["Waiting for your input".to_string()];
        let status = catalog.evaluate(rule, &input(cwd, &lines));
        assert_eq!(status.state, AgentState::WaitingInput);
        assert!(!status.state.is_green());
    }

    #[test]
    fn agent_invalid_rule_ignored() {
        let catalog = AgentCatalog::from_sources(&[
            ("<bad>", "schema_version = 1\nid = ["),
            (
                "<bad-regex>",
                "schema_version = 1\nid='x'\ndisplay_name='X'\nmatch_argv_regex='['",
            ),
            ("<opencode>", OPENCODE),
        ]);
        assert_eq!(catalog.errors.len(), 2);
        assert_eq!(
            catalog.detect(Some("opencode"), None).unwrap().rule.id,
            "opencode"
        );
    }
}
