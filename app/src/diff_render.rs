//! Render compartido de diffs de git en un `TextBuffer`.
//!
//! Lo usan el panel Git de repositorio y la columna de archivos, para que el
//! diff se vea igual en ambos lugares.

use gtk::prelude::*;
use gtk4 as gtk;
use termust_git_local::{Diff, DiffLineKind};

/// Tag visual de una línea de diff, o `None` si va sin color.
pub fn tag_for(kind: DiffLineKind) -> Option<&'static str> {
    match kind {
        DiffLineKind::Add => Some("termust-diff-add"),
        DiffLineKind::Del => Some("termust-diff-del"),
        DiffLineKind::Hunk => Some("termust-diff-hunk"),
        DiffLineKind::File => Some("termust-diff-file"),
        DiffLineKind::Context | DiffLineKind::Meta => None,
    }
}

pub fn create_diff_tags(buffer: &gtk::TextBuffer) {
    let add = gtk::TextTag::builder().name("termust-diff-add").build();
    add.set_foreground(Some("#26a269"));
    buffer.tag_table().add(&add);
    let del = gtk::TextTag::builder().name("termust-diff-del").build();
    del.set_foreground(Some("#c01c28"));
    buffer.tag_table().add(&del);
    let hunk = gtk::TextTag::builder().name("termust-diff-hunk").build();
    hunk.set_foreground(Some("#1c71d8"));
    buffer.tag_table().add(&hunk);
    let file = gtk::TextTag::builder().name("termust-diff-file").build();
    file.set_foreground(Some("#5e5c64"));
    file.set_weight(600);
    buffer.tag_table().add(&file);
}

/// Vuelca las líneas del diff aplicando los tags ya creados en el buffer.
pub fn fill_diff_buffer(buffer: &gtk::TextBuffer, diff: &Diff) {
    for line in &diff.lines {
        let mut end = buffer.end_iter();
        buffer.insert(&mut end, &format!("{}\n", line.text));
        if let Some(tag) = tag_for(line.kind) {
            let line_number = buffer.line_count().saturating_sub(1);
            if let Some(line_start) = buffer.iter_at_line(line_number) {
                let mut line_end = line_start;
                line_end.forward_to_line_end();
                buffer.apply_tag_by_name(tag, &line_start, &line_end);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use termust_git_local::DiffLineKind;

    #[test]
    fn maps_diff_kinds_to_tags() {
        assert_eq!(tag_for(DiffLineKind::Add), Some("termust-diff-add"));
        assert_eq!(tag_for(DiffLineKind::Del), Some("termust-diff-del"));
        assert_eq!(tag_for(DiffLineKind::Hunk), Some("termust-diff-hunk"));
        assert_eq!(tag_for(DiffLineKind::File), Some("termust-diff-file"));
        assert_eq!(tag_for(DiffLineKind::Context), None);
        assert_eq!(tag_for(DiffLineKind::Meta), None);
    }
}
