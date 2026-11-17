mod gtk;

use crate::devices::{self, Condition, Disk, Health, Report, Result, HOST_READ, HOST_WRITTEN, POWER_ON_TIME, USB_TYPES};
use gtk::*;
use std::{
    cell::RefCell,
    env,
    ffi::{c_int, CString},
    fs,
    path::PathBuf,
    ptr,
    sync::mpsc::{self, Receiver, TryRecvError},
    thread,
};

const WINDOW: &str = include_str!("window.ui");
const HEALTH_BAR_STYLE: &str = "
    progressbar.health-bar > trough,
    progressbar.health-bar > trough > progress {
        min-height: 10px;
        border-width: 0;
        border-radius: 5px;
        margin: 0;
    }
";

struct Desktop {
    disks_list: Object,
    disk_controls: [Object; 3],
    read_button: Object,
    adapter: Object,
    notebook: Object,
    heading: Object,
    subtitle: Object,
    health_panel: Object,
    health_condition: Object,
    health_endurance: Object,
    health_bar: Object,
    written_label: Object,
    read_label: Object,
    power_on_time_label: Object,
    summary: Object,
    full: Object,
    json: Object,
    spinner: Object,
    status: Object,
    disks: Vec<Disk>,
    job: Option<Receiver<Result<Report>>>,
}

impl Desktop {
    unsafe fn selected(&self) -> Option<&Disk> {
        let row = gtk_list_box_get_selected_row(self.disks_list);
        if row.is_null() {
            return None;
        }
        self.disks.get(gtk_list_box_row_get_index(row) as usize)
    }

    unsafe fn refresh(&mut self) {
        let previous = self.selected().map(|disk| disk.path.clone());
        match devices::disks() {
            Ok(disks) => self.disks = disks,
            Err(error) => {
                label_text(self.status, &format!("A meghajtólista nem frissíthető. {error}"));
                return;
            }
        }
        clear_list(self.disks_list);
        for disk in &self.disks {
            let model = disk.model.as_deref().map(str::trim).filter(|model| !model.is_empty());
            let row = gtk_box_new(1, 3);
            margins(row, 10);
            let name = label(model.unwrap_or(&disk.path));
            css(name, "heading");
            gtk_box_append(row, name);
            let details = label(&format!("{}  ·  {}", disk.path, disk.capacity_label()));
            css(details, "dim-label");
            gtk_box_append(row, details);
            gtk_list_box_append(self.disks_list, row);
        }
        let index = previous
            .and_then(|path| self.disks.iter().position(|disk| disk.path == path))
            .unwrap_or(0);
        let row = gtk_list_box_get_row_at_index(self.disks_list, index as c_int);
        if !row.is_null() {
            gtk_list_box_select_row(self.disks_list, row);
        }
        self.show_disk();
    }

    unsafe fn show_disk(&self) {
        gtk_widget_set_visible(self.health_panel, 0);
        clear_list(self.summary);
        buffer_text(self.full, "");
        buffer_text(self.json, "");
        gtk_notebook_set_current_page(self.notebook, 0);
        let Some(disk) = self.selected() else {
            gtk_widget_set_sensitive(self.read_button, 0);
            label_text(self.heading, "Nincs meghajtó kiválasztva");
            label_text(self.subtitle, "");
            label_text(self.status, "A frissítés gombbal újra lekérheted a meghajtólistát.");
            return;
        };
        gtk_widget_set_sensitive(self.read_button, 1);
        let model = disk.model.as_deref().map(str::trim).filter(|model| !model.is_empty());
        label_text(self.heading, model.unwrap_or(&disk.path));
        label_text(self.subtitle, &format!("{}  ·  {}  ·  {}", disk.path, disk.capacity_label(), disk.tran.as_deref().unwrap_or("–")));
        let mut partitions = disk.partitions().peekable();
        if partitions.peek().is_none() {
            self.add_row("Partíciók", "–");
        } else {
            for partition in partitions {
                self.add_row("Partíció", &partition);
            }
        }
        label_text(self.status, "A meghajtó állapotához kattints a SMART lekérése gombra.");
    }

    unsafe fn show_health(&self, health: Health) {
        let color = match health.condition {
            Condition::Passed => "#26935d",
            Condition::MediaWarnings | Condition::LowEndurance => "#ba810a",
            Condition::Critical => "#d74b4b",
            Condition::Unavailable => "#808080",
        };
        let badge = format!("<span foreground=\"{color}\">●</span>  <b>{}</b>", health.condition.label());
        gtk_label_set_markup(self.health_condition, c_text(&badge).as_ptr());
        if let Some(percent) = health.remaining {
            let text = format!("Becsült hátralévő élettartam · {percent}%");
            label_text(self.health_endurance, &text);
            gtk_progress_bar_set_fraction(self.health_bar, f64::from(percent) / 100.0);
        } else {
            label_text(self.health_endurance, "Százalékos élettartam-adat nem érhető el.");
        }
        gtk_widget_set_visible(self.health_bar, health.remaining.is_some().into());
        gtk_widget_set_visible(self.health_panel, 1);
    }

    unsafe fn add_row(&self, name: &str, value: &str) {
        let row = gtk_box_new(0, 16);
        margins(row, 9);
        let key = label(name);
        css(key, "dim-label");
        gtk_widget_set_hexpand(key, 1);
        gtk_box_append(row, key);
        let value = label(value);
        gtk_label_set_selectable(value, 1);
        gtk_label_set_max_width_chars(value, 48);
        gtk_box_append(row, value);
        gtk_list_box_append(self.summary, row);
        gtk_list_box_row_set_activatable(gtk_widget_get_parent(row), 0);
    }

    unsafe fn set_busy(&self, busy: bool) {
        gtk_widget_set_visible(self.spinner, busy.into());
        gtk_spinner_set_spinning(self.spinner, busy.into());
        for widget in self.disk_controls {
            gtk_widget_set_sensitive(widget, (!busy).into());
        }
        gtk_widget_set_sensitive(self.read_button, (!busy && self.selected().is_some()).into());
    }

    unsafe fn read(&mut self) {
        if self.job.is_some() {
            return;
        }
        let Some(disk) = self.selected() else { return };
        let path = disk.path.clone();
        let adapter = usize::try_from(gtk_combo_box_get_active(self.adapter) - 1)
            .ok().and_then(|index| USB_TYPES.get(index).copied());
        let (sender, receiver) = mpsc::channel();
        self.job = Some(receiver);
        self.set_busy(true);
        gtk_widget_set_visible(self.health_panel, 0);
        label_text(self.status, &format!("SMART-adatok lekérése · {path}"));
        thread::spawn(move || {
            let _ = sender.send(devices::read_smart_desktop(&path, adapter));
        });
    }

    unsafe fn receive(&mut self) {
        let Some(job) = &self.job else { return };
        let result = match job.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => Err("A SMART-lekérdezés megszakadt.".into()),
        };
        self.job = None;
        self.set_busy(false);
        match result {
            Ok(report) => {
                clear_list(self.summary);
                for (name, value) in report.rows() {
                    let value = value.as_deref().unwrap_or("–");
                    match name {
                        HOST_WRITTEN => label_text(self.written_label, &format!("Összes írt adat · {value}")),
                        HOST_READ => label_text(self.read_label, &format!("Összes olvasott adat · {value}")),
                        POWER_ON_TIME => label_text(self.power_on_time_label, &format!("Üzemidő · {value}")),
                        _ => self.add_row(name, value),
                    }
                }
                self.show_health(report.health());
                if let Some(disk) = &report.disk {
                    for partition in disk.partitions() {
                        self.add_row("Partíció", &partition);
                    }
                }
                buffer_text(self.full, &report.full());
                buffer_text(self.json, &report.json());
                let mut message = if report.code == 0 {
                    format!("SMART-jelentés · {}", report.path)
                } else {
                    format!("SMART állapotkód: {} · {}", report.code, report.path)
                };
                for detail in report.data["smartctl"]["messages"].as_array().into_iter().flatten() {
                    if let Some(text) = detail["string"].as_str() {
                        message.push_str(" · ");
                        message.push_str(text);
                    }
                }
                label_text(self.status, &message);
            }
            Err(error) => label_text(self.status, &format!("A SMART-lekérdezés sikertelen. {error}")),
        }
    }
}

fn c_text(text: &str) -> CString {
    CString::new(text.replace('\0', "�")).unwrap()
}

unsafe fn label(text: &str) -> Object {
    let widget = gtk_label_new(c_text(text).as_ptr());
    gtk_label_set_xalign(widget, 0.0);
    gtk_label_set_wrap(widget, 1);
    widget
}

unsafe fn label_text(widget: Object, text: &str) {
    gtk_label_set_text(widget, c_text(text).as_ptr());
}

unsafe fn css(widget: Object, class: &str) {
    gtk_widget_add_css_class(widget, c_text(class).as_ptr());
}

unsafe fn margins(widget: Object, margin: c_int) {
    gtk_widget_set_margin_start(widget, margin);
    gtk_widget_set_margin_end(widget, margin);
    gtk_widget_set_margin_top(widget, margin);
    gtk_widget_set_margin_bottom(widget, margin);
}

unsafe fn clear_list(list: Object) {
    loop {
        let row = gtk_widget_get_first_child(list);
        if row.is_null() {
            break;
        }
        gtk_list_box_remove(list, row);
    }
}

unsafe fn buffer_text(view: Object, text: &str) {
    gtk_text_buffer_set_text(gtk_text_view_get_buffer(view), c_text(text).as_ptr(), -1);
}

unsafe fn with_desktop(data: Object, update: impl FnOnce(&mut Desktop)) {
    let desktop = &*data.cast::<RefCell<Desktop>>();
    if let Ok(mut state) = desktop.try_borrow_mut() {
        update(&mut state);
    }
}

unsafe extern "C" fn refresh(_: Object, data: Object) {
    with_desktop(data, |desktop| desktop.refresh());
}

unsafe extern "C" fn selected(_: Object, _: Object, data: Object) {
    with_desktop(data, |desktop| desktop.show_disk());
}

unsafe extern "C" fn read(_: Object, data: Object) {
    with_desktop(data, |desktop| desktop.read());
}

unsafe extern "C" fn poll(data: Object) -> c_int {
    with_desktop(data, |desktop| desktop.receive());
    1
}

unsafe extern "C" fn close(_: Object, data: Object) -> c_int {
    g_main_loop_quit(data);
    0
}

unsafe fn connect(widget: Object, signal: &str, handler: Callback, data: Object) {
    g_signal_connect_data(widget, c_text(signal).as_ptr(), handler, data, None, 0);
}

pub fn run() -> Result<()> {
    // GTK and its callbacks run on this thread. The state and builder outlive the main loop.
    unsafe {
        if gtk_init_check() == 0 {
            return Err("A grafikus munkamenet nem érhető el.".into());
        }
        g_set_application_name(c_text("Roadmap").as_ptr());
        let provider = gtk_css_provider_new();
        gtk_css_provider_load_from_data(provider, HEALTH_BAR_STYLE.as_ptr().cast(), HEALTH_BAR_STYLE.len() as isize);
        gtk_style_context_add_provider_for_display(gdk_display_get_default(), provider, 600);
        g_object_unref(provider);
        let builder = gtk_builder_new_from_string(WINDOW.as_ptr().cast(), WINDOW.len() as isize);
        let widget = |name| gtk_builder_get_object(builder, c_text(name).as_ptr());
        let main_loop = g_main_loop_new(ptr::null_mut(), 0);
        let window = widget("window");
        let disks_list = widget("disks");
        let refresh_button = widget("refresh");
        let read_button = widget("read");
        let adapter = widget("adapter");
        for name in std::iter::once("Automatikus").chain(USB_TYPES.iter().copied()) {
            gtk_combo_box_text_append_text(adapter, c_text(name).as_ptr());
        }
        gtk_combo_box_set_active(adapter, 0);

        let state = RefCell::new(Desktop {
            disks_list,
            disk_controls: [disks_list, refresh_button, adapter],
            read_button,
            adapter,
            notebook: widget("notebook"),
            heading: widget("heading"),
            subtitle: widget("subtitle"),
            health_panel: widget("health_panel"),
            health_condition: widget("health_condition"),
            health_endurance: widget("health_endurance"),
            health_bar: widget("health_bar"),
            written_label: widget("written"),
            read_label: widget("read_total"),
            power_on_time_label: widget("power_on_time"),
            summary: widget("summary"),
            full: widget("full"),
            json: widget("json"),
            spinner: widget("spinner"),
            status: widget("status"),
            disks: Vec::new(),
            job: None,
        });
        state.borrow_mut().show_disk();
        state.borrow_mut().refresh();
        let data = (&state as *const RefCell<Desktop>).cast_mut().cast();
        connect(refresh_button, "clicked", std::mem::transmute::<unsafe extern "C" fn(Object, Object), Callback>(refresh), data);
        connect(read_button, "clicked", std::mem::transmute::<unsafe extern "C" fn(Object, Object), Callback>(read), data);
        connect(disks_list, "row-selected", std::mem::transmute::<unsafe extern "C" fn(Object, Object, Object), Callback>(selected), data);
        connect(window, "close-request", std::mem::transmute::<unsafe extern "C" fn(Object, Object) -> c_int, Callback>(close), main_loop);
        let source = g_timeout_add(100, poll, data);

        gtk_window_present(window);
        g_main_loop_run(main_loop);

        g_source_remove(source);
        g_object_unref(builder);
        g_main_loop_unref(main_loop);
    }
    Ok(())
}

pub fn install() -> Result<()> {
    let data_home = env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
        .ok_or("A felhasználói alkalmazásmenü mappája nem található.")?;
    let applications = data_home.join("applications");
    let executable = env::current_exe()?;
    let executable = executable.to_str().ok_or("A program elérési útja érvénytelen.")?;
    if executable.chars().any(char::is_control) {
        return Err("A program elérési útja vezérlőkaraktert tartalmaz.".into());
    }
    let command = executable.replace('\\', r"\\\\")
        .replace('"', r#"\\""#).replace('$', r"\\$").replace('`', r"\\`").replace('%', "%%");
    let entry = format!(
        "[Desktop Entry]\nType=Application\nName=Roadmap\nComment=Meghajtóállapot és SMART-adatok\nExec=\"{command}\" --gui\nIcon=drive-harddisk\nTerminal=false\nCategories=System;Monitor;\n"
    );
    fs::create_dir_all(&applications)?;
    let path = applications.join("roadmap-smart.desktop");
    fs::write(&path, entry)?;
    println!("A Roadmap elérhető az alkalmazásmenüből. {}", path.display());
    Ok(())
}
