//! Resaltado, folding y outline para el visor de solo lectura.
//!
//! El texto se carga con topes (2 MiB), se detecta binario/no-UTF8 y se parsea
//! con tree-sitter. El resaltado se calcula solo para el rango de líneas pedido.

#![forbid(unsafe_code)]

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use streaming_iterator::StreamingIterator;
use tree_sitter::{Parser, Query, QueryCursor, Tree};

/// Tope de lectura para el visor (FR-5.4).
pub const MAX_BYTES: usize = 2 * 1024 * 1024;
const BINARY_SAMPLE: usize = 8192;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Language {
    Rust,
    JavaScript,
    TypeScript,
    Tsx,
    Go,
    Php,
    Html,
    Css,
    Astro,
    Vue,
    Json,
    Toml,
    Yaml,
    Markdown,
    Plain,
}

impl Language {
    pub fn detect(path: &Path, _content: &str) -> Self {
        match path
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or("")
            .to_ascii_lowercase()
            .as_str()
        {
            "rs" => Self::Rust,
            "js" | "jsx" | "mjs" | "cjs" => Self::JavaScript,
            "ts" | "mts" | "cts" => Self::TypeScript,
            "tsx" => Self::Tsx,
            "go" => Self::Go,
            "php" => Self::Php,
            "html" | "htm" => Self::Html,
            "css" => Self::Css,
            "astro" => Self::Astro,
            "vue" => Self::Vue,
            "json" => Self::Json,
            "toml" => Self::Toml,
            "yaml" | "yml" => Self::Yaml,
            "md" | "markdown" => Self::Markdown,
            _ => Self::Plain,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Rust => "Rust",
            Self::JavaScript => "JavaScript",
            Self::TypeScript => "TypeScript",
            Self::Tsx => "TSX",
            Self::Go => "Go",
            Self::Php => "PHP",
            Self::Html => "HTML",
            Self::Css => "CSS",
            Self::Astro => "Astro",
            Self::Vue => "Vue (HTML + injection)",
            Self::Json => "JSON",
            Self::Toml => "TOML",
            Self::Yaml => "YAML",
            Self::Markdown => "Markdown",
            Self::Plain => "Plain text",
        }
    }

    fn grammar(self) -> Option<tree_sitter::Language> {
        Some(match self {
            Self::Rust => tree_sitter_rust::LANGUAGE.into(),
            Self::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
            Self::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Self::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
            Self::Go => tree_sitter_go::LANGUAGE.into(),
            Self::Php => tree_sitter_php::LANGUAGE_PHP.into(),
            Self::Html | Self::Vue => tree_sitter_html::LANGUAGE.into(),
            Self::Css => tree_sitter_css::LANGUAGE.into(),
            Self::Astro => tree_sitter_astro_next::LANGUAGE.into(),
            Self::Json => tree_sitter_json::LANGUAGE.into(),
            Self::Toml => tree_sitter_toml_ng::LANGUAGE.into(),
            Self::Yaml => tree_sitter_yaml::LANGUAGE.into(),
            Self::Markdown => tree_sitter_md::LANGUAGE.into(),
            Self::Plain => return None,
        })
    }

    fn highlights_query(self) -> Option<&'static str> {
        Some(match self {
            Self::Rust => tree_sitter_rust::HIGHLIGHTS_QUERY,
            Self::JavaScript => tree_sitter_javascript::HIGHLIGHT_QUERY,
            Self::TypeScript | Self::Tsx => tree_sitter_typescript::HIGHLIGHTS_QUERY,
            Self::Go => tree_sitter_go::HIGHLIGHTS_QUERY,
            Self::Php => tree_sitter_php::HIGHLIGHTS_QUERY,
            Self::Html | Self::Vue => tree_sitter_html::HIGHLIGHTS_QUERY,
            Self::Css => tree_sitter_css::HIGHLIGHTS_QUERY,
            Self::Astro => tree_sitter_astro_next::HIGHLIGHTS_QUERY,
            Self::Json => tree_sitter_json::HIGHLIGHTS_QUERY,
            Self::Toml => tree_sitter_toml_ng::HIGHLIGHTS_QUERY,
            Self::Yaml => tree_sitter_yaml::HIGHLIGHTS_QUERY,
            Self::Markdown => tree_sitter_md::HIGHLIGHT_QUERY_BLOCK,
            Self::Plain => return None,
        })
    }

    fn tags_query(self) -> Option<&'static str> {
        match self {
            Self::Rust => Some(tree_sitter_rust::TAGS_QUERY),
            Self::JavaScript => Some(tree_sitter_javascript::TAGS_QUERY),
            Self::TypeScript | Self::Tsx => Some(tree_sitter_typescript::TAGS_QUERY),
            Self::Go => Some(tree_sitter_go::TAGS_QUERY),
            Self::Php => Some(tree_sitter_php::TAGS_QUERY),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoadMode {
    Normal,
    /// Más de 2 MiB: se muestra el prefijo en texto plano.
    LargeFile,
    /// Bytes que no son UTF-8 válidos: texto plano con aviso.
    NonUtf8,
    /// Contiene NUL: no se muestra como texto.
    Binary,
}

#[derive(Clone, Debug)]
pub struct Document {
    pub path: PathBuf,
    pub content: String,
    pub language: Language,
    pub mode: LoadMode,
    pub bytes: u64,
    line_starts: Vec<usize>,
}

impl Document {
    pub fn load(path: &Path) -> std::io::Result<Self> {
        let metadata = std::fs::metadata(path)?;
        let file = File::open(path)?;
        let mut buffer = Vec::new();
        file.take(MAX_BYTES as u64 + 1).read_to_end(&mut buffer)?;
        let truncated = buffer.len() > MAX_BYTES;
        if truncated {
            buffer.truncate(MAX_BYTES);
        }
        let bytes = metadata.len();
        if buffer.iter().take(BINARY_SAMPLE).any(|byte| *byte == 0) {
            return Ok(Self {
                path: path.to_path_buf(),
                content: String::new(),
                language: Language::detect(path, ""),
                mode: LoadMode::Binary,
                bytes,
                line_starts: vec![0],
            });
        }
        let (content, invalid_utf8) = match String::from_utf8(buffer) {
            Ok(content) => (content, false),
            Err(error) => (String::from_utf8_lossy(error.as_bytes()).into_owned(), true),
        };
        let mode = if truncated {
            LoadMode::LargeFile
        } else if invalid_utf8 {
            LoadMode::NonUtf8
        } else {
            LoadMode::Normal
        };
        let language = Language::detect(path, &content);
        let line_starts = compute_line_starts(&content);
        Ok(Self {
            path: path.to_path_buf(),
            content,
            language,
            mode,
            bytes,
            line_starts,
        })
    }

    pub fn from_text(path: &Path, content: &str) -> Self {
        Self {
            path: path.to_path_buf(),
            content: content.to_string(),
            language: Language::detect(path, content),
            mode: LoadMode::Normal,
            bytes: content.len() as u64,
            line_starts: compute_line_starts(content),
        }
    }

    pub fn line_count(&self) -> usize {
        self.line_starts.len()
    }

    pub fn line_start(&self, line: usize) -> usize {
        let index = line.min(self.line_starts.len().saturating_sub(1));
        self.line_starts[index]
    }

    pub fn line_of_byte(&self, byte: usize) -> usize {
        match self.line_starts.binary_search(&byte) {
            Ok(index) => index,
            Err(index) => index.saturating_sub(1),
        }
    }

    /// Parsea el documento completo. `None` para texto plano o binario.
    pub fn parse(&self) -> Option<Parsed> {
        Parsed::new(self.language, &self.content)
    }
}

fn compute_line_starts(content: &str) -> Vec<usize> {
    let mut starts = vec![0];
    for (index, byte) in content.bytes().enumerate() {
        if byte == b'\n' {
            starts.push(index + 1);
        }
    }
    starts
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HighlightKind {
    Keyword,
    String,
    Comment,
    Function,
    Type,
    Number,
    Constant,
    Property,
    Variable,
    Operator,
    Punctuation,
    Escape,
    Attribute,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HighlightSpan {
    pub start: usize,
    pub end: usize,
    pub kind: HighlightKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FoldRegion {
    pub start_line: usize,
    pub end_line: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutlineItem {
    pub name: String,
    pub kind: String,
    pub line: usize,
}

/// Documento parseado. Es `Send`: el parseo puede vivir en un hilo de fondo.
pub struct Parsed {
    language: Language,
    content: String,
    line_starts: Vec<usize>,
    tree: Tree,
}

impl Parsed {
    pub fn new(language: Language, content: &str) -> Option<Self> {
        let grammar = language.grammar()?;
        let mut parser = Parser::new();
        parser.set_language(&grammar).ok()?;
        let tree = parser.parse(content, None)?;
        Some(Self {
            language,
            content: content.to_string(),
            line_starts: compute_line_starts(content),
            tree,
        })
    }

    pub fn language(&self) -> Language {
        self.language
    }

    pub fn content(&self) -> &str {
        &self.content
    }

    fn line_start(&self, line: usize) -> usize {
        let index = line.min(self.line_starts.len().saturating_sub(1));
        self.line_starts[index]
    }

    /// Spans que intersectan las líneas `first..=last`, recortados a ese rango.
    pub fn highlight_lines(&self, first: usize, last: usize) -> Vec<HighlightSpan> {
        let start = self.line_start(first);
        let end = if last + 1 >= self.line_starts.len() {
            self.content.len()
        } else {
            self.line_start(last + 1)
        };
        if start >= end {
            return Vec::new();
        }
        let mut spans = Vec::new();
        self.highlight_range(start, end, &mut spans);
        spans
    }

    fn highlight_range(&self, start: usize, end: usize, spans: &mut Vec<HighlightSpan>) {
        if let Some(query) = self.language.highlights_query() {
            if let Some(grammar) = self.language.grammar() {
                run_query(
                    &grammar,
                    query,
                    &self.tree,
                    &self.content,
                    start,
                    end,
                    0,
                    spans,
                );
            }
        }
        if self.language == Language::Vue {
            self.highlight_vue_injections(start, end, spans);
        }
        if self.language == Language::Markdown {
            run_query(
                &tree_sitter_md::INLINE_LANGUAGE.into(),
                tree_sitter_md::HIGHLIGHT_QUERY_INLINE,
                &self.tree,
                &self.content,
                start,
                end,
                0,
                spans,
            );
        }
    }

    /// Vue SFC: el HTML cubre template y tags; script/style se parsean con sus
    /// grammars y se desplazan al offset original.
    fn highlight_vue_injections(&self, start: usize, end: usize, spans: &mut Vec<HighlightSpan>) {
        let mut stack = vec![self.tree.root_node()];
        while let Some(node) = stack.pop() {
            let kind = node.kind();
            if kind == "script_element" || kind == "style_element" {
                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    if child.kind() != "raw_text" {
                        continue;
                    }
                    let text = &self.content[child.start_byte()..child.end_byte()];
                    let (grammar, query) = if kind == "script_element" {
                        (
                            tree_sitter_javascript::LANGUAGE.into(),
                            tree_sitter_javascript::HIGHLIGHT_QUERY,
                        )
                    } else {
                        (
                            tree_sitter_css::LANGUAGE.into(),
                            tree_sitter_css::HIGHLIGHTS_QUERY,
                        )
                    };
                    let mut parser = Parser::new();
                    if parser.set_language(&grammar).is_err() {
                        continue;
                    }
                    let Some(tree) = parser.parse(text, None) else {
                        continue;
                    };
                    let offset = child.start_byte();
                    let local_start = start.saturating_sub(offset);
                    let local_end = end.saturating_sub(offset).min(text.len());
                    if local_start < local_end {
                        run_query(
                            &grammar,
                            query,
                            &tree,
                            text,
                            local_start,
                            local_end,
                            offset,
                            spans,
                        );
                    }
                }
            }
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                stack.push(child);
            }
        }
    }

    /// Regiones plegables por tipo de nodo (funciones, bloques, elementos).
    pub fn folds(&self) -> Vec<FoldRegion> {
        let mut folds = Vec::new();
        let mut stack = vec![self.tree.root_node()];
        while let Some(node) = stack.pop() {
            let start_line = node.start_position().row;
            let end_line = node.end_position().row;
            if end_line > start_line && is_foldable(node.kind()) {
                folds.push(FoldRegion {
                    start_line,
                    end_line,
                });
            }
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                stack.push(child);
            }
        }
        folds.sort_by_key(|fold| (fold.start_line, std::cmp::Reverse(fold.end_line)));
        folds.dedup();
        folds
    }

    /// Definiciones de primer nivel vía tags query (Rust/JS/TS/Go/PHP).
    pub fn outline(&self) -> Vec<OutlineItem> {
        let Some(source) = self.language.tags_query() else {
            return Vec::new();
        };
        let Some(grammar) = self.language.grammar() else {
            return Vec::new();
        };
        let Ok(query) = Query::new(&grammar, source) else {
            return Vec::new();
        };
        let names = query.capture_names();
        let mut cursor = QueryCursor::new();
        let mut items = Vec::new();
        let mut matches = cursor.matches(&query, self.tree.root_node(), self.content.as_bytes());
        while let Some(query_match) = matches.next() {
            let mut name: Option<String> = None;
            let mut kind: Option<String> = None;
            let mut line = 0;
            for capture in query_match.captures() {
                let capture_name = names[capture.index as usize];
                if capture_name == "name" {
                    name = self
                        .content
                        .get(capture.node.byte_range())
                        .map(str::to_string);
                    line = capture.node.start_position().row;
                } else if capture_name.starts_with("definition.") {
                    kind = Some(capture_name.trim_start_matches("definition.").to_string());
                    if line == 0 {
                        line = capture.node.start_position().row;
                    }
                }
            }
            if let (Some(name), Some(kind)) = (name, kind) {
                items.push(OutlineItem { name, kind, line });
            }
        }
        items.sort_by_key(|item| (item.line, item.name.clone()));
        items.dedup_by(|a, b| a.name == b.name && a.line == b.line);
        items
    }
}

#[allow(clippy::too_many_arguments)]
fn run_query(
    grammar: &tree_sitter::Language,
    source: &str,
    tree: &Tree,
    text: &str,
    start: usize,
    end: usize,
    offset: usize,
    spans: &mut Vec<HighlightSpan>,
) {
    let Ok(query) = Query::new(grammar, source) else {
        return;
    };
    let names = query.capture_names();
    let mut query_cursor = QueryCursor::new();
    query_cursor.set_byte_range(start..end);
    let mut matches = query_cursor.matches(&query, tree.root_node(), text.as_bytes());
    while let Some(query_match) = matches.next() {
        for capture in query_match.captures() {
            let Some(kind) = classify(names[capture.index as usize]) else {
                continue;
            };
            let node_start = capture.node.start_byte();
            let node_end = capture.node.end_byte();
            let span_start = node_start.max(start);
            let span_end = node_end.min(end);
            if span_start < span_end {
                spans.push(HighlightSpan {
                    start: span_start + offset,
                    end: span_end + offset,
                    kind,
                });
            }
        }
    }
}

fn classify(capture: &str) -> Option<HighlightKind> {
    let kind = if capture.contains("comment") {
        HighlightKind::Comment
    } else if capture.contains("escape") {
        HighlightKind::Escape
    } else if capture.contains("string") {
        HighlightKind::String
    } else if capture.contains("keyword") {
        HighlightKind::Keyword
    } else if capture.contains("function") || capture.contains("method") {
        HighlightKind::Function
    } else if capture.contains("constructor") || capture.contains("type") {
        HighlightKind::Type
    } else if capture.contains("number") || capture.contains("float") || capture.contains("integer")
    {
        HighlightKind::Number
    } else if capture.contains("constant") || capture.contains("boolean") {
        HighlightKind::Constant
    } else if capture.contains("property")
        || capture.contains("field")
        || capture.contains("tag")
        || capture.contains("label")
    {
        HighlightKind::Property
    } else if capture.contains("attribute") {
        HighlightKind::Attribute
    } else if capture.contains("operator") {
        HighlightKind::Operator
    } else if capture.contains("punctuation") || capture.contains("delimiter") {
        HighlightKind::Punctuation
    } else if capture.contains("variable") || capture.contains("parameter") {
        HighlightKind::Variable
    } else {
        return None;
    };
    Some(kind)
}

fn is_foldable(kind: &str) -> bool {
    matches!(
        kind,
        "function_item"
            | "impl_item"
            | "trait_item"
            | "mod_item"
            | "struct_item"
            | "enum_item"
            | "union_item"
            | "function_definition"
            | "class_definition"
            | "function_declaration"
            | "method_definition"
            | "class_declaration"
            | "interface_declaration"
            | "type_declaration"
            | "arrow_function"
            | "statement_block"
            | "block"
            | "element"
            | "script_element"
            | "style_element"
            | "rule_set"
            | "media_statement"
            | "component"
            | "join"
            | "subquery"
            | "declaration_list"
            | "field_declaration_list"
            | "enclosed"
    )
}

/// Utilidad para tests y UI: recorta el fin de una línea sin el `\n`.
pub fn line_text(content: &str, start: usize) -> &str {
    let end = content[start..]
        .find('\n')
        .map(|offset| start + offset)
        .unwrap_or(content.len());
    &content[start..end]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::{Path, PathBuf};

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "termust-syntax-{label}-{}-{}",
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

    fn rust_source(functions: usize) -> String {
        let mut source = String::new();
        for index in 0..functions {
            source.push_str(&format!("fn f{index}() {{\n    let x = {index};\n}}\n"));
        }
        source
    }

    #[test]
    fn viewer_detects_languages() {
        assert_eq!(Language::detect(Path::new("a.rs"), ""), Language::Rust);
        assert_eq!(
            Language::detect(Path::new("a.js"), ""),
            Language::JavaScript
        );
        assert_eq!(
            Language::detect(Path::new("a.ts"), ""),
            Language::TypeScript
        );
        assert_eq!(Language::detect(Path::new("a.tsx"), ""), Language::Tsx);
        assert_eq!(Language::detect(Path::new("a.go"), ""), Language::Go);
        assert_eq!(Language::detect(Path::new("a.php"), ""), Language::Php);
        assert_eq!(Language::detect(Path::new("a.html"), ""), Language::Html);
        assert_eq!(Language::detect(Path::new("a.css"), ""), Language::Css);
        assert_eq!(Language::detect(Path::new("a.astro"), ""), Language::Astro);
        assert_eq!(Language::detect(Path::new("a.vue"), ""), Language::Vue);
        assert_eq!(Language::detect(Path::new("a.json"), ""), Language::Json);
        assert_eq!(Language::detect(Path::new("a.toml"), ""), Language::Toml);
        assert_eq!(Language::detect(Path::new("a.yaml"), ""), Language::Yaml);
        assert_eq!(Language::detect(Path::new("a.md"), ""), Language::Markdown);
        assert_eq!(Language::detect(Path::new("a.bin"), "x"), Language::Plain);
    }

    #[test]
    fn viewer_large_file_plain_mode() {
        let dir = TempDir::new("large");
        let path = dir.path().join("large.rs");
        let payload = vec![b'a'; MAX_BYTES + 1024];
        fs::write(&path, &payload).unwrap();
        let document = Document::load(&path).unwrap();
        assert_eq!(document.mode, LoadMode::LargeFile);
        assert!(document.content.len() <= MAX_BYTES);
        assert!(document.bytes > MAX_BYTES as u64);
    }

    #[test]
    fn viewer_binary_detection() {
        let dir = TempDir::new("binary");
        let path = dir.path().join("image.dat");
        fs::write(&path, [0u8, 159, 146, 150, 0, 1]).unwrap();
        let document = Document::load(&path).unwrap();
        assert_eq!(document.mode, LoadMode::Binary);
        assert!(document.content.is_empty());
    }

    #[test]
    fn viewer_non_utf8_plain_mode() {
        let dir = TempDir::new("nonutf8");
        let path = dir.path().join("legacy.txt");
        fs::write(&path, b"hola \xff mundo\n").unwrap();
        let document = Document::load(&path).unwrap();
        assert_eq!(document.mode, LoadMode::NonUtf8);
        assert!(document.content.contains("hola"));
    }

    #[test]
    fn viewer_highlights_viewport_only() {
        let source = rust_source(300);
        let document = Document::from_text(Path::new("fixture.rs"), &source);
        let parsed = document.parse().expect("parse");
        let spans = parsed.highlight_lines(100, 120);
        assert!(!spans.is_empty(), "sin spans en el viewport");
        let start = document.line_start(100);
        let end = document.line_start(121);
        for span in &spans {
            assert!(
                span.start >= start && span.end <= end,
                "span fuera del viewport: {}..{}",
                span.start,
                span.end
            );
        }
        assert!(
            spans.len() < 200,
            "demasiados spans para 21 líneas: {}",
            spans.len()
        );
    }

    #[test]
    fn viewer_fold_regions_rust_fixture() {
        let source = "impl Foo {\n    fn alpha() {\n        let x = 1;\n    }\n\n    fn beta() {\n        let y = 2;\n    }\n}\n";
        let document = Document::from_text(Path::new("fold.rs"), source);
        let parsed = document.parse().expect("parse");
        let folds = parsed.folds();
        assert!(
            folds
                .iter()
                .any(|fold| fold.start_line == 0 && fold.end_line >= 8),
            "falta el fold del impl: {folds:?}"
        );
        assert!(
            folds
                .iter()
                .any(|fold| fold.start_line == 1 && fold.end_line >= 3),
            "falta el fold de alpha: {folds:?}"
        );
    }

    #[test]
    fn viewer_outline_rust_fixture() {
        let source =
            "struct Foo;\n\nfn alpha() {\n    let x = 1;\n}\n\nfn beta() {\n    let y = 2;\n}\n";
        let document = Document::from_text(Path::new("outline.rs"), source);
        let parsed = document.parse().expect("parse");
        let outline = parsed.outline();
        assert!(
            outline.iter().any(|item| item.name == "alpha"),
            "{outline:?}"
        );
        assert!(
            outline.iter().any(|item| item.name == "beta"),
            "{outline:?}"
        );
    }

    #[test]
    fn viewer_highlights_config_and_markup_formats() {
        for (name, content) in [
            ("a.json", "{\"a\": 1, \"b\": true}\n"),
            ("a.toml", "[section]\nname = \"x\"\n"),
            ("a.yaml", "name: x\nvalue: 1\n"),
            ("a.md", "# Title\n\n**bold** and `code`\n"),
        ] {
            let document = Document::from_text(Path::new(name), content);
            let parsed = document.parse().expect("parse");
            assert!(
                !parsed.highlight_lines(0, 10).is_empty(),
                "sin resaltado para {name}"
            );
        }
    }

    #[test]
    fn viewer_vue_falls_back_to_injection() {
        let source = "<template><div class=\"x\">{{ msg }}</div></template>\n<script>\nconst msg = 1;\nfunction hi() { return msg; }\n</script>\n<style>\n.x { color: red; }\n</style>\n";
        let document = Document::from_text(Path::new("widget.vue"), source);
        assert_eq!(document.language, Language::Vue);
        let parsed = document.parse().expect("parse");
        let spans = parsed.highlight_lines(0, 10);
        let keyword = spans.iter().any(|span| {
            span.kind == HighlightKind::Keyword
                && document.content[span.start..span.end].contains("const")
        });
        assert!(keyword, "falta resaltado JS inyectado: {spans:?}");
    }
}
