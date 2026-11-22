//! Lista vertical de pestañas sobre el mismo `TabModel`/`TabView`.
//!
//! No crea pestañas ni PTYs: solo presenta el estado del manager. Cambiar
//! entre layout horizontal y vertical únicamente alterna la visibilidad.

use gtk4 as gtk;
use libadwaita::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use crate::tabs::TabManager;

pub struct TabSidebar {
    root: gtk::Box,
    list: gtk::ListBox,
    manager: Weak<RefCell<TabManager>>,
    ids: Rc<RefCell<Vec<u64>>>,
    last: RefCell<Vec<crate::tabs::TabSummary>>,
    refreshing: Cell<bool>,
}

impl TabSidebar {
    pub fn new(manager: &Rc<RefCell<TabManager>>) -> Rc<Self> {
        let list = gtk::ListBox::new();
        list.add_css_class("navigation-sidebar");
        list.set_selection_mode(gtk::SelectionMode::Single);
        list.set_activate_on_single_click(true);
        list.set_valign(gtk::Align::Start);

        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&list)
            .build();

        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.add_css_class("termust-tab-sidebar");
        root.set_size_request(200, -1);
        root.append(&scroller);

        let sidebar = Rc::new(Self {
            root,
            list: list.clone(),
            manager: Rc::downgrade(manager),
            ids: Rc::new(RefCell::new(Vec::new())),
            last: RefCell::new(Vec::new()),
            refreshing: Cell::new(false),
        });
        sidebar.connect(manager);
        sidebar.refresh();
        sidebar
    }

    fn connect(self: &Rc<Self>, manager: &Rc<RefCell<TabManager>>) {
        {
            let manager = self.manager.clone();
            let ids = self.ids.clone();
            self.list.connect_row_activated(move |_, row| {
                let index = row.index().max(0) as usize;
                let Some(id) = ids.borrow().get(index).copied() else {
                    return;
                };
                if let Some(manager) = manager.upgrade() {
                    if let Ok(mut manager) = manager.try_borrow_mut() {
                        manager.select_tab(id);
                    }
                }
            });
        }
        let weak = Rc::downgrade(self);
        let tab_view = manager.borrow().tab_view().clone();
        tab_view.connect_n_pages_notify(move |_| {
            if let Some(sidebar) = weak.upgrade() {
                sidebar.refresh();
            }
        });
        let weak = Rc::downgrade(self);
        tab_view.connect_selected_page_notify(move |_| {
            if let Some(sidebar) = weak.upgrade() {
                sidebar.refresh();
            }
        });
    }

    pub fn widget(&self) -> &gtk::Box {
        &self.root
    }

    pub fn row_count(&self) -> usize {
        let mut count = 0;
        let mut child = self.list.first_child();
        while let Some(widget) = child {
            count += 1;
            child = widget.next_sibling();
        }
        count
    }

    pub fn refresh(&self) {
        if self.refreshing.replace(true) {
            return;
        }
        let summaries = {
            let Some(manager) = self.manager.upgrade() else {
                self.refreshing.set(false);
                return;
            };
            let Ok(manager) = manager.try_borrow() else {
                self.refreshing.set(false);
                return;
            };
            manager.tab_summaries()
        };

        if *self.last.borrow() == summaries {
            self.refreshing.set(false);
            return;
        }
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        {
            let mut ids = self.ids.borrow_mut();
            ids.clear();
            ids.extend(summaries.iter().map(|summary| summary.id));
        }
        for summary in summaries.iter() {
            let row = gtk::ListBoxRow::new();
            row.set_tooltip_text(Some(&summary.cwd.display().to_string()));

            let content = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            content.set_margin_top(6);
            content.set_margin_bottom(6);
            content.set_margin_start(6);
            content.set_margin_end(6);

            let icon = gtk::Image::from_icon_name("utilities-terminal-symbolic");
            content.append(&icon);

            let labels = gtk::Box::new(gtk::Orientation::Vertical, 0);
            labels.set_hexpand(true);
            let title = gtk::Label::new(Some(&summary.title));
            title.set_xalign(0.0);
            title.set_ellipsize(gtk::pango::EllipsizeMode::End);
            if summary.active {
                title.add_css_class("heading");
            }
            labels.append(&title);
            let cwd = gtk::Label::new(Some(&summary.cwd.display().to_string()));
            cwd.set_xalign(0.0);
            cwd.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
            cwd.add_css_class("dim-label");
            cwd.add_css_class("caption");
            labels.append(&cwd);
            content.append(&labels);

            if summary.attention {
                let attention = gtk::Image::from_icon_name("dialog-warning-symbolic");
                attention.set_tooltip_text(Some("La última orden falló"));
                attention.add_css_class("error");
                content.append(&attention);
            }

            let close = gtk::Button::from_icon_name("window-close-symbolic");
            close.add_css_class("flat");
            close.set_tooltip_text(Some("Cerrar pestaña"));
            close.set_valign(gtk::Align::Center);
            {
                let manager = self.manager.clone();
                let id = summary.id;
                close.connect_clicked(move |_| {
                    if let Some(manager) = manager.upgrade() {
                        TabManager::close_tab(&manager, id);
                    }
                });
            }
            content.append(&close);

            row.set_child(Some(&content));
            self.list.append(&row);
            if summary.active {
                self.list.select_row(Some(&row));
            }
        }
        *self.last.borrow_mut() = summaries;
        self.refreshing.set(false);
    }
}
