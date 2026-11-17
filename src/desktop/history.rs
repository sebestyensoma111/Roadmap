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

pub fn now() -> u64 {
    u64::try_from(SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos())
        .unwrap_or(u64::MAX)
}

impl Snapshot {
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
        let mut changes = Vec::new();
        if self.condition != previous.condition && self.condition != Condition::Unavailable
            && previous.condition != Condition::Unavailable {
            changes.push((format!("Kondíció: {} → {}", previous.condition.label(), self.condition.label()),
                matches!(self.condition, Condition::Critical | Condition::MediaWarnings | Condition::LowEndurance)));
        }
        if let (Some(before), Some(after)) = (previous.remaining, self.remaining) {
            if before != after {
                changes.push((format!("Hátralévő élettartam: {before}% → {after}%"), after < before && after <= 10));
            }
        }
        for field in ERROR_FIELDS.iter().chain(MEASUREMENT_FIELDS) {
            let (Some(before), Some(after)) = (previous.values.get(*field), self.values.get(*field)) else { continue };
            if before == after { continue }
            let increased = if ERROR_FIELDS.contains(field) {
                match (before.parse::<u128>(), after.parse::<u128>()) {
                    (Ok(before), Ok(after)) => after > before,
                    _ => false,
                }
            } else { false };
            changes.push((format!("{field}: {before} → {after}"), increased));
        }
        changes.sort_by_key(|(_, warning)| !warning);
        changes
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
        assert_eq!(second.changes(&first), vec![("NVMe médiahibák: 2 → 8".into(), true)]);
        assert_eq!(first.changes(&second), vec![("NVMe médiahibák: 8 → 2".into(), false)]);
        assert!(snapshot("01A", 3, None).changes(&second).is_empty());
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
