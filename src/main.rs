mod devices;
mod i18n;
mod ui;
#[cfg(feature = "desktop")]
mod desktop;

use devices::Result;
use std::{env, process::ExitCode};

fn usage() {
    println!("Roadmap · {}\n\n  roadmap\n  roadmap list\n  roadmap smart {} [--full | --json] [--type {}]", i18n::tr("Linux SMART-lekérdezés"), i18n::tr("ESZKÖZ"), i18n::tr("USB-TÍPUS"));
    #[cfg(feature = "desktop")]
    println!("  roadmap --gui\n  roadmap --install-desktop");
}

fn run() -> Result<u8> {
    let args: Vec<String> = env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None => ui::run()?,
        #[cfg(feature = "desktop")]
        Some("--gui") if args.len() == 1 => desktop::run()?,
        #[cfg(feature = "desktop")]
        Some("--install-desktop") if args.len() == 1 => desktop::install()?,
        Some("--help" | "-h") => usage(),
        Some("list") if args.len() == 1 => {
            for disk in devices::disks()? {
                println!("{}", disk.label());
                for partition in disk.partitions() {
                    println!("  {partition}");
                }
            }
        }
        Some("smart") if args.get(1).is_some_and(|path| !path.starts_with('-')) => {
            let mut options = args[2..].iter().map(String::as_str);
            let mut view = None;
            let mut adapter = None;
            while let Some(option) = options.next() {
                match option {
                    "--full" | "--json" if view.is_none() => view = Some(option),
                    "--type" if adapter.is_none() => {
                        adapter = Some(options.next().ok_or("A --type kapcsolóhoz USB-típus szükséges.")?);
                    }
                    _ => return Err(i18n::message("Ismeretlen vagy ismételt kapcsoló: {}", &[option]).into()),
                }
            }
            let report = devices::read_smart(&args[1], adapter)?;
            let text = match view {
                Some("--full") => report.full(),
                Some("--json") => report.json(),
                _ => report.summary(),
            };
            println!("{text}");
            return Ok(u8::try_from(report.code).unwrap_or(1));
        }
        _ => return Err("Érvénytelen parancs. Súgó: roadmap --help".into()),
    }
    Ok(0)
}

fn main() -> ExitCode {
    i18n::load();
    match run() {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            eprintln!("{}", i18n::message("Hiba: {}", &[&i18n::error(&devices::clean(&error.to_string()))]));
            ExitCode::FAILURE
        }
    }
}
