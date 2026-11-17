mod gtk;
mod history;

use crate::devices::{self, Condition, Disk, Health, Report, Result, HOST_READ, HOST_WRITTEN, POWER_ON_TIME, TEMPERATURE, USB_TYPES};
use gtk::*;
use history::{DriveId, History, Snapshot};
use std::{
    cell::RefCell,
    collections::HashMap,
    env,
    ffi::{c_int, c_ulong, CStr, CString, OsStr},
    fs::{self, OpenOptions},
    io::Write,
    os::unix::{ffi::OsStrExt, fs::OpenOptionsExt},
    path::PathBuf,
    ptr,
    rc::Rc,
    sync::mpsc::{self, Receiver, TryRecvError},
    thread,
    time::{Duration, Instant},
};

const WINDOW: &str = include_str!("window.ui");
const ICON: &[u8] = include_bytes!("roadmap-smart.png");
const HEADER_ICON: &[u8] = include_bytes!("roadmap-smart-header.png");
const APP_ID: &str = "roadmap-smart";
const TEMPERATURE_INTERVAL: Duration = Duration::from_secs(5);
const SENSOR_RETRY: Duration = Duration::from_secs(30);
type TemperatureResult = (Option<String>, Option<PathBuf>);
const HEALTH_BAR_STYLE: &str = "
    progressbar.health-bar > trough,
    progressbar.health-bar > trough > progress {
        min-height: 10px;
        border-width: 0;
        border-radius: 5px;
        margin: 0;
    }
    .roadmap-card { padding: 12px; border-radius: 10px; background-color: alpha(currentColor, 0.06); }
    .roadmap-warning { font-weight: bold; color: @warning_color; }
";

struct Measurement {
    report: Report,
    rows: Vec<(&'static str, Option<String>)>,
    health: Health,
    measured: u64,
    snapshot: Option<Snapshot>,
}

impl Measurement {
    fn new(report: Report) -> Self {
        let rows = report.rows();
        let health = report.health();
        let measured = history::now();
        let value = |name| rows.iter().find(|(label, _)| *label == name)?.1.as_deref();
        let disk_id = report.disk.as_ref().and_then(DriveId::from_disk);
        let drive = report.data["serial_number"].as_str().and_then(|serial| {
            let size = report.disk.as_ref().map_or(0, |disk| disk.size);
            let model = report.disk.as_ref().and_then(|disk| disk.model.as_deref())
                .or_else(|| value("Modell")).unwrap_or_default();
            DriveId::new(serial, model, size)
        }).or(disk_id);
        let snapshot = drive.map(|drive| Snapshot::new(drive, measured, &health, &rows))
            .filter(|snapshot| snapshot.condition != Condition::Unavailable || !snapshot.values.is_empty());
        Self { report, rows, health, measured, snapshot }
    }

    fn export(&self, include_serial: bool) -> String {
        let mut output = format!("Roadmap · SMART-jelentés\nMérve: {}\nKondíció: {}\n",
            date_label(self.measured), self.health.condition.label());
        if let Some(percent) = self.health.remaining {
            output.push_str(&format!("Hátralévő élettartam: {percent}%\n"));
        }
        output.push_str(&format!("SMART állapotkód: {}\n\n", self.report.code));
        for (name, value) in &self.rows {
            if !include_serial && *name == "Sorozatszám" { continue }
            if let Some(value) = value {
                output.push_str(&format!("{name}: {value}\n"));
            }
        }
        output
    }
}

struct Desktop {
    window: Object,
    disks_list: Object,
    disk_controls: [Object; 3],
    read_button: Object,
    adapter: Object,
    notebook: Object,
    heading: Object,
    subtitle: Object,
    temperature_label: Object,
    changes_card: Object,
    changes: Object,
    snapshot_button: Object,
    export_button: Object,
    export_serial: Object,
    history_list: Object,
    diagnostics: Object,
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
    disk_metrics: Vec<Object>,
    measurements: HashMap<String, Rc<Measurement>>,
    history: History,
    history_issue: Option<String>,
    export_dialog: Option<(Object, String)>,
    rendered_tabs: [bool; 3],
    job: Option<Receiver<Result<Measurement>>>,
    temperature_job: Option<(u64, Receiver<TemperatureResult>)>,
    temperature_sensor: Option<PathBuf>,
    temperature_generation: u64,
    next_temperature: Instant,
    last_temperature: Option<String>,
    live_disk: Option<usize>,
    timer: u32,
}

impl Desktop {
    unsafe fn selected_index(&self) -> Option<usize> {
        let row = gtk_list_box_get_selected_row(self.disks_list);
        (!row.is_null()).then(|| gtk_list_box_row_get_index(row) as usize)
    }

    unsafe fn selected(&self) -> Option<&Disk> {
        self.disks.get(self.selected_index()?)
    }

    unsafe fn measurement(&self) -> Option<Rc<Measurement>> {
        self.measurements.get(&self.selected()?.path).cloned()
    }

    unsafe fn refresh(&mut self) {
        let previous = self.selected().map(|disk| disk.path.clone());
        let previous_ids: HashMap<_, _> = self.disks.iter()
            .filter_map(|disk| Some((disk.path.clone(), DriveId::from_disk(disk)?))).collect();
        match devices::disks() {
            Ok(disks) => self.disks = disks,
            Err(error) => {
                label_text(self.status, &format!("A meghajtólista nem frissíthető. {error}"));
                return;
            }
        }
        self.measurements.retain(|path, _| self.disks.iter().find(|disk| disk.path == *path)
            .and_then(DriveId::from_disk).as_ref()
            .is_some_and(|identity| previous_ids.get(path) == Some(identity)));
        self.reload_history();
        clear_list(self.disks_list);
        self.disk_metrics.clear();
        self.live_disk = None;
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
            let metrics = label("");
            css(metrics, "caption");
            css(metrics, "dim-label");
            gtk_box_append(row, metrics);
            self.disk_metrics.push(metrics);
            gtk_list_box_append(self.disks_list, row);
        }
        for index in 0..self.disks.len() {
            self.show_disk_metrics(index, None);
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

    unsafe fn show_disk(&mut self) {
        if let Some(index) = self.live_disk.take() {
            self.show_disk_metrics(index, None);
        }
        self.temperature_generation += 1;
        self.temperature_sensor = None;
        self.last_temperature = self.measurement().and_then(|measurement| measurement.report.temperature())
            .or_else(|| DriveId::from_disk(self.selected()?)
                .and_then(|id| self.history.for_drive(&id).last()?.values.get(TEMPERATURE).cloned()));
        self.next_temperature = Instant::now();
        gtk_widget_set_visible(self.temperature_label, self.selected().is_some().into());
        if self.last_temperature.is_some() {
            self.show_last_temperature();
        } else {
            label_text(self.temperature_label, "Hőfok · Lekérdezés…");
        }
        gtk_widget_set_visible(self.health_panel, 0);
        clear_list(self.summary);
        buffer_text(self.full, "");
        buffer_text(self.json, "");
        buffer_text(self.diagnostics, "Nincs SMART-jelentés.");
        self.rendered_tabs = [false; 3];
        gtk_notebook_set_current_page(self.notebook, 0);
        self.show_history();
        self.set_busy(false);
        let Some(disk) = self.selected() else {
            gtk_widget_set_sensitive(self.read_button, 0);
            label_text(self.heading, "Nincs meghajtó kiválasztva");
            label_text(self.subtitle, "");
            label_text(self.status, "");
            return;
        };
        gtk_widget_set_sensitive(self.read_button, 1);
        let model = disk.model.as_deref().map(str::trim).filter(|model| !model.is_empty());
        label_text(self.heading, model.unwrap_or(&disk.path));
        label_text(self.subtitle, &format!("{}  ·  {}  ·  {}", disk.path, disk.capacity_label(), disk.tran.as_deref().unwrap_or("–")));
        if let Some(measurement) = self.measurement() {
            self.show_measurement(&measurement);
            return;
        }
        let mut partitions = disk.partitions().peekable();
        if partitions.peek().is_none() {
            self.add_row("Partíciók", "–");
        } else {
            for partition in partitions {
                self.add_row("Partíció", &partition);
            }
        }
        label_text(self.status, "");
    }

    fn reload_history(&mut self) {
        match History::load() {
            Ok((history, unreadable)) => {
                self.history = history;
                self.history_issue = (unreadable > 0).then(|| format!("{unreadable} mentett mérés nem olvasható."));
            }
            Err(error) => self.history_issue = Some(format!("Az előzmények nem olvashatók. {error}")),
        }
    }

    unsafe fn show_disk_metrics(&self, index: usize, live_temperature: Option<&str>) {
        let disk = &self.disks[index];
        let measurement = self.measurements.get(&disk.path);
        let drive = DriveId::from_disk(disk);
        let saved = drive.as_ref().and_then(|id| self.history.for_drive(id).last());
        let condition = measurement.map(|item| item.health.condition).or_else(|| saved.map(|item| item.condition));
        let measured = measurement.map(|item| item.measured).or_else(|| saved.map(|item| item.measured));
        let last_temperature = measurement.and_then(|item| item.report.temperature())
            .or_else(|| saved.and_then(|item| item.values.get(TEMPERATURE).cloned()));
        let mut details = condition.map_or_else(String::new, |value| value.label().to_string());
        if let Some(value) = live_temperature {
            if !details.is_empty() { details.push_str(" · "); }
            details.push_str(&format!("{value} élő"));
        } else if let Some(value) = last_temperature {
            if !details.is_empty() { details.push_str(" · "); }
            details.push_str(&format!("{value} mérve"));
        }
        if let Some(time) = measured {
            details.push_str(&format!("\n{}", date_label(time)));
        }
        label_text(self.disk_metrics[index], &details);
        gtk_widget_set_visible(self.disk_metrics[index], (!details.is_empty()).into());
    }

    unsafe fn show_history(&self) {
        clear_list(self.history_list);
        let measurement = self.measurement();
        let current = measurement.as_ref().and_then(|item| item.snapshot.as_ref());
        let identity = current.map(|item| item.drive.clone())
            .or_else(|| self.selected().and_then(DriveId::from_disk));
        let saved = identity.as_ref().map(|id| self.history.for_drive(id)).unwrap_or_default();
        if let Some(issue) = &self.history_issue {
            self.add_history_row(issue, true);
        }
        if saved.is_empty() {
            self.add_history_row("Nincs mentett mérés.", false);
        }
        for snapshot in saved.iter().rev().take(50) {
            let stats = snapshot.values.iter().filter(|(_, value)| value.as_str() != "0")
                .map(|(field, value)| format!("{field}: {value}"));
            let text = std::iter::once(format!("{}   ·   {}", date_label(snapshot.measured), snapshot.condition.label()))
                .chain(snapshot.remaining.map(|value| format!("Hátralévő élettartam: {value}%")))
                .chain(stats).collect::<Vec<_>>().join("\n");
            self.add_history_row(&text, false);
        }
        if saved.len() > 50 {
            self.add_history_row("Legutóbbi 50 mentés", false);
        }
        let baseline = current.and_then(|current| saved.iter().rev().find(|item| item.measured < current.measured));
        let (Some(current), Some(previous)) = (current, baseline) else {
            gtk_widget_set_visible(self.changes_card, 0);
            return;
        };
        gtk_widget_set_visible(self.changes_card, 1);
        let mut warning = false;
        let changes = current.changes(previous);
        let mut lines = vec![format!("Előző mentés · {}", date_label(previous.measured))];
        if changes.is_empty() {
            lines.push(if current.comparable_to(previous) { "Nincs változás." }
                else { "Nincs összehasonlítható SMART-adat." }.into());
        }
        for (line, alert) in changes {
            warning |= alert;
            lines.push(if alert { format!("⚠ {line}") } else { line });
        }
        label_text(self.changes, &lines.join("\n"));
        if warning {
            css(self.changes, "roadmap-warning");
        } else {
            gtk_widget_remove_css_class(self.changes, c_text("roadmap-warning").as_ptr());
        }
    }

    unsafe fn add_history_row(&self, text: &str, warning: bool) {
        let item = label(text);
        margins(item, 10);
        gtk_label_set_selectable(item, 1);
        if warning { css(item, "roadmap-warning"); }
        gtk_list_box_append(self.history_list, item);
        gtk_list_box_row_set_activatable(gtk_widget_get_parent(item), 0);
    }

    unsafe fn show_health(&self, health: &Health) {
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
        let measurement = self.measurement();
        let can_save = measurement.as_ref().and_then(|item| item.snapshot.as_ref())
            .is_some_and(|item| !self.history.for_drive(&item.drive).iter().any(|saved| saved.measured == item.measured));
        gtk_widget_set_sensitive(self.snapshot_button, (!busy && can_save).into());
        gtk_widget_set_sensitive(self.export_button, (!busy && measurement.is_some()).into());
        gtk_widget_set_sensitive(self.export_serial, (!busy && measurement.is_some()).into());
    }

    unsafe fn read(&mut self) {
        if self.job.is_some() {
            return;
        }
        let Some(disk) = self.selected() else { return };
        let path = disk.path.clone();
        let adapter = self.selected_adapter();
        let (sender, receiver) = mpsc::channel();
        self.job = Some(receiver);
        self.set_busy(true);
        label_text(self.status, &format!("SMART-adatok lekérése · {path}"));
        thread::spawn(move || {
            let _ = sender.send(devices::read_smart_desktop(&path, adapter).map(Measurement::new));
        });
    }

    unsafe fn selected_adapter(&self) -> Option<&'static str> {
        usize::try_from(gtk_combo_box_get_active(self.adapter) - 1)
            .ok().and_then(|index| USB_TYPES.get(index).copied())
    }

    unsafe fn poll_temperature(&mut self) {
        if let Some((generation, job)) = &self.temperature_job {
            let (reading, sensor) = match job.try_recv() {
                Ok(reading) => reading,
                Err(TryRecvError::Empty) => return,
                Err(TryRecvError::Disconnected) => (None, None),
            };
            if *generation == self.temperature_generation {
                self.temperature_sensor = sensor;
                match reading {
                    Some(value) => {
                        if let Some(index) = self.selected_index() {
                            self.live_disk = Some(index);
                            self.show_disk_metrics(index, Some(&value));
                        }
                        label_text(self.temperature_label, &format!("Hőfok · {value}"));
                        self.last_temperature = Some(value);
                    }
                    None => {
                        self.live_disk = None;
                        if let Some(index) = self.selected_index() {
                            self.show_disk_metrics(index, None);
                        }
                        self.show_last_temperature();
                        if self.job.is_none() {
                            self.next_temperature = Instant::now() + SENSOR_RETRY;
                        }
                    }
                }
            }
            self.temperature_job = None;
        }

        if Instant::now() < self.next_temperature {
            return;
        }
        let Some(disk) = self.selected().cloned() else { return };
        let adapter = self.selected_adapter();
        let allow_smart_fallback = self.job.is_none();
        let (sender, receiver) = mpsc::channel();
        self.temperature_job = Some((self.temperature_generation, receiver));
        self.next_temperature = Instant::now() + TEMPERATURE_INTERVAL;
        let mut sensor = self.temperature_sensor.take();
        thread::spawn(move || {
            let value = devices::live_temperature(&disk, adapter, allow_smart_fallback, &mut sensor);
            let _ = sender.send((value, sensor));
        });
    }

    unsafe fn show_last_temperature(&self) {
        let label = match &self.last_temperature {
            Some(value) => format!("Hőfok · {value} (utolsó ismert)"),
            None => "Hőfok · Élő adat nem érhető el".into(),
        };
        label_text(self.temperature_label, &label);
    }

    unsafe fn receive(&mut self) {
        let Some(job) = &self.job else { return };
        let result = match job.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => Err("A SMART-lekérdezés megszakadt.".into()),
        };
        self.job = None;
        match result {
            Ok(measurement) => {
                let Some(index) = self.selected_index() else { self.set_busy(false); return };
                let measurement = Rc::new(measurement);
                let report = &measurement.report;
                if let Some(value) = report.temperature() {
                    self.last_temperature = Some(value);
                    self.show_last_temperature();
                    self.next_temperature = Instant::now();
                }
                self.measurements.insert(self.disks[index].path.clone(), measurement.clone());
                self.show_disk_metrics(index, None);
                self.rendered_tabs = [false; 3];
                // A korábbi mérés szövege ne maradjon a még meg nem nyitott füleken.
                buffer_text(self.full, "");
                buffer_text(self.json, "");
                buffer_text(self.diagnostics, "");
                self.show_measurement(&measurement);
                self.show_history();
            }
            Err(error) => label_text(self.status, &format!("A SMART-lekérdezés sikertelen. {error}")),
        }
        self.set_busy(false);
    }

    unsafe fn show_measurement(&mut self, measurement: &Measurement) {
        clear_list(self.summary);
        for (name, value) in &measurement.rows {
            let value = value.as_deref().unwrap_or("–");
            match *name {
                HOST_WRITTEN => label_text(self.written_label, &format!("Összes írt adat · {value}")),
                HOST_READ => label_text(self.read_label, &format!("Összes olvasott adat · {value}")),
                POWER_ON_TIME => label_text(self.power_on_time_label, &format!("Üzemidő · {value}")),
                TEMPERATURE => (),
                _ => self.add_row(name, value),
            }
        }
        self.show_health(&measurement.health);
        if let Some(disk) = &measurement.report.disk {
            for partition in disk.partitions() {
                self.add_row("Partíció", &partition);
            }
        }
        self.render_tab(gtk_notebook_get_current_page(self.notebook) as usize);
        let report = &measurement.report;
        let mut message = format!("SMART · {} · {}", date_label(measurement.measured), report.path);
        if report.code != 0 {
            message.push_str(&format!(" · Állapotkód: {}", report.code));
        }
        for detail in report.data["smartctl"]["messages"].as_array().into_iter().flatten() {
            if let Some(text) = detail["string"].as_str() {
                message.push_str(" · ");
                message.push_str(text);
            }
        }
        label_text(self.status, &message);
    }

    unsafe fn render_tab(&mut self, page: usize) {
        let (view, format): (Object, fn(&Report) -> String) = match page {
            2 => (self.diagnostics, Report::diagnostics),
            3 => (self.full, Report::full),
            4 => (self.json, Report::json),
            _ => return,
        };
        if self.rendered_tabs[page - 2] { return }
        if let Some(measurement) = self.measurement() {
            buffer_text(view, &format(&measurement.report));
            self.rendered_tabs[page - 2] = true;
        }
    }

    unsafe fn save_snapshot(&mut self) {
        let Some(snapshot) = self.measurement().and_then(|item| item.snapshot.clone()) else { return };
        match self.history.save(snapshot) {
            Ok(()) => {
                self.show_history();
                self.set_busy(false);
                label_text(self.status, "A SMART-pillanatkép mentve.");
            }
            Err(error) => label_text(self.status, &format!("A pillanatkép nem menthető. {error}")),
        }
    }

    unsafe fn start_export(&mut self, data: Object) {
        if self.export_dialog.is_some() { return }
        let Some(measurement) = self.measurement() else { return };
        let include_serial = gtk_check_button_get_active(self.export_serial) != 0;
        let dialog = gtk_file_chooser_native_new(c_text("SMART-jelentés mentése").as_ptr(), self.window, FILE_CHOOSER_SAVE,
            c_text("Mentés").as_ptr(), c_text("Mégse").as_ptr());
        gtk_file_chooser_set_current_name(dialog, c_text("roadmap-smart.txt").as_ptr());
        gtk_native_dialog_set_modal(dialog, 1);
        connect(dialog, "response", std::mem::transmute::<unsafe extern "C" fn(Object, c_int, Object), Callback>(exported), data);
        self.export_dialog = Some((dialog, measurement.export(include_serial)));
        gtk_native_dialog_show(dialog);
    }

    unsafe fn finish_export(&mut self, response: c_int) {
        let Some((dialog, text)) = self.export_dialog.take() else { return };
        if response == RESPONSE_ACCEPT {
            let result = export_path(dialog).and_then(|path| {
                let mut file = OpenOptions::new().write(true).create_new(true).mode(0o600).open(&path)
                    .map_err(|error| if error.kind() == std::io::ErrorKind::AlreadyExists {
                        "A fájl már létezik. Válassz új nevet.".into()
                    } else {
                        Box::<dyn std::error::Error + Send + Sync>::from(error)
                    })?;
                if let Err(error) = file.write_all(text.as_bytes()).and_then(|()| file.sync_all()) {
                    let _ = fs::remove_file(&path);
                    return Err(error.into());
                }
                Ok(path)
            });
            let message = match result {
                Ok(path) => format!("Jelentés mentve · {}", path.display()),
                Err(error) => format!("A jelentés nem menthető. {error}"),
            };
            label_text(self.status, &message);
        }
        gtk_native_dialog_destroy(dialog);
        g_object_unref(dialog);
    }

    unsafe fn cancel_timer(&mut self) {
        let timer = std::mem::take(&mut self.timer);
        if timer != 0 {
            g_source_remove(timer);
        }
    }

    unsafe fn schedule(&mut self, data: Object) {
        self.cancel_timer();
        let delay = if self.temperature_job.is_some() {
            Duration::from_millis(100)
        } else if self.selected().is_some() {
            let until_temperature = self.next_temperature.saturating_duration_since(Instant::now());
            if self.job.is_some() { until_temperature.min(Duration::from_millis(100)) }
            else { until_temperature }
        } else if self.job.is_some() {
            Duration::from_millis(100)
        } else {
            return;
        };
        self.timer = g_timeout_add(delay.as_millis().max(1) as u32, poll, data);
    }
}

fn date_label(timestamp: u64) -> String {
    unsafe {
        let date = g_date_time_new_from_unix_local((timestamp / 1_000_000_000) as i64);
        if date.is_null() { return "Ismeretlen mérési idő".into() }
        let formatted = g_date_time_format(date, c_text("%Y-%m-%d %H:%M:%S").as_ptr());
        g_date_time_unref(date);
        if formatted.is_null() { return "Ismeretlen mérési idő".into() }
        let result = CStr::from_ptr(formatted).to_string_lossy().into_owned();
        g_free(formatted.cast());
        result
    }
}

unsafe fn export_path(dialog: Object) -> Result<PathBuf> {
    let file = gtk_file_chooser_get_file(dialog);
    if file.is_null() { return Err("Nincs kiválasztva fájl.".into()) }
    let path = g_file_get_path(file);
    g_object_unref(file);
    if path.is_null() { return Err("A jelentést helyi fájlba mentheted.".into()) }
    let result = PathBuf::from(OsStr::from_bytes(CStr::from_ptr(path).to_bytes()));
    g_free(path.cast());
    Ok(result)
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
    with_desktop(data, |desktop| {
        desktop.refresh();
        desktop.schedule(data);
    });
}

unsafe extern "C" fn selected(_: Object, _: Object, data: Object) {
    with_desktop(data, |desktop| {
        desktop.show_disk();
        desktop.schedule(data);
    });
}

unsafe extern "C" fn read(_: Object, data: Object) {
    with_desktop(data, |desktop| {
        desktop.read();
        desktop.schedule(data);
    });
}

unsafe extern "C" fn poll(data: Object) -> c_int {
    let state = &*data.cast::<RefCell<Desktop>>();
    let Ok(mut desktop) = state.try_borrow_mut() else { return 1 };
    desktop.timer = 0;
    desktop.receive();
    desktop.poll_temperature();
    desktop.schedule(data);
    0
}

unsafe extern "C" fn adapter_changed(_: Object, data: Object) {
    with_desktop(data, |desktop| {
        desktop.temperature_generation += 1;
        desktop.next_temperature = Instant::now();
        desktop.schedule(data);
    });
}

unsafe extern "C" fn tab_changed(_: Object, _: Object, page: u32, data: Object) {
    with_desktop(data, |desktop| desktop.render_tab(page as usize));
}

unsafe extern "C" fn save_snapshot(_: Object, data: Object) {
    with_desktop(data, |desktop| desktop.save_snapshot());
}

unsafe extern "C" fn export_report(_: Object, data: Object) {
    with_desktop(data, |desktop| desktop.start_export(data));
}

unsafe extern "C" fn exported(_: Object, response: c_int, data: Object) {
    with_desktop(data, |desktop| desktop.finish_export(response));
}

unsafe extern "C" fn close(_: Object, data: Object) -> c_int {
    g_main_loop_quit(data);
    // A main loop után bontjuk a jeleket, mielőtt a GTK felszabadítja a widgeteket.
    1
}

unsafe fn connect(widget: Object, signal: &str, handler: Callback, data: Object) -> (Object, c_ulong) {
    let id = g_signal_connect_data(widget, c_text(signal).as_ptr(), handler, data, None, 0);
    (widget, id)
}

pub fn run() -> Result<()> {
    // GTK and its callbacks run on this thread. The state and builder outlive the main loop.
    unsafe {
        g_set_prgname(c_text(APP_ID).as_ptr());
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
        gtk_window_set_icon_name(window, c_text(APP_ID).as_ptr());
        let icon = g_bytes_new_static(HEADER_ICON.as_ptr().cast(), HEADER_ICON.len());
        let texture = gdk_texture_new_from_bytes(icon, ptr::null_mut());
        g_bytes_unref(icon);
        if !texture.is_null() {
            gtk_image_set_from_paintable(widget("app_icon"), texture);
            g_object_unref(texture);
        }
        let disks_list = widget("disks");
        let refresh_button = widget("refresh");
        let read_button = widget("read");
        let adapter = widget("adapter");
        for name in std::iter::once("Automatikus").chain(USB_TYPES.iter().copied()) {
            gtk_combo_box_text_append_text(adapter, c_text(name).as_ptr());
        }
        gtk_combo_box_set_active(adapter, 0);

        let state = RefCell::new(Desktop {
            window,
            disks_list,
            disk_controls: [disks_list, refresh_button, adapter],
            read_button,
            adapter,
            notebook: widget("notebook"),
            heading: widget("heading"),
            subtitle: widget("subtitle"),
            temperature_label: widget("temperature"),
            changes_card: widget("changes_card"),
            changes: widget("changes"),
            snapshot_button: widget("snapshot"),
            export_button: widget("export"),
            export_serial: widget("export_serial"),
            history_list: widget("history_list"),
            diagnostics: widget("diagnostics"),
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
            disk_metrics: Vec::new(),
            measurements: HashMap::new(),
            history: History::default(),
            history_issue: None,
            export_dialog: None,
            rendered_tabs: [false; 3],
            job: None,
            temperature_job: None,
            temperature_sensor: None,
            temperature_generation: 0,
            next_temperature: Instant::now(),
            last_temperature: None,
            live_disk: None,
            timer: 0,
        });
        state.borrow_mut().show_disk();
        state.borrow_mut().refresh();
        let data = (&state as *const RefCell<Desktop>).cast_mut().cast();
        let handlers = [
            connect(refresh_button, "clicked", std::mem::transmute::<unsafe extern "C" fn(Object, Object), Callback>(refresh), data),
            connect(read_button, "clicked", std::mem::transmute::<unsafe extern "C" fn(Object, Object), Callback>(read), data),
            connect(disks_list, "row-selected", std::mem::transmute::<unsafe extern "C" fn(Object, Object, Object), Callback>(selected), data),
            connect(adapter, "changed", std::mem::transmute::<unsafe extern "C" fn(Object, Object), Callback>(adapter_changed), data),
            connect(widget("notebook"), "switch-page", std::mem::transmute::<unsafe extern "C" fn(Object, Object, u32, Object), Callback>(tab_changed), data),
            connect(widget("snapshot"), "clicked", std::mem::transmute::<unsafe extern "C" fn(Object, Object), Callback>(save_snapshot), data),
            connect(widget("export"), "clicked", std::mem::transmute::<unsafe extern "C" fn(Object, Object), Callback>(export_report), data),
            connect(window, "close-request", std::mem::transmute::<unsafe extern "C" fn(Object, Object) -> c_int, Callback>(close), main_loop),
        ];
        state.borrow_mut().schedule(data);

        gtk_window_present(window);
        g_main_loop_run(main_loop);

        for (object, id) in handlers {
            g_signal_handler_disconnect(object, id);
        }
        {
            let mut state = state.borrow_mut();
            state.cancel_timer();
            if let Some((dialog, _)) = state.export_dialog.take() {
                gtk_native_dialog_destroy(dialog);
                g_object_unref(dialog);
            }
        }
        gtk_window_destroy(window);
        g_object_unref(builder);
        g_main_loop_unref(main_loop);
    }
    Ok(())
}

fn data_home() -> Result<PathBuf> {
    env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
        .ok_or_else(|| "A felhasználói adatmappa nem található.".into())
}

pub fn install() -> Result<()> {
    let data_home = data_home()?;
    let applications = data_home.join("applications");
    let executable = env::current_exe()?;
    let executable = executable.to_str().ok_or("A program elérési útja érvénytelen.")?;
    if executable.chars().any(char::is_control) {
        return Err("A program elérési útja vezérlőkaraktert tartalmaz.".into());
    }
    let command = executable.replace('\\', r"\\\\")
        .replace('"', r#"\\""#).replace('$', r"\\$").replace('`', r"\\`").replace('%', "%%");
    let entry = format!(
        "[Desktop Entry]\nType=Application\nName=Roadmap\nComment=Meghajtóállapot és SMART-adatok\nExec=\"{command}\" --gui\nIcon={APP_ID}\nStartupWMClass={APP_ID}\nTerminal=false\nCategories=System;Monitor;\n"
    );
    let icons = data_home.join("icons/hicolor/256x256/apps");
    fs::create_dir_all(&icons)?;
    fs::write(icons.join(format!("{APP_ID}.png")), ICON)?;
    fs::create_dir_all(&applications)?;
    let path = applications.join(format!("{APP_ID}.desktop"));
    fs::write(&path, entry)?;
    println!("A Roadmap elérhető az alkalmazásmenüből. {}", path.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn report_export_only_includes_serial_when_enabled() {
        let measurement = Measurement::new(Report {
            data: json!({
                "model_name": "SSD test", "serial_number": "SECRET-SERIAL",
                "smart_status": {"passed": true},
                "smartctl": {"output": ["Serial Number: SECRET-SERIAL"]}
            }),
            code: 0, path: "/dev/test".into(), disk: None,
        });
        assert!(measurement.export(false).contains("SSD test"));
        assert!(!measurement.export(false).contains("SECRET-SERIAL"));
        assert!(measurement.export(true).contains("Sorozatszám: SECRET-SERIAL"));
    }
}
