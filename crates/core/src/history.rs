//! Índice de comandos y parsers de historial de shells.
//!
//! Los registros de sesión (OSC 133) tienen prioridad sobre los importados.
//! El índice no guarda salida ni rangos de scrollback; sobrevive a la evicción
//! del scrollback porque el texto vive acá.

use std::path::PathBuf;

pub const MAX_ENTRIES: usize = 10_000;
pub const MAX_BYTES: usize = 2 * 1024 * 1024;
const MAX_ENTRY_BYTES: usize = MAX_BYTES / 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HistorySource {
    Session,
    Imported,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryEntry {
    pub text: String,
    pub cwd: PathBuf,
    pub timestamp: u64,
    pub exit_code: Option<i32>,
    pub source: HistorySource,
    pub last_used: u64,
    pub sequence: u64,
}

impl HistoryEntry {
    pub fn new(
        text: impl Into<String>,
        cwd: impl Into<PathBuf>,
        timestamp: u64,
        exit_code: Option<i32>,
        source: HistorySource,
    ) -> Self {
        Self {
            text: text.into(),
            cwd: cwd.into(),
            timestamp,
            exit_code,
            source,
            last_used: 0,
            sequence: 0,
        }
    }
}

#[derive(Default)]
pub struct HistoryIndex {
    entries: Vec<HistoryEntry>,
    bytes: usize,
    clock: u64,
}

impl HistoryIndex {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn bytes(&self) -> usize {
        self.bytes
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.bytes = 0;
    }

    pub fn insert(&mut self, mut entry: HistoryEntry) {
        entry.text = entry.text.trim().to_string();
        if entry.text.is_empty() || entry.text.len() > MAX_ENTRY_BYTES {
            return;
        }
        self.clock += 1;
        let clock = self.clock;

        if let Some(pos) = self.entries.iter().position(|e| e.text == entry.text) {
            let old_cost = entry_cost(&self.entries[pos]);
            {
                let existing = &mut self.entries[pos];
                let take_metadata = entry.source == HistorySource::Session
                    || existing.source == HistorySource::Imported;
                if take_metadata {
                    existing.cwd = entry.cwd;
                    existing.timestamp = entry.timestamp;
                    existing.exit_code = entry.exit_code;
                }
                if entry.source == HistorySource::Session {
                    existing.source = HistorySource::Session;
                }
                existing.last_used = clock;
                existing.sequence = clock;
            }
            let new_cost = entry_cost(&self.entries[pos]);
            self.bytes = self.bytes.saturating_add(new_cost).saturating_sub(old_cost);
        } else {
            entry.last_used = clock;
            entry.sequence = clock;
            self.bytes += entry_cost(&entry);
            self.entries.push(entry);
        }
        self.enforce();
    }

    /// Marca un comando como usado (LRU) al insertarlo en el prompt.
    pub fn touch(&mut self, text: &str) {
        if let Some(entry) = self.entries.iter_mut().find(|entry| entry.text == text) {
            self.clock += 1;
            entry.last_used = self.clock;
        }
    }

    /// Búsqueda incremental. Prefiere coincidencia exacta, prefijo, límite de
    /// palabra y, dentro de eso, lo más reciente.
    pub fn search(&self, query: &str, limit: usize) -> Vec<HistoryEntry> {
        if limit == 0 {
            return Vec::new();
        }
        let query = query.trim().to_lowercase();
        let mut matches: Vec<(u8, u64, &HistoryEntry)> = Vec::new();
        for entry in &self.entries {
            let text = entry.text.to_lowercase();
            let Some(score) = score(&text, &query) else {
                continue;
            };
            matches.push((score, entry.sequence, entry));
        }
        matches.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| b.1.cmp(&a.1)));
        matches
            .into_iter()
            .take(limit)
            .map(|(_, _, entry)| entry.clone())
            .collect()
    }

    fn enforce(&mut self) {
        while self.entries.len() > MAX_ENTRIES || self.bytes > MAX_BYTES {
            let Some(index) = self
                .entries
                .iter()
                .enumerate()
                .min_by_key(|(_, entry)| (entry.source == HistorySource::Session, entry.last_used))
                .map(|(index, _)| index)
            else {
                break;
            };
            let entry = self.entries.remove(index);
            self.bytes = self.bytes.saturating_sub(entry_cost(&entry));
        }
    }
}

fn entry_cost(entry: &HistoryEntry) -> usize {
    entry.text.len() + entry.cwd.as_os_str().len()
}

fn score(text_lower: &str, query: &str) -> Option<u8> {
    if query.is_empty() {
        return Some(0);
    }
    if text_lower == query {
        return Some(0);
    }
    if text_lower.starts_with(query) {
        return Some(1);
    }
    let position = text_lower.find(query)?;
    let boundary = position == 0
        || matches!(
            text_lower.as_bytes()[position - 1],
            b' ' | b'\t' | b';' | b'&' | b'|' | b'(' | b'/' | b'"' | b'\''
        );
    Some(if boundary { 2 } else { 3 })
}

/// Parsea el formato extendido de zsh (`: <ts>:<dur>;<cmd>`), incluyendo
/// multilínea con `\` al final de línea.
pub fn parse_zsh_history(content: &str) -> Vec<HistoryEntry> {
    let mut out = Vec::new();
    let mut current: Option<(u64, String)> = None;
    let mut continuing = false;

    for line in content.lines() {
        if let Some(rest) = line.strip_prefix(": ") {
            if let Some((meta, command)) = rest.split_once(';') {
                flush(&mut out, current.take());
                let timestamp = meta
                    .split(':')
                    .next()
                    .and_then(|value| value.parse::<u64>().ok())
                    .unwrap_or(0);
                let mut command = command.to_string();
                continuing = command.ends_with('\\');
                if continuing {
                    command.pop();
                }
                current = Some((timestamp, command));
                continue;
            }
        }

        if continuing {
            if let Some((_, command)) = current.as_mut() {
                command.push('\n');
                if let Some(stripped) = line.strip_suffix('\\') {
                    command.push_str(stripped);
                } else {
                    command.push_str(line);
                    continuing = false;
                }
            }
        } else {
            flush(&mut out, current.take());
            let mut command = line.to_string();
            continuing = command.ends_with('\\');
            if continuing {
                command.pop();
            }
            current = Some((0, command));
        }
    }
    flush(&mut out, current.take());
    out
}

/// Parsea historial bash: timestamps `#<n>` opcionales y multilínea con `\`.
pub fn parse_bash_history(content: &str) -> Vec<HistoryEntry> {
    let mut out = Vec::new();
    let mut next_timestamp = 0u64;
    let mut current: Option<(u64, String)> = None;
    let mut continuing = false;

    for line in content.lines() {
        if let Some(rest) = line.strip_prefix('#') {
            if !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()) {
                flush(&mut out, current.take());
                next_timestamp = rest.parse().unwrap_or(0);
                continuing = false;
                continue;
            }
        }
        if continuing {
            if let Some((_, command)) = current.as_mut() {
                command.push('\n');
                if let Some(stripped) = line.strip_suffix('\\') {
                    command.push_str(stripped);
                } else {
                    command.push_str(line);
                    continuing = false;
                }
            }
        } else {
            flush(&mut out, current.take());
            let mut command = line.to_string();
            continuing = command.ends_with('\\');
            if continuing {
                command.pop();
            }
            current = Some((next_timestamp, command));
            next_timestamp = 0;
        }
    }
    flush(&mut out, current.take());
    out
}

fn flush(out: &mut Vec<HistoryEntry>, entry: Option<(u64, String)>) {
    if let Some((timestamp, command)) = entry {
        let command = command.trim();
        if !command.is_empty() {
            out.push(HistoryEntry::new(
                command,
                PathBuf::new(),
                timestamp,
                None,
                HistorySource::Imported,
            ));
        }
    }
}

/// Payload para insertar un comando en el prompt sin ejecutarlo.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HistoryInsertError {
    MultilineWithoutBracketedPaste,
}

pub fn insertion_payload(text: &str, bracketed_paste: bool) -> Result<Vec<u8>, HistoryInsertError> {
    let multiline = text.contains('\n');
    if multiline && !bracketed_paste {
        return Err(HistoryInsertError::MultilineWithoutBracketedPaste);
    }
    let mut out = Vec::with_capacity(text.len() + 12);
    if bracketed_paste {
        out.extend_from_slice(b"\x1b[200~");
        out.extend_from_slice(text.as_bytes());
        out.extend_from_slice(b"\x1b[201~");
    } else {
        out.extend_from_slice(text.as_bytes());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(text: &str, cwd: &str) -> HistoryEntry {
        HistoryEntry::new(text, cwd, 1_700_000_000, Some(0), HistorySource::Session)
    }

    fn imported(text: &str) -> HistoryEntry {
        HistoryEntry::new(text, "", 0, None, HistorySource::Imported)
    }

    #[test]
    fn insert_dedupes_and_session_wins() {
        let mut index = HistoryIndex::new();
        index.insert(imported("cargo test"));
        index.insert(session("cargo test", "/repo"));
        assert_eq!(index.len(), 1);
        let entry = &index.search("cargo", 10)[0];
        assert_eq!(entry.source, HistorySource::Session);
        assert_eq!(entry.cwd, PathBuf::from("/repo"));
        assert_eq!(entry.exit_code, Some(0));
    }

    #[test]
    fn search_ranks_prefix_and_recent() {
        let mut index = HistoryIndex::new();
        index.insert(session("git status", "/a"));
        index.insert(session("git stash", "/a"));
        index.insert(session("cargo test", "/a"));
        let results = index.search("git", 10);
        assert_eq!(
            results
                .iter()
                .take(2)
                .map(|e| e.text.as_str())
                .collect::<Vec<_>>(),
            vec!["git stash", "git status"]
        );
        let results = index.search("statu", 10);
        assert_eq!(results[0].text, "git status");
        assert!(index.search("nada", 10).is_empty());
    }

    #[test]
    fn search_empty_returns_latest_first() {
        let mut index = HistoryIndex::new();
        index.insert(session("one", "/"));
        index.insert(session("two", "/"));
        index.insert(session("three", "/"));
        let results = index.search("", 10);
        assert_eq!(
            results.iter().map(|e| e.text.as_str()).collect::<Vec<_>>(),
            vec!["three", "two", "one"]
        );
    }

    #[test]
    fn touch_does_not_reorder_history() {
        let mut index = HistoryIndex::new();
        index.insert(session("one", "/"));
        index.insert(session("two", "/"));
        index.touch("one");
        let results = index.search("", 10);
        assert_eq!(results[0].text, "two");
        assert_eq!(results[1].text, "one");
    }

    #[test]
    fn reinsert_moves_command_to_top() {
        let mut index = HistoryIndex::new();
        index.insert(session("one", "/"));
        index.insert(session("two", "/"));
        index.insert(session("one", "/"));
        let results = index.search("", 10);
        assert_eq!(results[0].text, "one");
        assert_eq!(results[1].text, "two");
    }

    #[test]
    fn cap_evicts_oldest() {
        let mut index = HistoryIndex::new();
        for i in 0..=MAX_ENTRIES {
            index.insert(session(&format!("cmd-{i}"), "/"));
        }
        assert_eq!(index.len(), MAX_ENTRIES);
        assert!(!index.search("cmd-0", 5).iter().any(|e| e.text == "cmd-0"));
        assert!(index
            .search("cmd-10000", 5)
            .iter()
            .any(|e| e.text == "cmd-10000"));
    }

    #[test]
    fn bytes_cap_is_respected() {
        let mut index = HistoryIndex::new();
        let chunk = "x".repeat(100_000);
        for i in 0..40 {
            index.insert(session(&format!("{chunk}-{i}"), "/"));
        }
        assert!(index.bytes() <= MAX_BYTES);
        assert!(index.len() < 40);
    }

    #[test]
    fn clear_empties_the_index() {
        let mut index = HistoryIndex::new();
        index.insert(session("ls", "/"));
        index.clear();
        assert!(index.is_empty());
        assert_eq!(index.bytes(), 0);
    }

    #[test]
    fn zsh_extended_and_multiline() {
        let text = ": 1700000000:0;cargo test\n: 1700000001:0;echo one\\\ntwo\n";
        let entries = parse_zsh_history(text);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].text, "cargo test");
        assert_eq!(entries[0].timestamp, 1_700_000_000);
        assert_eq!(entries[1].text, "echo one\ntwo");
        assert_eq!(entries[1].source, HistorySource::Imported);
    }

    #[test]
    fn bash_multiline_and_timestamps() {
        let text = "#1700000000\ngit status\n#1700000001\necho one\\\ntwo\n";
        let entries = parse_bash_history(text);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].text, "git status");
        assert_eq!(entries[0].timestamp, 1_700_000_000);
        assert_eq!(entries[1].text, "echo one\ntwo");
        assert_eq!(entries[1].timestamp, 1_700_000_001);
    }

    #[test]
    fn insertion_respects_bracketed_paste() {
        assert_eq!(
            insertion_payload("ls -la", false).unwrap(),
            b"ls -la".to_vec()
        );
        assert_eq!(
            insertion_payload("ls -la", true).unwrap(),
            b"\x1b[200~ls -la\x1b[201~".to_vec()
        );
        assert_eq!(
            insertion_payload("echo one\ntwo", false),
            Err(HistoryInsertError::MultilineWithoutBracketedPaste)
        );
        assert_eq!(
            insertion_payload("echo one\ntwo", true).unwrap(),
            b"\x1b[200~echo one\ntwo\x1b[201~".to_vec()
        );
    }

    #[test]
    fn history_survives_scrollback_eviction() {
        let mut index = HistoryIndex::new();
        index.insert(session("survivor", "/tmp"));
        // El índice no guarda rangos de scrollback: el texto sigue disponible
        // aunque la salida se haya evictado del terminal.
        assert_eq!(index.len(), 1);
        assert_eq!(index.search("survivor", 1)[0].text, "survivor");
    }

    #[test]
    fn imported_entries_are_evicted_before_session_ones() {
        let mut index = HistoryIndex::new();
        for i in 0..MAX_ENTRIES {
            index.insert(imported(&format!("old-{i}")));
        }
        index.insert(session("mine", "/repo"));
        assert_eq!(index.len(), MAX_ENTRIES);
        assert!(index.search("mine", 5).iter().any(|e| e.text == "mine"));
        assert!(index.search("old-0", 5).is_empty());
    }
}
