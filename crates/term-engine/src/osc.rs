use std::path::PathBuf;
use termust_core::terminal::{PromptState, PromptStatus};

const MAX_OSC: usize = 4096;
const MAX_EVENTS: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PromptMark {
    PromptStart,
    CommandStart,
    CommandExecuted,
    Finished(Option<i32>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OscEvent {
    Title(String),
    Cwd(PathBuf),
    Prompt(PromptMark),
    Command(String),
    ViMode(bool),
}

#[derive(Clone, Debug, Default)]
enum State {
    #[default]
    Ground,
    Esc,
    Osc {
        esc: bool,
    },
    Discard {
        esc: bool,
    },
}

#[derive(Default)]
pub struct OscScanner {
    state: State,
    buf: Vec<u8>,
    events: Vec<OscEvent>,
    title: Option<String>,
    cwd: Option<PathBuf>,
    prompt: PromptStatus,
}

impl OscScanner {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        // En texto normal (sin ESC y fuera de una secuencia) no hay nada que
        // escanear: evita recorrer cada byte del output.
        if matches!(self.state, State::Ground) && !bytes.contains(&0x1b) {
            return;
        }
        for &b in bytes {
            self.step(b);
        }
    }

    pub fn events(&self) -> &[OscEvent] {
        &self.events
    }

    pub fn take_events(&mut self) -> Vec<OscEvent> {
        std::mem::take(&mut self.events)
    }

    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    pub fn cwd(&self) -> Option<&PathBuf> {
        self.cwd.as_ref()
    }

    pub fn prompt(&self) -> PromptStatus {
        self.prompt
    }

    fn step(&mut self, b: u8) {
        match std::mem::take(&mut self.state) {
            State::Ground => {
                if b == 0x1b {
                    self.state = State::Esc;
                } else {
                    self.state = State::Ground;
                }
            }
            State::Esc => {
                if b == b']' {
                    self.buf.clear();
                    self.state = State::Osc { esc: false };
                } else if b == 0x1b {
                    self.state = State::Esc;
                } else {
                    self.state = State::Ground;
                }
            }
            State::Osc { esc } => {
                if esc {
                    if b == b'\\' {
                        self.finalize();
                        self.state = State::Ground;
                    } else {
                        self.push(0x1b);
                        self.push(b);
                        self.state = State::Osc { esc: false };
                    }
                } else if b == 0x1b {
                    self.state = State::Osc { esc: true };
                } else if b == 0x07 {
                    self.finalize();
                    self.state = State::Ground;
                } else {
                    self.push(b);
                    self.state = State::Osc { esc: false };
                }
            }
            State::Discard { esc } => {
                if esc {
                    if b == b'\\' {
                        self.state = State::Ground;
                    } else {
                        self.state = State::Discard { esc: false };
                    }
                } else if b == 0x1b {
                    self.state = State::Discard { esc: true };
                } else if b == 0x07 {
                    self.state = State::Ground;
                } else {
                    self.state = State::Discard { esc: false };
                }
            }
        }
    }

    fn push(&mut self, b: u8) {
        if self.buf.len() < MAX_OSC {
            self.buf.push(b);
        } else {
            self.buf.clear();
            self.state = State::Discard { esc: false };
        }
    }

    fn finalize(&mut self) {
        let s = String::from_utf8_lossy(&self.buf).to_string();
        self.buf.clear();
        let (cmd, rest) = match s.split_once(';') {
            Some((c, r)) => (c, Some(r)),
            None => (s.as_str(), None),
        };
        match cmd {
            "0" | "2" => {
                if let Some(title) = rest {
                    if !title.is_empty() && title.len() <= MAX_OSC {
                        self.title = Some(title.to_string());
                        self.emit(OscEvent::Title(title.to_string()));
                    }
                }
            }
            "7" => {
                if let Some(url) = rest {
                    if let Some(path) = parse_file_url(url) {
                        self.cwd = Some(path.clone());
                        self.emit(OscEvent::Cwd(path));
                    }
                }
            }
            "133" => {
                if let Some(mark) = rest {
                    self.prompt_mark(mark);
                }
            }
            "633" => {
                if let Some(rest) = rest {
                    self.extension(rest);
                }
            }
            _ => {}
        }
    }

    fn prompt_mark(&mut self, mark: &str) {
        let (kind, code) = match mark.split_once(';') {
            Some((k, c)) => (k, Some(c)),
            None => (mark, None),
        };
        let code = code.and_then(|c| c.parse::<i32>().ok());
        match kind {
            "A" => {
                self.prompt.state = PromptState::AtPrompt;
                self.emit(OscEvent::Prompt(PromptMark::PromptStart));
            }
            "B" => {
                self.prompt.state = PromptState::AtPrompt;
                self.emit(OscEvent::Prompt(PromptMark::CommandStart));
            }
            "C" => {
                self.prompt.state = PromptState::CommandRunning;
                self.emit(OscEvent::Prompt(PromptMark::CommandExecuted));
            }
            "D" => {
                self.prompt.state = PromptState::AtPrompt;
                self.prompt.last_exit = code;
                self.emit(OscEvent::Prompt(PromptMark::Finished(code)));
            }
            _ => {}
        }
    }

    fn extension(&mut self, rest: &str) {
        let (kind, payload) = match rest.split_once(';') {
            Some((kind, payload)) => (kind, payload),
            None => (rest, ""),
        };
        match kind {
            "E" => {
                if !payload.is_empty() && payload.len() <= MAX_OSC {
                    self.emit(OscEvent::Command(payload.to_string()));
                }
            }
            "V" => {
                self.emit(OscEvent::ViMode(payload.starts_with("vi")));
            }
            _ => {}
        }
    }

    fn emit(&mut self, ev: OscEvent) {
        if self.events.len() >= MAX_EVENTS {
            self.events.remove(0);
        }
        self.events.push(ev);
    }
}

fn parse_file_url(url: &str) -> Option<PathBuf> {
    let rest = url.strip_prefix("file://")?;
    let slash = rest.find('/')?;
    Some(PathBuf::from(percent_decode(&rest[slash..])))
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_bel() {
        let mut s = OscScanner::new();
        s.feed(b"\x1b]0;hello\x07");
        assert_eq!(s.title(), Some("hello"));
        assert_eq!(s.take_events(), vec![OscEvent::Title("hello".into())]);
    }

    #[test]
    fn title_st_split_across_chunks() {
        let mut s = OscScanner::new();
        s.feed(b"\x1b]2;par");
        s.feed(b"tial\x1b");
        s.feed(b"\\");
        assert_eq!(s.title(), Some("partial"));
    }

    #[test]
    fn cwd_percent_decoded() {
        let mut s = OscScanner::new();
        s.feed(b"\x1b]7;file://host/home/u/my%20repo\x07");
        assert_eq!(s.cwd(), Some(&PathBuf::from("/home/u/my repo")));
    }

    #[test]
    fn prompt_marks_and_exit_code() {
        let mut s = OscScanner::new();
        s.feed(b"\x1b]133;A\x07\x1b]133;B\x07\x1b]133;C\x07");
        assert_eq!(s.prompt().state, PromptState::CommandRunning);
        s.feed(b"\x1b]133;D;7\x07");
        assert_eq!(s.prompt().state, PromptState::AtPrompt);
        assert_eq!(s.prompt().last_exit, Some(7));
    }

    #[test]
    fn command_and_vi_mode_extensions() {
        let mut s = OscScanner::new();
        s.feed(b"\x1b]633;E;cargo test; --all\x07\x1b]633;V;vicmd\x07");
        let events = s.take_events();
        assert!(events.contains(&OscEvent::Command("cargo test; --all".into())));
        assert!(events.contains(&OscEvent::ViMode(true)));
        s.feed(b"\x1b]633;V;main\x07");
        assert!(s.take_events().contains(&OscEvent::ViMode(false)));
    }

    #[test]
    fn unknown_and_oversized_are_ignored() {
        let mut s = OscScanner::new();
        s.feed(b"\x1b]999;whatever\x07");
        assert!(s.title().is_none());
        let huge = vec![b'x'; MAX_OSC + 100];
        s.feed(b"\x1b]0;");
        s.feed(&huge);
        s.feed(b"\x07");
        assert!(s.title().is_none());
    }

    #[test]
    fn malformed_does_not_panic() {
        let mut s = OscScanner::new();
        s.feed(b"\x1b]");
        s.feed(b"7;not-a-url");
        s.feed(b"\x07");
        s.feed(b"\x1b]133;");
        s.feed(b"\x07");
        assert!(s.cwd().is_none());
    }
}
