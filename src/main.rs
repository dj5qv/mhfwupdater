mod bootloader;
mod cbl;
mod devices;
mod discover;
mod keyer;
mod link;
#[cfg(test)]
mod sim;

use std::io::{self, BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use clap::{ArgAction, Args, Parser, Subcommand};
use log::LevelFilter;

use crate::bootloader::Entry;
use crate::cbl::Firmware;
use crate::devices::{HwInfo, family_name};
use crate::keyer::FirmwareInfo;
use crate::link::{Link, SerialLink};

const RECOVER_WINDOW: Duration = Duration::from_secs(60);

#[derive(Parser)]
#[command(version, about = "Firmware updater for microHAM USB devices")]
struct Cli {
    /// More output (-v: debug, -vv: also byte traffic)
    #[arg(short, long, action = ArgAction::Count, global = true)]
    verbose: u8,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List attached microHAM devices
    List,
    /// Show the contents of firmware files
    Info {
        #[arg(required = true)]
        files: Vec<PathBuf>,
    },
    /// Show the firmware version of a device
    Probe {
        #[command(flatten)]
        device: DeviceArg,
        /// Also restart into the bootloader and read its version (writes nothing)
        #[arg(long)]
        bootloader: bool,
    },
    /// Write a firmware file to a device
    Flash {
        file: PathBuf,
        #[command(flatten)]
        device: DeviceArg,
        /// Check the file against the device and enter the bootloader, but write nothing
        #[arg(long)]
        dry_run: bool,
        /// Don't write the "invalidate" page that guards against interrupted updates
        #[arg(long)]
        no_invalidate: bool,
        /// Don't ask the firmware to restart, wait for the bootloader instead
        /// (after an interrupted update, or while you power-cycle the device)
        #[arg(long)]
        recover: bool,
        /// Don't ask for confirmation
        #[arg(short, long)]
        yes: bool,
    },
}

#[derive(Args)]
struct DeviceArg {
    /// Device path or serial number, needed only with several devices attached
    #[arg(short, long)]
    device: Option<String>,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    env_logger::Builder::new()
        .filter_level(match cli.verbose {
            0 => LevelFilter::Warn,
            1 => LevelFilter::Debug,
            _ => LevelFilter::Trace,
        })
        .format(|buf, record| writeln!(buf, "{}: {}", record.level().as_str().to_lowercase(), record.args()))
        .init();

    let result = match cli.command {
        Command::List => list(),
        Command::Info { files } => info(&files),
        Command::Probe { device, bootloader } => probe(&device, bootloader),
        Command::Flash { file, device, dry_run, no_invalidate, recover, yes } => {
            flash(&file, &device, dry_run, no_invalidate, recover, yes)
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn list() -> Result<()> {
    for d in discover::find_devices()? {
        println!("{}  {}  {}", d.tty.display(), d.serial, d.product);
    }
    Ok(())
}

fn info(files: &[PathBuf]) -> Result<()> {
    for file in files {
        let fw = Firmware::load(file)?;
        let v = &fw.version;
        println!("{}", file.display());
        println!("  device:   {} (product type 0x{:02x})", family_name(v.product_type), v.product_type);
        println!("  version:  {}.{}", v.version_major, v.version_minor);
        println!("  requires: hardware {}, mechanical {}", v.min_hardware, v.min_mechanical);
        println!(
            "  pages:    {} x {} bytes ({:.1} KiB of flash)",
            fw.pages.len(),
            fw.page_len(),
            fw.payload_len() as f64 / 1024.0
        );
        for comment in &fw.comments {
            println!("  comment:  {comment}");
        }
    }
    Ok(())
}

fn select_device(arg: &DeviceArg) -> Result<PathBuf> {
    let devices = discover::find_devices()?;
    match arg.device.as_deref() {
        Some(path) if path.starts_with('/') => Ok(PathBuf::from(path)),
        Some(serial) => devices
            .into_iter()
            .find(|d| d.serial.eq_ignore_ascii_case(serial))
            .map(|d| d.tty)
            .ok_or_else(|| anyhow!("no device with serial number {serial}")),
        None => match &devices[..] {
            [] => bail!("no microHAM device found"),
            [d] => Ok(d.tty.clone()),
            _ => bail!("several devices attached, select one with --device"),
        },
    }
}

fn print_firmware(info: &FirmwareInfo) {
    println!(
        "Firmware:   {} {}.{}",
        family_name(info.hw.product_type),
        info.version_major,
        info.version_minor
    );
    if info.appl_product_type != info.hw.product_type {
        println!("            (built for {})", family_name(info.appl_product_type));
    }
}

fn print_bootloader(hw: &HwInfo) {
    println!(
        "Bootloader: {}.{}, product type 0x{:02x}/0x{:02x}, hardware {}, mechanical {}",
        hw.bootloader_major, hw.bootloader_minor, hw.product_type, hw.product_subtype, hw.hardware, hw.mechanical
    );
}

/// Waits for the firmware to come up after the bootloader terminated.
fn wait_for_firmware(link: &mut dyn Link) -> Result<FirmwareInfo> {
    let mut result = Err(anyhow!("firmware did not start"));
    for _ in 0..5 {
        thread::sleep(Duration::from_millis(500));
        result = keyer::get_version(link);
        if result.is_ok() {
            break;
        }
    }
    result
}

fn probe(device: &DeviceArg, bootloader: bool) -> Result<()> {
    let path = select_device(device)?;
    let mut link = SerialLink::open(&path)?;
    println!("Device:     {}", path.display());
    print_firmware(&keyer::get_version(&mut link)?);

    if bootloader {
        print_bootloader(&bootloader::enter(&mut link, Entry::Command)?);
        bootloader::wait_terminated(&mut link)?;
        wait_for_firmware(&mut link).context("firmware did not come back after the bootloader")?;
        println!("Firmware restarted.");
    }
    Ok(())
}

fn confirm() -> Result<()> {
    if !io::stdin().is_terminal() {
        bail!("no terminal to confirm, use --yes");
    }
    print!("Write firmware? Don't interrupt or power off the device while writing. [y/N] ");
    io::stdout().flush()?;
    let mut answer = String::new();
    io::stdin().lock().read_line(&mut answer)?;
    if !matches!(answer.trim(), "y" | "Y" | "yes") {
        bail!("cancelled");
    }
    Ok(())
}

fn flash(
    file: &Path,
    device: &DeviceArg,
    dry_run: bool,
    no_invalidate: bool,
    recover: bool,
    yes: bool,
) -> Result<()> {
    let fw = Firmware::load(file)?;
    let v = &fw.version;
    let family = devices::family(v.product_type)
        .ok_or_else(|| anyhow!("firmware is for unknown product type 0x{:02x}", v.product_type))?;
    let path = select_device(device)?;
    let mut link = SerialLink::open(&path)?;

    println!("Device:     {}", path.display());
    if !recover {
        let info = keyer::get_version(&mut link)
            .context("no answer from the device firmware (after an interrupted update, use --recover)")?;
        print_firmware(&info);
        devices::check_compatible(&fw, &info.hw)?;
    }
    let invalidate = if no_invalidate { None } else { family.invalidate_page() };
    println!("New:        {} {}.{}, {} pages", family.name, v.version_major, v.version_minor, fw.pages.len());
    println!(
        "Protection: {}",
        match (&invalidate, no_invalidate) {
            (Some(_), _) => "invalidate page",
            (None, true) => "off",
            (None, false) => "none known for this device",
        }
    );
    if !dry_run && !yes {
        confirm()?;
    }

    let entry = if recover {
        println!("Waiting for the bootloader, power-cycle the device now...");
        Entry::Wait(RECOVER_WINDOW)
    } else {
        Entry::Command
    };
    let hw = bootloader::enter(&mut link, entry)?;
    print_bootloader(&hw);
    // Nothing is written yet. On errors, the bootloader times out and restarts
    // the old firmware.
    devices::check_compatible(&fw, &hw)?;

    if dry_run {
        bootloader::wait_terminated(&mut link)?;
        println!("Dry run, nothing written.");
        return Ok(());
    }

    let result = bootloader::flash(&mut link, &fw, invalidate.as_deref(), &mut |n, total| {
        eprint!("\rWriting:    page {n}/{total} ({}%)", n * 100 / total);
    });
    eprintln!();
    let invalidated = result.context(format!(
        "update incomplete, finish it with: mhfwupdater flash --recover {}",
        file.display()
    ))?;
    if invalidate.is_some() && !invalidated {
        println!("Note:       the device rejected the invalidate page.");
    }

    let info = wait_for_firmware(&mut link).context("new firmware does not answer")?;
    print_firmware(&info);
    Ok(())
}
