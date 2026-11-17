use super::data_home;
use crate::devices::{Condition, Disk, Health, Result, HOST_READ, HOST_WRITTEN, POWER_ON_TIME, TEMPERATURE};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::{self, DirBuilder, OpenOptions},
    io::Write,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

const ERROR_FIELDS: &[&str] = &[
    "Áthelyezett szektorok", "Függőben lévő szektorok", "Javíthatatlan ATA szektorok",
    "NVMe médiahibák", "SCSI utólagos hibák", "SCSI javíthatatlan olvasási hibák",
    "SCSI javíthatatlan írási hibák",
];
const MEASUREMENT_FIELDS: &[&str] = &[TEMPERATURE, POWER_ON_TIME, HOST_WRITTEN, HOST_READ];

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct DriveId {
    serial: String,
    model: String,
    size: u64,
}

impl DriveId {
    pub fn matches_drive(&self, other: &Self) -> bool {
        self.serial == other.serial && self.size != 0 && self.size == other.size
    }
    pub fn from_disk(disk: &Disk) -> Option<Self> {
        Self::new(disk.serial.as_deref()?, disk.model.as_deref().unwrap_or_default(), disk.size)
    }

    pub fn new(serial: &str, model: &str, size: u64) -> Option<Self> {
        let serial = serial.trim();
        if serial.is_empty() || serial.chars().all(|ch| ch == '0')
            || ["-", "unknown", "none", "n/a"].iter().any(|value| serial.eq_ignore_ascii_case(value))
        {
            return None;
        }
        Some(Self { serial: serial.into(), model: model.trim().into(), size })
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub drive: DriveId,
    pub measured: u64,
    pub condition: Condition,
    pub remaining: Option<u8>,
    pub values: BTreeMap<String, String>,
}

#[derive(Serialize, Deserialize)]
struct PortableMeasurement {
    version: u8,
    measurement: Snapshot,
}

pub struct Difference {
    pub field: &'static str,
    pub before: String,
    pub after: String,
    pub warning: bool,
}

pub fn now() -> u64 {
    u64::try_from(SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos())
        .unwrap_or(u64::MAX)
}

impl Snapshot {
    pub fn severity(&self) -> Option<u8> {
        match self.condition {
            Condition::Passed => Some(0),
            Condition::MediaWarnings | Condition::LowEndurance => Some(1),
            Condition::Critical => Some(2),
            Condition::Unavailable => None,
        }
    }

    pub fn worsened_since(&self, previous: &Self) -> bool {
        matches!((previous.severity(), self.severity()), (Some(old), Some(new)) if new > old)
            || matches!((previous.remaining, self.remaining), (Some(old), Some(new)) if new < old)
            || ERROR_FIELDS.iter().any(|field| {
                let value = |sample: &Self| sample.values.get(*field).and_then(|value| value.parse::<u128>().ok());
                matches!((value(previous), value(self)), (Some(old), Some(new)) if new > old)
            })
    }
    pub fn portable(&self) -> Result<Vec<u8>> {
        if self.drive.size == 0 {
            return Err("A méréshez hiányzik a meghajtó kapacitása.".into());
        }
        Ok(serde_json::to_vec_pretty(&PortableMeasurement { version: 1, measurement: self.clone() })?)
    }

    pub fn from_portable(bytes: &[u8], current: &Self) -> Result<Self> {
        let file: PortableMeasurement = serde_json::from_slice(bytes)
            .map_err(|_| "Érvénytelen Roadmap-mérés.")?;
        if file.version != 1 {
            return Err("A mérés fájlverziója nem támogatott.".into());
        }
        let mut sample = file.measurement;
        if !sample.drive.matches_drive(&current.drive) {
            return Err("A fájl másik meghajtóhoz tartozik.".into());
        }
        if sample.measured == 0 || sample.remaining.is_some_and(|value| value > 100)
            || sample.values.iter().any(|(field, value)| {
                !(ERROR_FIELDS.contains(&field.as_str()) || MEASUREMENT_FIELDS.contains(&field.as_str()))
                    || value.len() > 96 || value.chars().any(char::is_control)
            })
            || (sample.values.is_empty() && sample.remaining.is_none() && sample.condition == Condition::Unavailable)
        {
            return Err("A mérés adatai érvénytelenek.".into());
        }
        sample.drive = current.drive.clone();
        Ok(sample)
    }

    pub fn new(drive: DriveId, measured: u64, health: &Health, rows: &[(&str, Option<String>)]) -> Self {
        let values = rows.iter().filter_map(|(label, value)| {
            if !ERROR_FIELDS.contains(label) && !MEASUREMENT_FIELDS.contains(label) {
                return None;
            }
            Some(((*label).into(), value.as_ref()?.clone()))
        }).collect();
        Self { drive, measured, condition: health.condition, remaining: health.remaining, values }
    }

    pub fn comparable_to(&self, previous: &Self) -> bool {
        (self.condition != Condition::Unavailable && previous.condition != Condition::Unavailable)
            || (self.remaining.is_some() && previous.remaining.is_some())
            || self.values.keys().any(|field| previous.values.contains_key(field))
    }

    pub fn changes(&self, previous: &Self) -> Vec<(String, bool)> {
        self.comparison(previous, false).into_iter().filter_map(|change| {
            let growth = |unit: &str| {
                let before: u128 = change.before.trim_end_matches(unit).parse().ok()?;
                let after: u128 = change.after.trim_end_matches(unit).parse().ok()?;
                after.checked_sub(before).filter(|value| *value > 0)
            };
            let text = match change.field {
                "Kondíció" => format!("Új állapot: {}.", change.after),
                "Állapot" => format!("Az állapot most {}.", change.after),
                POWER_ON_TIME => format!("{} órával többet üzemelt.", growth(" óra")?),
                HOST_WRITTEN => format!("Az összes írt adat most {}.", change.after),
                "Függőben lévő szektorok" => format!("{} szektorral több vár ellenőrzésre.", growth("")?),
                "Áthelyezett szektorok" => format!("{} új áthelyezett szektor.", growth("")?),
                "NVMe médiahibák" => format!("{} új médiahiba.", growth("")?),
                "Javíthatatlan ATA szektorok" => format!("{} új javíthatatlan szektor.", growth("")?),
                "SCSI utólagos hibák" => format!("{} újonnan észlelt lemezhiba.", growth("")?),
                "SCSI javíthatatlan olvasási hibák" => format!("{} új javíthatatlan olvasási hiba.", growth("")?),
                "SCSI javíthatatlan írási hibák" => format!("{} új javíthatatlan írási hiba.", growth("")?),
                _ => return None,
            };
            Some((text, change.warning))
        }).collect()
    }

    pub fn comparison(&self, previous: &Self, show_all: bool) -> Vec<Difference> {
        let mut rows = Vec::new();
        let mut add = |field, before: Option<String>, after: Option<String>, warning| {
            let changed = matches!((&before, &after), (Some(before), Some(after)) if before != after);
            if changed || (show_all && (before.is_some() || after.is_some())) {
                rows.push(Difference {
                    field,
                    before: before.unwrap_or_else(|| "–".into()),
                    after: after.unwrap_or_else(|| "–".into()),
                    warning: changed && warning,
                });
            }
        };
        let condition = |condition: Condition| (condition != Condition::Unavailable).then(|| condition.label().into());
        add("Kondíció", condition(previous.condition), condition(self.condition), matches!(
            self.condition, Condition::Critical | Condition::MediaWarnings | Condition::LowEndurance
        ));
        add("Állapot",
            previous.remaining.map(|value| format!("{value}%")), self.remaining.map(|value| format!("{value}%")),
            matches!((previous.remaining, self.remaining), (Some(before), Some(after)) if after < before && after <= 10));
        for &field in ERROR_FIELDS.iter().chain(MEASUREMENT_FIELDS) {
            let before = previous.values.get(field);
            let after = self.values.get(field);
            let count = |value: Option<&String>| value.and_then(|value| value.parse::<u128>().ok());
            let increased = ERROR_FIELDS.contains(&field)
                && matches!((count(before), count(after)), (Some(before), Some(after)) if after > before);
            add(field, before.cloned(), after.cloned(), increased);
        }
        rows.sort_by_key(|row| !row.warning);
        rows
    }
}

#[derive(Default)]
pub struct History {
    drives: BTreeMap<DriveId, Vec<Snapshot>>,
}

impl History {
    pub fn load() -> Result<(Self, usize)> {
        let directory = data_home()?.join("roadmap/history");
        Self::load_from(&directory)
    }

    fn load_from(directory: &Path) -> Result<(Self, usize)> {
        let files = match fs::read_dir(directory) {
            Ok(files) => files,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok((Self::default(), 0)),
            Err(error) => return Err(error.into()),
        };
        let mut history = Self::default();
        let mut unreadable = 0;
        for file in files {
            let Ok(file) = file else { unreadable += 1; continue };
            if file.path().extension().is_some_and(|extension| extension == "json")
                && file.file_type().is_ok_and(|kind| kind.is_file())
            {
                let snapshot = fs::read(file.path()).ok().and_then(|bytes| serde_json::from_slice::<Snapshot>(&bytes).ok());
                if let Some(snapshot) = snapshot {
                    history.drives.entry(snapshot.drive.clone()).or_default().push(snapshot);
                } else {
                    unreadable += 1;
                }
            }
        }
        for values in history.drives.values_mut() {
            values.sort_by_key(|snapshot| snapshot.measured);
        }
        Ok((history, unreadable))
    }

    pub fn for_drive(&self, drive: &DriveId) -> &[Snapshot] {
        self.drives.get(drive).map(Vec::as_slice).unwrap_or_default()
    }

    pub fn save(&mut self, snapshot: Snapshot) -> Result<()> {
        let directory = data_home()?.join("roadmap/history");
        self.save_to(&directory, snapshot)
    }

    fn save_to(&mut self, directory: &Path, snapshot: Snapshot) -> Result<()> {
        DirBuilder::new().recursive(true).mode(0o700).create(directory)?;
        let name = format!("{}-{}.json", snapshot.measured, std::process::id());
        write_new(&directory.join(name), &serde_json::to_vec(&snapshot)?)?;
        let entries = self.drives.entry(snapshot.drive.clone()).or_default();
        let position = entries.partition_point(|item| item.measured < snapshot.measured);
        entries.insert(position, snapshot);
        Ok(())
    }
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    let directory = path.parent().ok_or("A célmappa nem található.")?;
    let temporary = directory.join(format!(".roadmap-{}-{}.tmp", std::process::id(), now()));
    let mut file = OpenOptions::new().write(true).create_new(true).mode(0o600).open(&temporary)?;
    let result = file.write_all(bytes).and_then(|()| file.sync_all())
        .and_then(|()| fs::hard_link(&temporary, path));
    let _ = fs::remove_file(temporary);
    result.map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(serial: &str, measured: u64, count: Option<&str>) -> Snapshot {
        let drive = DriveId::new(serial, "Tesztmodell", 4096).unwrap();
        let health = Health { condition: Condition::Passed, remaining: None };
        Snapshot::new(drive, measured, &health, &[("NVMe médiahibák", count.map(str::to_string))])
    }

    #[test]
    fn warnings_require_an_increase_in_a_comparable_error_counter() {
        let first = snapshot("01A", 1, Some("2"));
        let second = snapshot("01A", 2, Some("8"));
        assert_eq!(second.changes(&first), vec![("6 új médiahiba.".into(), true)]);
        assert!(first.changes(&second).is_empty());
        assert!(snapshot("01A", 3, None).changes(&second).is_empty());
        assert!(second.worsened_since(&first));
        assert!(!first.worsened_since(&second));
        assert!(!second.worsened_since(&snapshot("01A", 1, None)));
    }

    #[test]
    fn trend_marks_wear_and_condition_declines_without_inventing_unknown_status() {
        let mut first = snapshot("01A", 1, None);
        first.remaining = Some(95);
        let mut second = first.clone();
        second.remaining = Some(94);
        assert!(second.worsened_since(&first));
        second.remaining = None;
        second.condition = Condition::Unavailable;
        assert!(!second.worsened_since(&first));
        second.condition = Condition::Critical;
        assert!(second.worsened_since(&first));
    }

    #[test]
    fn saved_comparison_keeps_values_in_columns_and_hides_unchanged_by_default() {
        let first = snapshot("01A", 1, Some("2"));
        let second = snapshot("01A", 2, Some("8"));
        let changed = second.comparison(&first, false);
        assert_eq!(changed.len(), 1);
        assert_eq!((&*changed[0].before, &*changed[0].after), ("2", "8"));
        assert!(changed[0].warning);
        let all = snapshot("01A", 3, None).comparison(&first, true);
        assert!(all.iter().any(|row| row.before == "2" && row.after == "–" && !row.warning));
        assert!(all.iter().any(|row| row.field == "Kondíció" && row.before == row.after));
    }

    #[test]
    fn portable_measurement_matches_serial_and_capacity_even_with_different_model_labels() {
        let mut first = snapshot("01A", 1, Some("2"));
        first.drive.model = "USB adapter".into();
        let bytes = first.portable().unwrap();
        let second = snapshot("01A", 2, Some("8"));
        let imported = Snapshot::from_portable(&bytes, &second).unwrap();
        assert!(imported.drive == second.drive);
        assert_eq!(imported.measured, 1);
        assert!(Snapshot::from_portable(&bytes, &snapshot("OTHER", 2, None)).is_err());
    }

    #[test]
    fn import_rejects_unsupported_versions_and_invalid_measurements() {
        let current = snapshot("01A", 2, Some("8"));
        let original = snapshot("01A", 1, Some("2"));
        let original: serde_json::Value = serde_json::from_slice(&original.portable().unwrap()).unwrap();
        let rejects = |file: &serde_json::Value| {
            assert!(Snapshot::from_portable(&serde_json::to_vec(file).unwrap(), &current).is_err());
        };
        let mut file = original.clone();
        file["version"] = 2.into();
        rejects(&file);

        let mut file = original.clone();
        file["measurement"]["drive"]["size"] = 8192.into();
        rejects(&file);

        let mut file = original.clone();
        file["measurement"]["remaining"] = 101.into();
        rejects(&file);

        let mut file = original;
        file["measurement"]["values"]["NVMe médiahibák"] = "2\n3".into();
        rejects(&file);
    }

    #[test]
    fn stored_snapshots_reload_per_drive_and_never_overwrite_existing_files() {
        let directory = std::env::temp_dir().join(format!("roadmap-test-{}-{}", std::process::id(), now()));
        let mut history = History::default();
        let first = snapshot("01A", 1, Some("2"));
        let second = snapshot("02B", 2, Some("8"));
        history.save_to(&directory, first.clone()).unwrap();
        history.save_to(&directory, second.clone()).unwrap();
        assert!(history.save_to(&directory, snapshot("01A", 1, Some("9"))).is_err());
        fs::write(directory.join("invalid.json"), "{").unwrap();
        let (loaded, invalid) = History::load_from(&directory).unwrap();
        fs::remove_dir_all(directory).unwrap();
        assert_eq!(invalid, 1);
        assert_eq!(loaded.for_drive(&first.drive)[0].values, first.values);
        assert_eq!(loaded.for_drive(&second.drive)[0].values, second.values);
    }
}
