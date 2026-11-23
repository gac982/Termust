//! Área de trabajo del monitor y barra superior del shell.
//!
//! En Wayland no existe una API para preguntar cuánto reserva el shell: el
//! contrato es el *work area* que Mutter aplica al maximizar. Por eso la
//! detección no asume píxeles ni resoluciones: se mide la diferencia entre la
//! geometría del monitor y la que el compositor dio a la ventana maximizada.
//! Esa diferencia es la franja real de la barra superior (28 px en un 1080p,
//! otra en 1440p, otra con escala 2, etc.) y se recalcula con cada cambio de
//! tamaño, de monitor o de escala. Si hubiera además un dock fijo inferior, la
//! diferencia incluye también esa reserva.
//!
//! La ventana nunca pide pantalla completa: solo `maximize()`, que es lo que
//! hace que el compositor respete la reserva del shell.

use gtk::gdk;
use gtk::prelude::*;
use gtk4 as gtk;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

/// Identifica un monitor por geometría y escala; sirve para recordar la
/// reserva medida en cada configuración sin depender del nombre del conector.
type MonitorKey = (i32, i32, i32, i32, i32);

const LOG_ENV: &str = "TERMUST_WORKAREA_LOG";

/// Rectángulo útil del monitor: el alto descuenta la franja que reserva el
/// shell. Independiente de la resolución y de la escala.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WorkArea {
    pub monitor_x: i32,
    pub monitor_y: i32,
    pub monitor_width: i32,
    pub monitor_height: i32,
    pub reserved: i32,
}

impl WorkArea {
    pub fn new(monitor_width: i32, monitor_height: i32, reserved: i32) -> Self {
        let inset = reserved.clamp(0, monitor_height.max(0));
        Self {
            monitor_x: 0,
            monitor_y: 0,
            monitor_width: monitor_width.max(0),
            monitor_height: monitor_height.max(0),
            reserved: inset,
        }
    }

    pub fn of_monitor(monitor: &gdk::Monitor, reserved: i32) -> Self {
        let geometry = monitor.geometry();
        let mut area = Self::new(geometry.width(), geometry.height(), reserved);
        area.monitor_x = geometry.x();
        area.monitor_y = geometry.y();
        area
    }

    /// Alto disponible debajo de la barra.
    pub fn height(&self) -> i32 {
        (self.monitor_height - self.reserved).max(0)
    }

    pub fn fits(&self, width: i32, height: i32) -> bool {
        width <= self.monitor_width && height <= self.height()
    }
}

/// Monitor donde está la ventana (o el de su superficie).
pub fn monitor_of(window: &gtk::Window) -> Option<gdk::Monitor> {
    let surface = window.surface()?;
    surface.display().monitor_at_surface(&surface)
}

/// Reserva vertical (barra superior y, si la hubiera, dock fijo inferior) que
/// el compositor está aplicando ahora mismo. En GNOME con la barra superior
/// siempre visible es exactamente el alto de esa barra.
///
/// Solo tiene sentido con la ventana maximizada: ahí el alto de la ventana es
/// exactamente el work area del monitor.
pub fn measured_reservation(window: &gtk::Window) -> i32 {
    if !window.is_maximized() {
        return 0;
    }
    let Some(monitor) = monitor_of(window) else {
        return 0;
    };
    let geometry = monitor.geometry();
    (geometry.height() - window.height()).clamp(0, geometry.height())
}

fn log_enabled() -> bool {
    std::env::var_os(LOG_ENV).is_some()
}

fn verify(window: &gtk::Window, known: &RefCell<HashMap<MonitorKey, i32>>, reason: &str) {
    let Some(monitor) = monitor_of(window) else {
        return;
    };
    let geometry = monitor.geometry();
    let maximized = window.is_maximized();
    let inset = if maximized {
        (geometry.height() - window.height()).clamp(0, geometry.height())
    } else {
        0
    };
    let key = (
        geometry.x(),
        geometry.y(),
        geometry.width(),
        geometry.height(),
        monitor.scale_factor(),
    );

    if !maximized {
        if log_enabled() {
            eprintln!(
                "termust-workarea: motivo={reason} sin-maximizar monitor={}x{}+{},{} escala={} ventana={}x{}",
                geometry.width(),
                geometry.height(),
                geometry.x(),
                geometry.y(),
                monitor.scale_factor(),
                window.width(),
                window.height()
            );
        }
        return;
    }

    let area = WorkArea::of_monitor(&monitor, inset);
    let fills_width = (window.width() - area.monitor_width).abs() <= 1;
    if !fills_width {
        // Muestra intermedia: la propiedad `maximized` cambia antes de que el
        // compositor redimensione, y en X11 el frame incluye sombras. No es una
        // medición válida del work area; se ignora y se espera al `layout`.
        if log_enabled() {
            eprintln!(
                "termust-workarea: motivo={reason} transicion monitor={}x{} ventana={}x{}",
                area.monitor_width,
                area.monitor_height,
                window.width(),
                window.height()
            );
        }
        return;
    }
    let previous = known.borrow_mut().insert(key, inset);
    let reaches_bottom = inset + window.height() >= area.monitor_height - 1;
    let fits = area.fits(window.width(), window.height());
    // Si antes había una reserva en este mismo monitor y ahora desapareció,
    // puede ser una barra con auto-ocultar o un compositor que ignoró el
    // strut. Se informa, no se fuerza: pelear con el shell por auto-ocultar
    // sería peor que dejarlo decidir.
    let reservation_lost = inset == 0 && previous.unwrap_or(0) > 0 && fits;

    if log_enabled() || reservation_lost {
        eprintln!(
            "termust-workarea: motivo={reason} monitor={}x{}+{},{} escala={} reserva_vertical={}px work_area={}x{} ventana={}x{} ancho_completo={} fondo_completo={}{}",
            area.monitor_width,
            area.monitor_height,
            area.monitor_x,
            area.monitor_y,
            monitor.scale_factor(),
            inset,
            area.monitor_width,
            area.height(),
            window.width(),
            window.height(),
            fills_width,
            reaches_bottom,
            if reservation_lost {
                " aviso=reserva-no-aplicada"
            } else {
                ""
            }
        );
    }
}

/// Observa maximizado, cambios de tamaño y cambios de monitor/escala.
///
/// No hay polling: el compositor emite `layout` cuando cambia la superficie
/// (maximizar, cambiar de monitor, cambiar resolución o alto de la barra).
pub fn install(window: &gtk::Window) {
    let known: Rc<RefCell<HashMap<MonitorKey, i32>>> = Rc::new(RefCell::new(HashMap::new()));

    {
        let known = known.clone();
        window.connect_maximized_notify(move |window| {
            verify(window, &known, "propiedad-maximized");
        });
    }

    if window.surface().is_some() {
        attach_layout(window, &known);
    } else {
        // Si todavía no hay superficie (ventana sin mapear), engancharse al
        // realize en vez de sondear.
        let known = known.clone();
        window.connect_realize(move |window| attach_layout(window, &known));
    }

    verify(window, &known, "instalacion");
}

fn attach_layout(window: &gtk::Window, known: &Rc<RefCell<HashMap<MonitorKey, i32>>>) {
    let Some(surface) = window.surface() else {
        return;
    };
    let window = window.clone();
    let known = known.clone();
    let last: Rc<RefCell<Option<(i32, i32)>>> = Rc::new(RefCell::new(None));
    surface.connect_layout(move |_surface, width, height| {
        if *last.borrow() == Some((width, height)) {
            return;
        }
        *last.borrow_mut() = Some((width, height));
        verify(&window, &known, "layout");
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn work_area_descuenta_la_barra_en_cualquier_resolucion() {
        // (ancho, alto, reserva de la barra, alto útil esperado)
        let cases = [
            (1920, 1080, 28, 1052),
            (2560, 1440, 28, 1412),
            (3840, 2160, 56, 2104),
            (1366, 768, 24, 744),
            (1024, 600, 22, 578),
        ];
        for (width, height, inset, useful) in cases {
            let area = WorkArea::new(width, height, inset);
            assert_eq!(area.height(), useful, "{width}x{height} inset {inset}");
            assert!(area.fits(width, useful));
            assert!(!area.fits(width, height));
        }
    }

    #[test]
    fn sin_barra_o_reserva_invalida_no_recorta_de_mas() {
        assert_eq!(WorkArea::new(1920, 1080, 0).height(), 1080);
        assert_eq!(WorkArea::new(1920, 1080, -5).reserved, 0);
        assert_eq!(WorkArea::new(1920, 1080, 5000).reserved, 1080);
        assert_eq!(WorkArea::new(1920, 1080, 5000).height(), 0);
    }

    #[test]
    fn escala_no_cambia_la_aritmetica_logica() {
        // GTK entrega geometría en píxeles lógicos: la misma pantalla 4K con
        // escala 2 mide 1920x1080 y la barra lógica sigue siendo 28.
        let area = WorkArea::new(1920, 1080, 28);
        assert_eq!(area.height(), 1052);
        assert_eq!(area.monitor_width, 1920);
    }
}
