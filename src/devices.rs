use serde::Deserialize;
use serde_json::Value;
use std::fs;
use std::os::unix::fs::FileTypeExt;
use std::path::Path;
use std::process::Command;

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[derive(Clone, Deserialize)]
pub struct Disk {
    pub path: String,
    #[serde(rename = "type")]
    kind: String,
    pub size: u64,
    pub tran: Option<String>,
    pub vendor: Option<String>,
    pub model: Option<String>,
    pub serial: Option<String>,
    pub rev: Option<String>,
    pub fstype: Option<String>,
    #[serde(default)]
    pub children: Vec<Disk>,
}

#[derive(Deserialize)]
struct DiskList {
    blockdevices: Vec<Disk>,
}

pub fn disks() -> Result<Vec<Disk>> {
    let output = Command::new("lsblk")
        .args([
            "--bytes",
            "--json",
            "--tree",
            "--output",
            "NAME,PATH,TYPE,SIZE,TRAN,VENDOR,MODEL,SERIAL,REV,FSTYPE",
        ])
        .output()?;
    if !output.status.success() {
        return Err(format!("lsblk: {}", String::from_utf8_lossy(&output.stderr)).into());
    }
    let list: DiskList = serde_json::from_slice(&output.stdout)?;
    Ok(list.blockdevices.into_iter().filter(|disk| disk.kind == "disk").collect())
}

pub fn clean(input: &str) -> String {
    input.chars().map(|ch| if ch.is_control() { ' ' } else { ch }).collect()
}

impl Disk {
    pub fn capacity_label(&self) -> String {
        format!("{:.2} GiB", self.size as f64 / 1073741824.0)
    }

    pub fn label(&self) -> String {
        format!(
            "{}  ·  {}  ·  {}  ·  {}",
            self.path,
            self.capacity_label(),
            self.tran.as_deref().unwrap_or("–"),
            clean(self.model.as_deref().unwrap_or("ismeretlen modell").trim()),
        )
    }

    pub fn partitions(&self) -> impl Iterator<Item = String> + '_ {
        self.children.iter().filter(|child| child.kind == "part").map(|child| {
            format!(
                "{}  ·  {:.2} GiB  ·  {}",
                child.path,
                child.size as f64 / 1073741824.0,
                clean(child.fstype.as_deref().unwrap_or("–")),
            )
        })
    }
}

pub const USB_TYPES: &[&str] = &["sat", "sat,12", "sat,16", "scsi", "sntjmicron", "sntasmedia", "sntrealtek"];
pub const POWER_ON_TIME: &str = "Üzemidő";
pub const HOST_WRITTEN: &str = "Összes írt adat (host)";
pub const HOST_READ: &str = "Összes olvasott adat (host)";

pub struct Report {
    pub data: Value,
    pub code: i32,
    pub path: String,
    pub disk: Option<Disk>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Condition {
    Passed,
    MediaWarnings,
    LowEndurance,
    Critical,
    Unavailable,
}

impl Condition {
    pub fn label(self) -> &'static str {
        match self {
            Self::Passed => "Jó SMART-állapot",
            Self::MediaWarnings => "Figyelmeztetés · adathordozó-hibák",
            Self::LowEndurance => "Figyelmeztetés · alacsony hátralévő élettartam",
            Self::Critical => "Kritikus SMART-jelzés",
            Self::Unavailable => "SMART-állapot nem érhető el",
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct Health {
    pub condition: Condition,
    pub remaining: Option<u8>,
}

pub fn read_smart(path: &str, device_type: Option<&str>) -> Result<Report> {
    query_smart(path, device_type, false)
}

#[cfg(feature = "desktop")]
pub fn read_smart_desktop(path: &str, device_type: Option<&str>) -> Result<Report> {
    let report = read_smart(path, device_type)?;
    let permission_denied = report.code & 2 != 0 && report.data["smartctl"]["messages"]
        .as_array().into_iter().flatten().any(|message| {
            message["string"].as_str().is_some_and(|text| text.contains("Permission denied"))
        });
    if permission_denied {
        return query_smart(path, device_type, true);
    }
    Ok(report)
}

fn query_smart(path: &str, device_type: Option<&str>, elevation: bool) -> Result<Report> {
    let target = fs::canonicalize(path)?;
    let file_type = fs::metadata(&target)?.file_type();
    let disk = disks()?.into_iter().find(|disk| target.as_path() == Path::new(&disk.path));
    let controller = file_type.is_char_device()
        && target.file_name().is_some_and(|name| Path::new("/sys/class/nvme").join(name).is_dir());
    if !target.starts_with("/dev") || !(file_type.is_block_device() && disk.is_some() || controller) {
        return Err("Teljes meghajtót vagy NVMe-vezérlőt válassz a /dev alól.".into());
    }
    let mut command = Command::new("timeout");
    command.env("LC_ALL", "C");
    if elevation {
        let smartctl = ["/usr/sbin/smartctl", "/usr/bin/smartctl"]
            .into_iter().find(|path| Path::new(path).is_file())
            .ok_or("A smartctl rendszerparancs nem található.")?;
        command.args(["--kill-after=2s", "120s", "pkexec", "--disable-internal-agent", smartctl]);
    } else {
        command.args(["--kill-after=2s", "30s", "smartctl"]);
    }
    command.args(["--xall", "--json=ov"]);
    if let Some(device_type) = device_type {
        if !USB_TYPES.contains(&device_type) {
            return Err("Ismeretlen USB-adaptertípus.".into());
        }
        command.args(["-d", device_type]);
    }
    let output = command.arg(&target).output()?;
    let code = output.status.code().unwrap_or(1);
    let data = serde_json::from_slice(&output.stdout).map_err(|error| -> Box<dyn std::error::Error + Send + Sync> {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if matches!(code, 124 | 137) {
            "A SMART-lekérdezés túllépte az időkorlátot.".into()
        } else if elevation && code == 126 {
            "A hitelesítést megszakították.".into()
        } else if elevation && code == 127 {
            format!("A grafikus hitelesítés sikertelen. {}", clean(stderr.trim())).into()
        } else if stderr.trim().is_empty() {
            format!("A smartctl nem adott érvényes JSON-jelentést: {error}").into()
        } else {
            clean(stderr.trim()).into()
        }
    })?;
    Ok(Report { data, code, path: target.to_string_lossy().into_owned(), disk })
}

fn number(value: &Value) -> Option<u128> {
    match value {
        Value::Number(value) => value.to_string().parse().ok(),
        Value::String(value) => value.parse().ok(),
        _ => None,
    }
}

fn text(value: &Value) -> Option<String> {
    match value {
        Value::String(value) if !value.trim().is_empty() => Some(clean(value.trim())),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

fn amount(bytes: u128) -> String {
    format!("{bytes} B  ·  {:.2} GB  ·  {:.3} TB", bytes as f64 / 1e9, bytes as f64 / 1e12)
}

fn host_amount(bytes: u128) -> String {
    if bytes < 1_000_000_000_000 {
        format!("{:.2} GB", bytes as f64 / 1e9)
    } else {
        format!("{:.3} TB", bytes as f64 / 1e12)
    }
}

fn pci_vendor_name(id: u128) -> Option<String> {
    let prefix = format!("{id:04x}  ");
    let database = fs::read_to_string("/usr/share/hwdata/pci.ids")
        .or_else(|_| fs::read_to_string("/usr/share/misc/pci.ids")).ok()?;
    database.lines().find_map(|line| line.strip_prefix(&prefix).map(clean))
}

impl Report {
    fn ata_statistics(&self) -> impl Iterator<Item = &Value> {
        self.data["ata_device_statistics"]["pages"].as_array().into_iter().flatten()
            .filter_map(|page| page["table"].as_array()).flatten()
            .filter(|field| field["flags"]["valid"].as_bool() != Some(false))
    }

    pub fn health(&self) -> Health {
        let nvme = &self.data["nvme_smart_health_information_log"];
        let attributes = self.data["ata_smart_attributes"]["table"].as_array();
        let ata_value = |names: &[&str], field: &str| {
            attributes.into_iter().flatten()
                .filter(|attribute| attribute["name"].as_str().is_some_and(|name| names.contains(&name)))
                .find_map(|attribute| attribute.pointer(field).and_then(number))
        };
        let used = number(&nvme["percentage_used"])
            .or_else(|| self.ata_statistics()
                .find(|field| field["name"].as_str() == Some("Percentage Used Endurance Indicator"))
                .and_then(|field| number(&field["value"])))
            .or_else(|| ata_value(&["Percent_Lifetime_Used"], "/raw/value"));
        let remaining = used.filter(|used| *used <= 255)
            .map(|used| 100_u8.saturating_sub(used as u8))
            .or_else(|| {
                ata_value(&["Percent_Lifetime_Remain", "SSD_Life_Left", "Remaining_Lifetime_Perc"], "/value")
                    .filter(|value| *value <= 100).map(|value| value as u8)
            });
        let has_count = |value: &Value| number(value).is_some_and(|count| count > 0);
        let media_warning = has_count(&nvme["media_errors"])
            || has_count(&self.data["ata_pending_defects_log"]["count"])
            || attributes.into_iter().flatten().any(|attribute| {
                matches!(attribute["name"].as_str(), Some("Reallocated_Sector_Ct"
                    | "Current_Pending_Sector" | "Offline_Uncorrectable"))
                    && has_count(&attribute["raw"]["value"])
            })
            || self.ata_statistics().any(|field| {
                matches!(field["name"].as_str(), Some("Number of Reallocated Logical Sectors"
                    | "Number of Realloc. Candidate Logical Sectors" | "Number of Reported Uncorrectable Errors"))
                    && field["flags"]["normalized"].as_bool() != Some(true)
                    && has_count(&field["value"])
            });
        let passed = self.data["smart_status"]["passed"].as_bool();
        let critical = &nvme["critical_warning"];
        let condition = if passed == Some(false) || has_count(critical) || has_count(&critical["value"]) {
            Condition::Critical
        } else if media_warning {
            Condition::MediaWarnings
        } else if remaining.is_some_and(|value| value <= 10) {
            Condition::LowEndurance
        } else if passed == Some(true) {
            Condition::Passed
        } else {
            Condition::Unavailable
        };
        Health { condition, remaining }
    }

    pub fn json(&self) -> String {
        serde_json::to_string_pretty(&self.data).unwrap_or_default()
    }

    pub fn full(&self) -> String {
        match self.data["smartctl"]["output"].as_array() {
            Some(lines) if !lines.is_empty() => lines.iter().filter_map(Value::as_str)
                .map(clean).collect::<Vec<_>>().join("\n"),
            _ => self.json(),
        }
    }

    pub fn rows(&self) -> Vec<(&'static str, Option<String>)> {
        let data = &self.data;
        let nvme = &data["nvme_smart_health_information_log"];
        let disk = self.disk.as_ref();
        let attributes = data["ata_smart_attributes"]["table"].as_array();
        let null = Value::Null;
        let statistic = |name: &str| -> &Value {
            self.ata_statistics()
                .find(|field| field["name"].as_str() == Some(name)
                    && field["flags"]["normalized"].as_bool() != Some(true))
                .map(|field| &field["value"]).unwrap_or(&null)
        };
        let attribute = |name: &str| -> &Value {
            attributes.into_iter().flatten()
                .find(|item| item["name"].as_str() == Some(name))
                .map(|item| &item["raw"]).unwrap_or(&null)
        };
        let raw = |name: &str| {
            let item = attribute(name);
            text(&item["string"]).or_else(|| text(&item["value"]))
        };
        let host_bytes = |direction: &str, statistic_name: &str, operation: &str| {
            number(&nvme[format!("data_units_{direction}_s")])
                .or_else(|| number(&nvme[format!("data_units_{direction}")]))
                .and_then(|count| count.checked_mul(512_000))
                .or_else(|| {
                    number(statistic(statistic_name))?.checked_mul(number(&data["logical_block_size"])?)
                })
                .or_else(|| {
                    [("32MiB", 32 * 2_u128.pow(20)), ("MiB", 2_u128.pow(20)), ("GiB", 2_u128.pow(30))]
                        .into_iter()
                        .find_map(|(suffix, scale)| {
                            ["Host", "Total_Host"].into_iter().find_map(|prefix| {
                                number(&attribute(&format!("{prefix}_{operation}_{suffix}"))["value"])
                                    .and_then(|value| value.checked_mul(scale))
                            })
                        })
                })
                .map(host_amount)
        };
        let pci_id = number(&data["nvme_pci_vendor"]["id"]);
        let vendor = text(&data["vendor"])
            .or_else(|| disk.and_then(|disk| disk.vendor.as_deref())
                .map(str::trim).filter(|value| !value.is_empty()).map(clean))
            .filter(|value| value != "ATA")
            .or_else(|| pci_id.and_then(pci_vendor_name));
        let temperature = text(&data["temperature"]["current"]).or_else(|| text(&nvme["temperature"]));
        let hours = text(&data["power_on_time"]["hours"]).or_else(|| text(&nvme["power_on_hours"]))
            .or_else(|| text(statistic("Power-on Hours")));
        let written = host_bytes("written", "Logical Sectors Written", "Writes");
        let unknown_written_units = written.is_none();
        let mut rows = vec![
            ("Eszköz", Some(self.path.clone())),
            ("Gyártó", vendor),
            ("Modellcsalád", text(&data["model_family"])),
            ("Modell", text(&data["model_name"]).or_else(|| text(&data["product"]))
                .or_else(|| disk.and_then(|disk| disk.model.as_deref()).map(clean))),
            ("Sorozatszám", text(&data["serial_number"])
                .or_else(|| disk.and_then(|disk| disk.serial.as_deref()).map(clean))),
            ("Firmware", text(&data["firmware_version"]).or_else(|| text(&data["revision"]))
                .or_else(|| disk.and_then(|disk| disk.rev.as_deref()).map(clean))),
            ("Protokoll", text(&data["device"]["protocol"])
                .or_else(|| disk.and_then(|disk| disk.tran.as_deref()).map(clean))),
            ("Kapacitás", number(&data["user_capacity"]["bytes"])
                .or_else(|| disk.map(|disk| u128::from(disk.size))).map(amount)),
            ("Logikai szektor", text(&data["logical_block_size"]).map(|size| format!("{size} B"))),
            ("Fizikai szektor", text(&data["physical_block_size"]).map(|size| format!("{size} B"))),
            ("Fordulatszám", text(&data["rotation_rate"])),
            ("NVMe PCI gyártóazonosító", pci_id.map(|id| format!("0x{id:04x}"))),
            ("SMART állapot", data["smart_status"]["passed"].as_bool()
                .map(|passed| if passed { "Megfelelt".into() } else { "Hibát jelez".into() })),
            ("Hőmérséklet", temperature.map(|value| format!("{value} °C"))),
            (POWER_ON_TIME, hours.map(|value| format!("{value} óra"))),
            ("Bekapcsolási ciklusok", text(&data["power_cycle_count"])
                .or_else(|| text(&nvme["power_cycles"]))
                .or_else(|| text(statistic("Lifetime Power-On Resets")))
                .or_else(|| raw("Power_Cycle_Count"))),
            ("Motor indítás/leállítás", text(&data["scsi_start_stop_cycle_counter"]["accumulated_start_stop_cycles"])
                .or_else(|| raw("Start_Stop_Count"))),
            ("Szabálytalan leállások", text(&nvme["unsafe_shutdowns"])
                .or_else(|| raw("Unsafe_Shutdown_Count")).or_else(|| raw("Unexpected_Power_Loss"))),
            ("Vészparkolások (ATA)", text(statistic("Number of High Priority Unload Events"))
                .or_else(|| raw("Power-Off_Retract_Count"))),
            (HOST_WRITTEN, written),
            (HOST_READ, host_bytes("read", "Logical Sectors Read", "Reads")),
            ("Írási parancsok", text(&nvme["host_writes"])
                .or_else(|| text(statistic("Number of Write Commands")))),
            ("Olvasási parancsok", text(&nvme["host_reads"])
                .or_else(|| text(statistic("Number of Read Commands")))),
            ("NVMe elhasználódás", text(&nvme["percentage_used"]).map(|value| format!("{value}%"))),
            ("NVMe tartalékkapacitás", text(&nvme["available_spare"]).map(|value| format!("{value}%"))),
            ("NVMe médiahibák", text(&nvme["media_errors"])),
            ("Áthelyezett szektorok", text(statistic("Number of Reallocated Logical Sectors"))
                .or_else(|| raw("Reallocated_Sector_Ct"))),
            ("Függőben lévő szektorok", raw("Current_Pending_Sector")
                .or_else(|| text(statistic("Number of Realloc. Candidate Logical Sectors")))),
            ("Javíthatatlan ATA szektorok", raw("Offline_Uncorrectable")),
            ("SCSI utólagos hibák", text(&data["scsi_grown_defect_list"])),
            ("SCSI javíthatatlan olvasási hibák",
                text(&data["scsi_error_counter_log"]["read"]["total_uncorrected_errors"])),
            ("SCSI javíthatatlan írási hibák",
                text(&data["scsi_error_counter_log"]["write"]["total_uncorrected_errors"])),
        ];
        if unknown_written_units {
            rows.extend(attributes.into_iter().flatten().filter_map(|item| {
                let name = item["name"].as_str()?;
                if !name.to_ascii_lowercase().contains("writ") {
                    return None;
                }
                Some(("Nyers ATA írásszámláló", Some(format!("{} = {}", clean(name), raw(name)?))))
            }));
        }
        rows
    }

    pub fn summary(&self) -> String {
        let health = self.health();
        let remaining = health.remaining.map(|value| format!("{value}%"))
            .unwrap_or_else(|| "Nem érhető el".into());
        let mut lines = vec![
            format!("{:36} {}", "Kondíció", health.condition.label()),
            format!("{:36} {remaining}", "Becsült hátralévő élettartam"),
            String::new(),
        ];
        lines.extend(self.rows().into_iter()
            .map(|(label, value)| format!("{label:36} {}", value.as_deref().unwrap_or("–"))));
        if self.code != 0 {
            lines.push(format!("\nSMART állapotkód: {}", self.code));
        }
        for message in self.data["smartctl"]["messages"].as_array().into_iter().flatten() {
            if let Some(message) = text(&message["string"]) {
                lines.push(message);
            }
        }
        lines.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn host_totals_switch_to_terabytes_at_one_terabyte() {
        assert_eq!(host_amount(512_500_000_000), "512.50 GB");
        assert_eq!(host_amount(1_000_000_000_000), "1.000 TB");
        assert_eq!(host_amount(1_250_000_000_000), "1.250 TB");
    }

    fn health(data: Value) -> Health {
        Report { data, code: 0, path: String::new(), disk: None }.health()
    }

    #[test]
    fn nvme_wear_is_inverted_and_critical_status_takes_precedence() {
        let report = |used, warning| json!({
            "smart_status": { "passed": true },
            "nvme_smart_health_information_log": {
                "percentage_used": used,
                "critical_warning": warning
            }
        });
        assert_eq!(health(report(8, 0)), Health { condition: Condition::Passed, remaining: Some(92) });
        assert_eq!(health(report(110, 0)), Health { condition: Condition::LowEndurance, remaining: Some(0) });
        assert_eq!(health(report(8, 1)), Health { condition: Condition::Critical, remaining: Some(92) });
    }

    #[test]
    fn sata_wear_prefers_standard_statistic_over_vendor_attribute() {
        let report = |valid| json!({
            "smart_status": { "passed": true },
            "ata_device_statistics": { "pages": [{ "table": [{
                "name": "Percentage Used Endurance Indicator",
                "value": 15,
                "flags": { "valid": valid, "normalized": true }
            }]}] },
            "ata_smart_attributes": { "table": [{
                "name": "Percent_Lifetime_Remain", "value": 81, "raw": { "value": 19 }
            }] }
        });
        assert_eq!(health(report(true)).remaining, Some(85));
        assert_eq!(health(report(false)).remaining, Some(81));
    }

    #[test]
    fn hdd_reports_sector_warnings_without_fabricating_a_percentage() {
        let disk = health(json!({
            "smart_status": { "passed": true },
            "ata_smart_attributes": { "table": [
                { "name": "Reallocated_Sector_Ct", "value": 100, "raw": { "value": 0 } },
                { "name": "Current_Pending_Sector", "value": 100, "raw": { "value": 2 } }
            ] }
        }));
        assert_eq!(disk, Health { condition: Condition::MediaWarnings, remaining: None });
        assert_eq!(health(json!({})), Health { condition: Condition::Unavailable, remaining: None });
    }
}
