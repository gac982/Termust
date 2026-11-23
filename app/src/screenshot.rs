//! Capturas de widgets para documentación (hook de desarrollo).

use gtk::gdk::prelude::*;
use gtk::gsk::prelude::*;
use gtk::prelude::*;
use gtk4 as gtk;

/// Renderiza un widget mapeado a PNG. Devuelve `false` si aún no tiene tamaño.
pub fn save_widget_png(widget: &impl IsA<gtk::Widget>, path: &str) -> bool {
    let width = widget.width() as f64;
    let height = widget.height() as f64;
    if width < 1.0 || height < 1.0 {
        return false;
    }
    let paintable = gtk::WidgetPaintable::new(Some(widget));
    let snapshot = gtk::Snapshot::new();
    paintable.snapshot(&snapshot, width, height);
    let Some(node) = snapshot.to_node() else {
        return false;
    };
    let Some(native) = widget.native() else {
        return false;
    };
    let Some(renderer) = native.renderer() else {
        return false;
    };
    let viewport = gtk::graphene::Rect::new(0.0, 0.0, width as f32, height as f32);
    let texture = renderer.render_texture(&node, Some(&viewport));
    texture.save_to_png(path).is_ok()
}
