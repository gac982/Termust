//! Vista renderizada de Markdown: documento con estilos, tablas, links e imágenes.
//!
//! Es una primera pasada: convierte los eventos de `pulldown-cmark` en texto con
//! tags de `GtkTextView` (títulos, énfasis, listas, citas, código), e inserta
//! tablas e imágenes como widgets anclados al buffer.

use gtk::prelude::*;
use gtk4 as gtk;
use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

const LINK_COLOR: &str = "#1c71d8";
pub const MAX_RENDER_BYTES: usize = 1_000_000;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MarkdownCounts {
    pub headings: usize,
    pub tables: usize,
    pub links: usize,
    pub images: usize,
    pub code_blocks: usize,
}

pub struct MarkdownPreview {
    root: gtk::ScrolledWindow,
    text_view: gtk::TextView,
    buffer: gtk::TextBuffer,
    base_tag: gtk::TextTag,
    headings: Vec<gtk::TextTag>,
    strong: gtk::TextTag,
    em: gtk::TextTag,
    strike: gtk::TextTag,
    code: gtk::TextTag,
    code_block: gtk::TextTag,
    quote: gtk::TextTag,
    rule: gtk::TextTag,
    list_tags: RefCell<HashMap<usize, gtk::TextTag>>,
    link_urls: RefCell<HashMap<String, String>>,
    counts: Cell<MarkdownCounts>,
    base_dir: RefCell<Option<PathBuf>>,
}

#[derive(Default)]
struct State {
    inline: Vec<gtk::TextTag>,
    block: Vec<gtk::TextTag>,
    lists: Vec<Option<u64>>,
    link: Option<(gtk::TextTag, String, i32)>,
    image: Option<(String, String)>,
    table_rows: Vec<Vec<String>>,
    row: Option<Vec<String>>,
    cell: Option<String>,
    last_newlines: usize,
}

impl MarkdownPreview {
    pub fn new() -> Rc<Self> {
        let buffer = gtk::TextBuffer::new(None);
        let text_view = gtk::TextView::with_buffer(&buffer);
        text_view.set_editable(false);
        text_view.set_cursor_visible(false);
        text_view.set_wrap_mode(gtk::WrapMode::Word);
        text_view.set_left_margin(24);
        text_view.set_right_margin(24);
        text_view.set_top_margin(20);
        text_view.set_bottom_margin(24);
        text_view.add_css_class("termust-markdown-view");
        let root = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .hexpand(true)
            .vexpand(true)
            .child(&text_view)
            .build();
        root.add_css_class("termust-markdown");

        let family = gtk::Settings::default()
            .and_then(|settings| settings.gtk_font_name())
            .and_then(|name| {
                gtk::pango::FontDescription::from_string(&name)
                    .family()
                    .map(|family| family.to_string())
            })
            .unwrap_or_else(|| "Cantarell".into());
        let base_tag = gtk::TextTag::builder()
            .name("md-base")
            .family(family)
            .build();
        buffer.tag_table().add(&base_tag);

        let headings = [2.0, 1.55, 1.3, 1.15, 1.05, 0.95]
            .iter()
            .enumerate()
            .map(|(index, scale)| {
                let tag = gtk::TextTag::builder()
                    .name(format!("md-h{}", index + 1))
                    .scale(*scale)
                    .weight(700)
                    .pixels_above_lines(if index == 0 { 20 } else { 14 })
                    .pixels_below_lines(6)
                    .build();
                buffer.tag_table().add(&tag);
                tag
            })
            .collect::<Vec<_>>();

        let strong = gtk::TextTag::builder()
            .name("md-strong")
            .weight(700)
            .build();
        let em = gtk::TextTag::builder()
            .name("md-em")
            .style(gtk::pango::Style::Italic)
            .build();
        let strike = gtk::TextTag::builder()
            .name("md-strike")
            .strikethrough(true)
            .build();
        let code = gtk::TextTag::builder()
            .name("md-code")
            .family("monospace")
            .background_rgba(&gray(0.14))
            .build();
        let code_block = gtk::TextTag::builder()
            .name("md-code-block")
            .family("monospace")
            .paragraph_background_rgba(&gray(0.10))
            .pixels_above_lines(6)
            .pixels_below_lines(6)
            .build();
        let quote = gtk::TextTag::builder()
            .name("md-quote")
            .indent(22)
            .foreground_rgba(&gray(0.60))
            .build();
        let rule = gtk::TextTag::builder()
            .name("md-rule")
            .foreground_rgba(&gray(0.45))
            .build();
        for tag in [&strong, &em, &strike, &code, &code_block, &quote, &rule] {
            buffer.tag_table().add(tag);
        }

        let view = Rc::new(Self {
            root,
            text_view,
            buffer,
            base_tag,
            headings,
            strong,
            em,
            strike,
            code,
            code_block,
            quote,
            rule,
            list_tags: RefCell::new(HashMap::new()),
            link_urls: RefCell::new(HashMap::new()),
            counts: Cell::new(MarkdownCounts::default()),
            base_dir: RefCell::new(None),
        });

        {
            let gesture = gtk::GestureClick::new();
            gesture.set_button(1);
            let weak = Rc::downgrade(&view);
            gesture.connect_released(move |_, _, x, y| {
                let Some(view) = weak.upgrade() else {
                    return;
                };
                if let Some(iter) = view.text_view.iter_at_location(x as i32, y as i32) {
                    for tag in iter.tags() {
                        let name = tag.name().map(|name| name.to_string()).unwrap_or_default();
                        if let Some(url) = view.link_urls.borrow().get(&name) {
                            open_link(url);
                            return;
                        }
                    }
                }
            });
            view.text_view.add_controller(gesture);
        }

        view
    }

    pub fn grab_focus(&self) {
        self.text_view.grab_focus();
    }

    pub fn widget(&self) -> &gtk::ScrolledWindow {
        &self.root
    }

    pub fn text(&self) -> String {
        self.buffer
            .text(&self.buffer.start_iter(), &self.buffer.end_iter(), false)
            .to_string()
    }

    pub fn counts(&self) -> MarkdownCounts {
        self.counts.get()
    }

    pub fn render(self: &Rc<Self>, text: &str, base: &Path) {
        self.buffer.set_text("");
        self.link_urls.borrow_mut().clear();
        self.list_tags.borrow_mut().clear();
        self.counts.set(MarkdownCounts::default());
        self.base_dir.replace(Some(base.to_path_buf()));

        let mut state = State::default();
        let options = Options::ENABLE_TABLES
            | Options::ENABLE_TASKLISTS
            | Options::ENABLE_STRIKETHROUGH
            | Options::ENABLE_FOOTNOTES;
        for event in Parser::new_ext(text, options) {
            self.event(event, &mut state);
        }
        self.ensure_newlines(&mut state, 1);

        let start = self.buffer.start_iter();
        let end = self.buffer.end_iter();
        self.buffer.apply_tag(&self.base_tag, &start, &end);
        let mut top = self.buffer.start_iter();
        self.text_view
            .scroll_to_iter(&mut top, 0.0, false, 0.0, 0.0);
    }

    fn event(self: &Rc<Self>, event: Event<'_>, state: &mut State) {
        match event {
            Event::Start(tag) => self.start_tag(tag, state),
            Event::End(tag) => self.end_tag(tag, state),
            Event::Text(text) => self.push_text(&text, state),
            Event::Code(code) => {
                state.inline.push(self.code.clone());
                self.push_text(&code, state);
                state.inline.pop();
            }
            Event::SoftBreak => self.push_text(" ", state),
            Event::HardBreak => self.push_text("\n", state),
            Event::Rule => {
                state.block.push(self.rule.clone());
                self.push_text(&"─".repeat(48), state);
                state.block.pop();
                self.ensure_newlines(state, 2);
            }
            Event::TaskListMarker(done) => {
                self.push_text(if done { "☑ " } else { "☐ " }, state);
            }
            Event::Html(text) | Event::InlineHtml(text) => self.push_text(&text, state),
            _ => {}
        }
    }

    fn start_tag(self: &Rc<Self>, tag: Tag<'_>, state: &mut State) {
        match tag {
            Tag::Paragraph => {}
            Tag::Heading { level, .. } => {
                let index = heading_index(level);
                state.block.push(self.headings[index].clone());
                self.bump(|counts| counts.headings += 1);
            }
            Tag::BlockQuote(_) => state.block.push(self.quote.clone()),
            Tag::CodeBlock(kind) => {
                let _ = kind;
                state.block.push(self.code_block.clone());
                self.bump(|counts| counts.code_blocks += 1);
            }
            Tag::List(start) => state.lists.push(start),
            Tag::Item => {
                let depth = state.lists.len();
                let tag = self.list_tag(depth);
                state.block.push(tag);
                let marker = match state.lists.last_mut() {
                    Some(Some(number)) => {
                        let marker = format!("{number}. ");
                        *number += 1;
                        marker
                    }
                    _ => "• ".to_string(),
                };
                self.push_text(&marker, state);
            }
            Tag::Emphasis => state.inline.push(self.em.clone()),
            Tag::Strong => state.inline.push(self.strong.clone()),
            Tag::Strikethrough => state.inline.push(self.strike.clone()),
            Tag::Link { dest_url, .. } => {
                let id = self.link_urls.borrow().len();
                let name = format!("md-link-{id}");
                let tag = gtk::TextTag::builder()
                    .name(&name)
                    .foreground(LINK_COLOR)
                    .underline(gtk::pango::Underline::Single)
                    .build();
                self.buffer.tag_table().add(&tag);
                self.link_urls
                    .borrow_mut()
                    .insert(name, dest_url.to_string());
                self.bump(|counts| counts.links += 1);
                state.inline.push(tag.clone());
                state.link = Some((tag, dest_url.to_string(), self.offset()));
            }
            Tag::Image { dest_url, .. } => {
                state.image = Some((dest_url.to_string(), String::new()));
            }
            Tag::Table(_) => {
                state.table_rows.clear();
                self.bump(|counts| counts.tables += 1);
            }
            Tag::TableHead | Tag::TableRow => state.row = Some(Vec::new()),
            Tag::TableCell => state.cell = Some(String::new()),
            _ => {}
        }
    }

    fn end_tag(self: &Rc<Self>, tag: TagEnd, state: &mut State) {
        match tag {
            TagEnd::Paragraph => {
                self.ensure_newlines(state, if state.lists.is_empty() { 2 } else { 1 });
            }
            TagEnd::Heading(_) => {
                state.block.pop();
                self.ensure_newlines(state, 2);
            }
            TagEnd::BlockQuote(_) => {
                state.block.pop();
                self.ensure_newlines(state, 2);
            }
            TagEnd::CodeBlock => {
                state.block.pop();
                self.ensure_newlines(state, 2);
            }
            TagEnd::List(_) => {
                state.lists.pop();
                self.ensure_newlines(state, 2);
            }
            TagEnd::Item => {
                state.block.pop();
                self.ensure_newlines(state, 1);
            }
            TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough => {
                state.inline.pop();
            }
            TagEnd::Link => {
                if let Some((tag, url, start)) = state.link.take() {
                    let end = self.offset();
                    state.inline.retain(|active| active != &tag);
                    let name = tag.name().map(|name| name.to_string()).unwrap_or_default();
                    self.link_urls.borrow_mut().insert(name, url);
                    let _ = (start, end);
                }
            }
            TagEnd::Image => {
                if let Some((url, alt)) = state.image.take() {
                    self.insert_image(&url, &alt, state);
                }
            }
            TagEnd::TableCell => {
                if let (Some(cell), Some(row)) = (state.cell.take(), state.row.as_mut()) {
                    row.push(cell);
                }
            }
            TagEnd::TableHead | TagEnd::TableRow => {
                if let Some(row) = state.row.take() {
                    state.table_rows.push(row);
                }
            }
            TagEnd::Table => {
                let rows = std::mem::take(&mut state.table_rows);
                self.insert_table(&rows, state);
            }
            _ => {}
        }
    }

    fn push_text(self: &Rc<Self>, text: &str, state: &mut State) {
        if let Some((_, alt)) = state.image.as_mut() {
            alt.push_str(text);
            return;
        }
        if let Some(cell) = state.cell.as_mut() {
            cell.push_str(text);
            return;
        }
        self.insert_text(text, state);
    }

    fn insert_text(self: &Rc<Self>, text: &str, state: &mut State) {
        if text.is_empty() {
            return;
        }
        let start = self.offset();
        let mut end = self.buffer.end_iter();
        self.buffer.insert(&mut end, text);
        let from = self.buffer.iter_at_offset(start);
        let to = self
            .buffer
            .iter_at_offset(start + text.chars().count() as i32);
        for tag in state.inline.iter().chain(state.block.iter()) {
            self.buffer.apply_tag(tag, &from, &to);
        }
        state.last_newlines = text.chars().rev().take_while(|ch| *ch == '\n').count();
    }

    fn ensure_newlines(self: &Rc<Self>, state: &mut State, count: usize) {
        if state.last_newlines >= count {
            return;
        }
        let missing = count - state.last_newlines;
        let start = self.offset();
        let mut end = self.buffer.end_iter();
        self.buffer.insert(&mut end, &"\n".repeat(missing));
        let from = self.buffer.iter_at_offset(start);
        let to = self.buffer.iter_at_offset(start + missing as i32);
        for tag in state.block.iter() {
            self.buffer.apply_tag(tag, &from, &to);
        }
        state.last_newlines = count;
    }

    fn bump(&self, update: impl FnOnce(&mut MarkdownCounts)) {
        let mut counts = self.counts.get();
        update(&mut counts);
        self.counts.set(counts);
    }

    fn offset(&self) -> i32 {
        self.buffer.end_iter().offset()
    }

    fn list_tag(self: &Rc<Self>, depth: usize) -> gtk::TextTag {
        if let Some(tag) = self.list_tags.borrow().get(&depth) {
            return tag.clone();
        }
        let tag = gtk::TextTag::builder()
            .name(format!("md-list-{depth}"))
            .indent((depth as i32) * 22)
            .pixels_below_lines(2)
            .build();
        self.buffer.tag_table().add(&tag);
        self.list_tags.borrow_mut().insert(depth, tag.clone());
        tag
    }

    fn insert_table(self: &Rc<Self>, rows: &[Vec<String>], state: &mut State) {
        if rows.is_empty() {
            return;
        }
        let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
        if columns == 0 {
            return;
        }
        let grid = gtk::Grid::new();
        grid.add_css_class("termust-md-table");
        grid.set_column_spacing(16);
        grid.set_row_spacing(2);
        grid.set_margin_top(8);
        grid.set_margin_bottom(8);
        for (row_index, row) in rows.iter().enumerate() {
            for col in 0..columns {
                let text = row.get(col).map(String::as_str).unwrap_or("");
                let label = gtk::Label::new(Some(text));
                label.set_xalign(0.0);
                label.set_wrap(true);
                label.set_margin_start(6);
                label.set_margin_end(6);
                label.set_margin_top(4);
                label.set_margin_bottom(4);
                if row_index == 0 {
                    label.add_css_class("termust-md-th");
                }
                grid.attach(&label, col as i32, row_index as i32, 1, 1);
            }
        }
        self.insert_widget(&grid);
        self.ensure_newlines(state, 2);
    }

    fn insert_image(self: &Rc<Self>, url: &str, alt: &str, state: &mut State) {
        self.bump(|counts| counts.images += 1);
        let remote = url.starts_with("http://") || url.starts_with("https://");
        let path = if remote {
            None
        } else {
            let base = self.base_dir.borrow().clone().unwrap_or_default();
            Some(base.join(url))
        };
        if let Some(path) = path {
            if path.is_file() {
                let picture = gtk::Picture::for_filename(&path);
                picture.set_can_shrink(true);
                picture.set_content_fit(gtk::ContentFit::Contain);
                picture.set_width_request(420);
                picture.set_margin_top(8);
                picture.set_margin_bottom(8);
                self.insert_widget(&picture);
                self.ensure_newlines(state, 2);
                return;
            }
        }
        let label = gtk::Label::new(Some(if alt.is_empty() { url } else { alt }));
        label.set_xalign(0.0);
        label.set_wrap(true);
        label.add_css_class("dim-label");
        label.set_tooltip_text(Some(url));
        label.set_margin_top(6);
        label.set_margin_bottom(6);
        self.insert_widget(&label);
        self.ensure_newlines(state, 2);
    }

    fn insert_widget(self: &Rc<Self>, widget: &impl IsA<gtk::Widget>) {
        let mut iter = self.buffer.end_iter();
        let anchor = self.buffer.create_child_anchor(&mut iter);
        self.text_view.add_child_at_anchor(widget, &anchor);
        let mut end = self.buffer.end_iter();
        self.buffer.insert(&mut end, "\n");
    }
}

fn heading_index(level: pulldown_cmark::HeadingLevel) -> usize {
    use pulldown_cmark::HeadingLevel::*;
    match level {
        H1 => 0,
        H2 => 1,
        H3 => 2,
        H4 => 3,
        H5 => 4,
        H6 => 5,
    }
}

fn gray(alpha: f32) -> gtk::gdk::RGBA {
    gtk::gdk::RGBA::new(0.5, 0.5, 0.5, alpha)
}

fn open_link(url: &str) {
    if url.starts_with("http://") || url.starts_with("https://") || url.starts_with("mailto:") {
        let launcher = gtk::UriLauncher::new(url);
        launcher.launch(None::<&gtk::Window>, None::<&gtk::gio::Cancellable>, |_| {});
    }
}
