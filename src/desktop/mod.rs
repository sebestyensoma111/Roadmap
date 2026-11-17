mod gtk;
mod history;
mod sheet;
mod trend;

use crate::devices::{self, Condition, Disk, Health, Report, Result, HOST_WRITTEN, POWER_ON_TIME, TEMPERATURE, USB_TYPES};
use gtk::*;
use history::{DriveId, History, Snapshot};
use std::{
    cell::RefCell,
    collections::HashMap,
    env,
    ffi::{c_int, c_ulong, CStr, CString, OsStr},
    fs::{self, OpenOptions},
    io::{Read, Write},
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
    .roadmap-warning { font-weight: bold; color: mix(currentColor, @warning_color, 0.4); }
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

    fn png(&self, include_serial: bool) -> Result<Vec<u8>> {
        let value = |name| self.rows.iter().find(|(label, _)| *label == name)?.1.as_deref();
        let capacity = self.report.disk.as_ref().map(|disk| {
            if disk.size >= 1_000_000_000_000 {
                format!("{:.2} TB", disk.size as f64 / 1e12)
            } else {
                format!("{:.1} GB", disk.size as f64 / 1e9)
            }
        });
        let remaining = self.health.remaining.map(|percent| format!("{percent}%"));
        let metrics: Vec<_> = [
            ("Kapacitás", capacity.as_deref()),
            ("Üzemidő", value(POWER_ON_TIME)),
            ("Eddig írt adat", value(HOST_WRITTEN)),
            ("Állapot", remaining.as_deref()),
        ].into_iter().filter_map(|(name, value)| Some((name, value?))).collect();
        let status = match self.health.condition {
            Condition::Passed => "Jó állapot",
            Condition::MediaWarnings | Condition::LowEndurance => "Érdemes ellenőrizni",
            Condition::Critical => "Kritikus állapot",
            Condition::Unavailable => "Az állapot ismeretlen",
        };
        let reason = self.report.health_reason(&self.health).map(|reason| {
            match reason.strip_suffix(" függőben lévő szektor") {
                Some(count) => format!("{count} lemezterület ellenőrzésre vár."),
                None => reason,
            }
        });
        sheet::render(&sheet::Sheet {
            model: value("Modell").unwrap_or("Meghajtó"),
            condition: self.health.condition,
            status,
            reason: reason.as_deref(),
            metrics: &metrics,
            measured: &date_label(self.measured),
            serial: include_serial.then(|| value("Sorozatszám")).flatten(),
        })
    }
}

enum FileAction {
    Save(Vec<u8>),
    Import,
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
    history_trend: Object,
    history_trend_note: Object,
    sheet_button: Object,
    import_measurement: Object,
    export_measurement: Object,
    export_serial: Object,
    history_list: Object,
    history_controls: Object,
    history_pickers: [Object; 2],
    history_all: Object,
    history_samples: Vec<Snapshot>,
    diagnostics: Object,
    health_panel: Object,
    health_condition: Object,
    health_endurance: Object,
    health_reason: Object,
    health_bar: Object,
    written_label: Object,
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
    export_dialog: Option<(Object, FileAction)>,
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
        gtk_widget_set_visible(self.health_panel, self.selected().is_some().into());
        self.show_health(None);
        label_text(self.written_label, "–");
        label_text(self.power_on_time_label, "–");
        if self.last_temperature.is_some() {
            self.show_last_temperature();
        } else {
            label_text(self.temperature_label, "–");
        }
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

    unsafe fn show_history(&mut self) {
        let measurement = self.measurement();
        let current = measurement.as_ref().and_then(|item| item.snapshot.as_ref());
        let identity = current.map(|item| item.drive.clone())
            .or_else(|| self.selected().and_then(DriveId::from_disk));
        let saved = identity.as_ref().map(|id| self.history.for_drive(id)).unwrap_or_default();
        let baseline = current.and_then(|current| {
            saved.iter().rev().find(|item| item.measured < current.measured).cloned()
        });
        self.update_history_pickers(saved.iter().rev().take(50).cloned().collect());
        let (Some(current), Some(previous)) = (current, baseline) else {
            gtk_widget_set_visible(self.changes_card, 0);
            return;
        };
        gtk_widget_set_visible(self.changes_card, 1);
        let changes = current.changes(&previous);
        let mut lines = vec![format!("Előző mentés · {}", date_label(previous.measured))];
        if changes.is_empty() {
            lines.push(if current.comparable_to(&previous) { "Nincs lényeges változás." }
                else { "Nincs összehasonlítható SMART-adat." }.into());
        }
        for (line, alert) in changes {
            let line = line.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
            lines.push(if alert { format!("<b>⚠ {line}</b>") } else { line });
        }
        gtk_label_set_markup(self.changes, c_text(&lines.join("\n")).as_ptr());
    }

    unsafe fn update_history_pickers(&mut self, samples: Vec<Snapshot>) {
        let previous_picks = self.history_pickers.map(|picker| gtk_combo_box_get_active(picker));
        let previous_times = previous_picks.map(|index| {
            self.history_samples.get(index as usize).map(|snapshot| snapshot.measured)
        });
        let same_drive = matches!((self.history_samples.first(), samples.first()),
            (Some(previous), Some(current)) if previous.drive == current.drive);
        self.history_samples = samples;
        let has_pair = self.history_samples.len() >= 2;
        gtk_widget_set_visible(self.history_trend, has_pair.into());
        gtk_widget_set_visible(self.history_trend_note, has_pair.into());
        if has_pair {
            let latest = self.history_samples.windows(2).find(|pair| pair[0].worsened_since(&pair[1]));
            let note = latest.map(|pair| format!("Utolsó jelzett romlás · {}", date_label(pair[0].measured)))
                .unwrap_or_else(|| "Nincs észlelt romlás a mentett mérések között.".into());
            label_text(self.history_trend_note, &note);
            gtk_widget_queue_draw(self.history_trend);
        }
        gtk_widget_set_visible(self.history_controls, has_pair.into());
        let labels: Vec<_> = self.history_samples.iter().enumerate().map(|(index, snapshot)| {
            let time = date_label(snapshot.measured);
            let second = snapshot.measured / 1_000_000_000;
            let same_second = |index| self.history_samples.get(index).is_some_and(
                |other: &Snapshot| other.measured / 1_000_000_000 == second
            );
            if index.checked_sub(1).is_some_and(same_second) || same_second(index + 1) {
                format!("{time} · {}", self.history_samples.len() - index)
            } else {
                time
            }
        }).collect();
        for (column, picker) in self.history_pickers.into_iter().enumerate() {
            gtk_combo_box_text_remove_all(picker);
            if has_pair {
                for text in &labels {
                    gtk_combo_box_text_append_text(picker, c_text(text).as_ptr());
                }
                let default = 1 - column;
                let index = if same_drive && previous_picks != [1, 0] {
                    previous_times[column].and_then(|time| {
                        self.history_samples.iter().position(|item| item.measured == time)
                    }).unwrap_or(default)
                } else {
                    default
                };
                gtk_combo_box_set_active(picker, index as c_int);
            }
        }
        self.render_history_comparison();
    }

    unsafe fn render_history_comparison(&self) {
        clear_list(self.history_list);
        if let Some(issue) = &self.history_issue {
            self.add_history_row(issue, true);
        }
        if self.history_samples.is_empty() {
            self.add_history_row("Nincs mentett mérés.", false);
            return;
        }
        if self.history_samples.len() == 1 {
            let snapshot = &self.history_samples[0];
            self.add_history_row(&format!("{} · {}", date_label(snapshot.measured), snapshot.condition.label()), false);
            if let Some(percent) = snapshot.remaining {
                self.add_history_row(&format!("Állapot · {percent}%"), false);
            }
            for (field, value) in &snapshot.values {
                self.add_history_row(&format!("{field} · {value}"), false);
            }
            return;
        }
        let [left, right] = self.history_pickers.map(|picker| {
            self.history_samples.get(gtk_combo_box_get_active(picker) as usize)
        });
        let (Some(left), Some(right)) = (left, right) else { return };
        if std::ptr::eq(left, right) {
            self.add_history_row("Azonos mérés.", false);
            return;
        }
        let show_all = gtk_check_button_get_active(self.history_all) != 0;
        let reversed = left.measured > right.measured;
        let (before, after) = if reversed { (right, left) } else { (left, right) };
        let changes = after.comparison(before, show_all);
        if changes.is_empty() {
            let message = if after.comparable_to(before) { "Nincs változás." }
                else { "Nincs összehasonlítható SMART-adat." };
            self.add_history_row(message, false);
        }
        for change in changes {
            let (left, right) = if reversed { (&change.after, &change.before) }
                else { (&change.before, &change.after) };
            let row = gtk_box_new(0, 8);
            gtk_box_set_homogeneous(row, 1);
            margins(row, 10);
            for value in [change.field, left, right] {
                let item = label(value);
                gtk_label_set_selectable(item, 1);
                if change.warning { css(item, "roadmap-warning"); }
                gtk_box_append(row, item);
            }
            gtk_list_box_append(self.history_list, row);
            gtk_list_box_row_set_activatable(gtk_widget_get_parent(row), 0);
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

    unsafe fn show_health(&self, measurement: Option<&Measurement>) {
        let health = measurement.map(|measurement| &measurement.health);
        let (color, condition) = match health.map(|health| health.condition) {
            Some(Condition::Passed) => ("#26935d", "Jó"),
            Some(Condition::MediaWarnings | Condition::LowEndurance) => ("#ba810a", "Figyelmeztetés"),
            Some(Condition::Critical) => ("#d74b4b", "Kritikus"),
            Some(Condition::Unavailable) => ("#808080", "Ismeretlen"),
            None => ("#808080", "–"),
        };
        let badge = format!("<span foreground=\"{color}\">●</span>  <b>{condition}</b>");
        gtk_label_set_markup(self.health_condition, c_text(&badge).as_ptr());
        let reason = measurement.and_then(|item| item.report.health_reason(&item.health));
        if let Some(reason) = &reason { label_text(self.health_reason, reason); }
        gtk_widget_set_visible(self.health_reason, reason.is_some().into());
        let remaining = health.and_then(|health| health.remaining);
        if let Some(percent) = remaining {
            let text = format!("Állapot · {percent}%");
            label_text(self.health_endurance, &text);
            gtk_progress_bar_set_fraction(self.health_bar, f64::from(percent) / 100.0);
        }
        gtk_widget_set_visible(self.health_endurance, remaining.is_some().into());
        gtk_widget_set_visible(self.health_bar, remaining.is_some().into());
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
        let snapshot = measurement.as_ref().and_then(|item| item.snapshot.as_ref());
        gtk_widget_set_sensitive(self.sheet_button, (!busy && measurement.is_some()).into());
        gtk_widget_set_sensitive(self.import_measurement, (!busy && snapshot.is_some()).into());
        gtk_widget_set_sensitive(self.export_measurement, (!busy && snapshot.is_some()).into());
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
                        label_text(self.temperature_label, &value);
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
            Some(value) => format!("{value} (utolsó ismert)"),
            None => "–".into(),
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
                let save_error = measurement.snapshot.as_ref().and_then(|snapshot| {
                    self.history.save(snapshot.clone()).err()
                });
                if let Some(value) = report.temperature() {
                    self.last_temperature = Some(value);
                    self.show_last_temperature();
                    self.next_temperature = Instant::now();
                }
                self.measurements.insert(self.disks[index].path.clone(), measurement.clone());
                self.show_disk_metrics(index, None);
                self.rendered_tabs = [false; 3];
                buffer_text(self.full, "");
                buffer_text(self.json, "");
                buffer_text(self.diagnostics, "");
                self.show_measurement(&measurement);
                self.show_history();
                if let Some(error) = save_error {
                    label_text(self.status, &format!("A mérés nem menthető. {error}"));
                }
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
                HOST_WRITTEN => label_text(self.written_label, value),
                POWER_ON_TIME => label_text(self.power_on_time_label, value),
                TEMPERATURE => (),
                _ => self.add_row(name, value),
            }
        }
        self.show_health(Some(measurement));
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

    unsafe fn choose_file(&mut self, button: Object, data: Object) {
        if self.export_dialog.is_some() { return }
        let Some(measurement) = self.measurement() else { return };
        let include_serial = gtk_check_button_get_active(self.export_serial) != 0;
        let (title, name, action) = if button == self.import_measurement {
            ("Mérés importálása", None, Ok(FileAction::Import))
        } else if button == self.export_measurement {
            let Some(snapshot) = &measurement.snapshot else { return };
            ("Mérés exportálása", Some("roadmap-meres.json"), snapshot.portable().map(FileAction::Save))
        } else if button == self.sheet_button {
            ("Mérési lap mentése", Some("roadmap-meresi-lap.png"), measurement.png(include_serial).map(FileAction::Save))
        } else {
            return;
        };
        let action = match action {
            Ok(action) => action,
            Err(error) => { label_text(self.status, &error.to_string()); return }
        };
        let (mode, accept) = if name.is_some() { (FILE_CHOOSER_SAVE, "Mentés") }
            else { (FILE_CHOOSER_OPEN, "Importálás") };
        let dialog = gtk_file_chooser_native_new(c_text(title).as_ptr(), self.window, mode,
            c_text(accept).as_ptr(), c_text("Mégse").as_ptr());
        if let Some(name) = name {
            gtk_file_chooser_set_current_name(dialog, c_text(name).as_ptr());
        }
        gtk_native_dialog_set_modal(dialog, 1);
        connect(dialog, "response", std::mem::transmute::<unsafe extern "C" fn(Object, c_int, Object), Callback>(exported), data);
        self.export_dialog = Some((dialog, action));
        gtk_native_dialog_show(dialog);
    }

    unsafe fn finish_export(&mut self, response: c_int) {
        let Some((dialog, action)) = self.export_dialog.take() else { return };
        if response == RESPONSE_ACCEPT {
            let result = export_path(dialog).and_then(|path| match action {
                FileAction::Save(bytes) => {
                    save_file(&path, &bytes)?;
                    Ok(format!("Mentve · {}", path.display()))
                }
                FileAction::Import => self.import_file(&path),
            });
            let message = match result {
                Ok(message) => message,
                Err(error) => format!("{error}"),
            };
            label_text(self.status, &message);
        }
        gtk_native_dialog_destroy(dialog);
        g_object_unref(dialog);
    }

    unsafe fn import_file(&mut self, path: &std::path::Path) -> Result<String> {
        const MAX_BYTES: u64 = 65_536;
        let Some(current) = self.measurement().and_then(|item| item.snapshot.clone()) else {
            return Err("Nincs azonosítható SMART-mérés.".into());
        };
        if !fs::metadata(path)?.is_file() {
            return Err("Az importhoz normál fájlt válassz.".into());
        }
        let mut bytes = Vec::new();
        fs::File::open(path)?.take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err("A mérés fájlja túl nagy.".into());
        }
        let snapshot = Snapshot::from_portable(&bytes, &current)?;
        if snapshot.measured == current.measured || self.history.for_drive(&snapshot.drive)
            .iter().any(|saved| saved.measured == snapshot.measured)
        {
            return Ok("Ez a mérés már megvan.".into());
        }
        self.history.save(snapshot)?;
        self.show_history();
        self.set_busy(false);
        Ok("Mérés importálva.".into())
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

fn save_file(path: &std::path::Path, bytes: &[u8]) -> Result<()> {
    let mut file = match OpenOptions::new().write(true).create_new(true).mode(0o600).open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err("A fájl már létezik. Válassz új nevet.".into())
        }
        Err(error) => return Err(error.into()),
    };
    if let Err(error) = file.write_all(bytes).and_then(|()| file.sync_all()) {
        let _ = fs::remove_file(path);
        return Err(error.into());
    }
    Ok(())
}

unsafe fn export_path(dialog: Object) -> Result<PathBuf> {
    let file = gtk_file_chooser_get_file(dialog);
    if file.is_null() { return Err("Nincs kiválasztva fájl.".into()) }
    let path = g_file_get_path(file);
    g_object_unref(file);
    if path.is_null() { return Err("Helyi fájlt válassz.".into()) }
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

unsafe extern "C" fn draw_history(_: Object, canvas: Object, width: c_int, height: c_int, data: Object) {
    let state = &*data.cast::<RefCell<Desktop>>();
    if let Ok(desktop) = state.try_borrow() {
        trend::draw(canvas, width, height, &desktop.history_samples);
    }
}

unsafe extern "C" fn history_tooltip(widget: Object, x: c_int, _: c_int, keyboard: c_int,
    tooltip: Object, data: Object) -> c_int {
    let state = &*data.cast::<RefCell<Desktop>>();
    let Ok(desktop) = state.try_borrow() else { return 0 };
    let Some(text) = trend::tooltip(&desktop.history_samples, gtk_widget_get_width(widget),
        (keyboard == 0).then_some(x)) else { return 0 };
    gtk_tooltip_set_text(tooltip, c_text(&text).as_ptr());
    1
}

unsafe extern "C" fn history_comparison_changed(_: Object, data: Object) {
    with_desktop(data, |desktop| desktop.render_history_comparison());
}

unsafe extern "C" fn choose_file(button: Object, data: Object) {
    with_desktop(data, |desktop| desktop.choose_file(button, data));
}

unsafe extern "C" fn exported(_: Object, response: c_int, data: Object) {
    with_desktop(data, |desktop| desktop.finish_export(response));
}

unsafe extern "C" fn close(_: Object, data: Object) -> c_int {
    g_main_loop_quit(data);
    // a jeleket az ablak felszabaditasa elott bontjuk
    1
}

unsafe fn connect(widget: Object, signal: &str, handler: Callback, data: Object) -> (Object, c_ulong) {
    let id = g_signal_connect_data(widget, c_text(signal).as_ptr(), handler, data, None, 0);
    (widget, id)
}

pub fn run() -> Result<()> {
    // a gtk visszahivasai a fo szalon futnak
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
            history_trend: widget("history_trend"),
            history_trend_note: widget("history_trend_note"),
            sheet_button: widget("export_sheet"),
            import_measurement: widget("import_measurement"),
            export_measurement: widget("export_measurement"),
            export_serial: widget("export_serial"),
            history_list: widget("history_list"),
            history_controls: widget("history_controls"),
            history_pickers: [widget("history_before"), widget("history_after")],
            history_all: widget("history_all"),
            history_samples: Vec::new(),
            diagnostics: widget("diagnostics"),
            health_panel: widget("health_panel"),
            health_condition: widget("health_condition"),
            health_endurance: widget("health_endurance"),
            health_reason: widget("health_reason"),
            health_bar: widget("health_bar"),
            written_label: widget("written"),
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
        gtk_drawing_area_set_draw_func(widget("history_trend"), Some(draw_history), data, None);
        let handlers = [
            connect(refresh_button, "clicked", std::mem::transmute::<unsafe extern "C" fn(Object, Object), Callback>(refresh), data),
            connect(read_button, "clicked", std::mem::transmute::<unsafe extern "C" fn(Object, Object), Callback>(read), data),
            connect(disks_list, "row-selected", std::mem::transmute::<unsafe extern "C" fn(Object, Object, Object), Callback>(selected), data),
            connect(adapter, "changed", std::mem::transmute::<unsafe extern "C" fn(Object, Object), Callback>(adapter_changed), data),
            connect(widget("notebook"), "switch-page", std::mem::transmute::<unsafe extern "C" fn(Object, Object, u32, Object), Callback>(tab_changed), data),
            connect(widget("history_trend"), "query-tooltip", std::mem::transmute::<unsafe extern "C" fn(Object, c_int, c_int, c_int, Object, Object) -> c_int, Callback>(history_tooltip), data),
            connect(widget("history_before"), "changed", std::mem::transmute::<unsafe extern "C" fn(Object, Object), Callback>(history_comparison_changed), data),
            connect(widget("history_after"), "changed", std::mem::transmute::<unsafe extern "C" fn(Object, Object), Callback>(history_comparison_changed), data),
            connect(widget("history_all"), "toggled", std::mem::transmute::<unsafe extern "C" fn(Object, Object), Callback>(history_comparison_changed), data),
            connect(widget("export_sheet"), "clicked", std::mem::transmute::<unsafe extern "C" fn(Object, Object), Callback>(choose_file), data),
            connect(widget("import_measurement"), "clicked", std::mem::transmute::<unsafe extern "C" fn(Object, Object), Callback>(choose_file), data),
            connect(widget("export_measurement"), "clicked", std::mem::transmute::<unsafe extern "C" fn(Object, Object), Callback>(choose_file), data),
            connect(window, "close-request", std::mem::transmute::<unsafe extern "C" fn(Object, Object) -> c_int, Callback>(close), main_loop),
        ];
        state.borrow_mut().schedule(data);

        gtk_window_present(window);
        g_main_loop_run(main_loop);

        for (object, id) in handlers {
            g_signal_handler_disconnect(object, id);
        }
        gtk_drawing_area_set_draw_func(widget("history_trend"), None, ptr::null_mut(), None);
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
    fn measurement_sheet_is_a_compact_png() {
        let measurement = Measurement::new(Report {
            data: json!({
                "model_name": "SSD test", "serial_number": "SECRET-SERIAL",
                "smart_status": {"passed": true},
                "smartctl": {"output": ["Serial Number: SECRET-SERIAL"]}
            }),
            code: 0, path: "/dev/test".into(), disk: None,
        });
        let hidden = measurement.png(false).unwrap();
        let with_serial = measurement.png(true).unwrap();
        assert!(hidden.starts_with(b"\x89PNG\r\n\x1a\n"));
        assert!(with_serial.starts_with(b"\x89PNG\r\n\x1a\n"));
        let height = |bytes: &[u8]| u32::from_be_bytes(bytes[20..24].try_into().unwrap());
        assert!(height(&hidden) < 400);
        assert!(height(&with_serial) > height(&hidden));
    }

    #[test]
    fn export_requires_a_new_file_and_preserves_existing_data() {
        let directory = env::temp_dir().join(format!("roadmap-export-{}-{}", std::process::id(), history::now()));
        fs::create_dir(&directory).unwrap();
        let target = directory.join("measurement.json");
        save_file(&target, b"original").unwrap();
        let overwrite = save_file(&target, b"replacement");
        let contents = fs::read(&target).unwrap();
        fs::remove_dir_all(directory).unwrap();
        assert!(overwrite.is_err());
        assert_eq!(contents, b"original");
    }
}
