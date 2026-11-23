//! Recepción de resultados de hilos en el hilo principal de GTK.

use gtk::glib;
use gtk4 as gtk;
use std::rc::{Rc, Weak};
use std::sync::mpsc;
use std::time::Duration;

/// Aplica el primer resultado que llegue por `rx` y deja de sondear.
pub(crate) fn listen<W: 'static, T: Send + 'static>(
    weak: Weak<W>,
    rx: mpsc::Receiver<T>,
    apply: impl Fn(&Rc<W>, T) + 'static,
) {
    glib::timeout_add_local(Duration::from_millis(40), move || match rx.try_recv() {
        Ok(value) => {
            if let Some(owner) = weak.upgrade() {
                apply(&owner, value);
            }
            glib::ControlFlow::Break
        }
        Err(mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
        Err(mpsc::TryRecvError::Disconnected) => glib::ControlFlow::Break,
    });
}
