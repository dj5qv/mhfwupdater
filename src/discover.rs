// mhfwupdater - firmware updater for microHAM USB devices
// Copyright (C) 2026  Matthias Moeller, DJ5QV
// SPDX-License-Identifier: GPL-2.0-only

//! Finds attached microHAM devices (FTDI VID 0403, PID EEEF) through sysfs.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub struct UsbDevice {
    pub tty: PathBuf,
    pub serial: String,
    pub product: String,
}

pub fn find_devices() -> io::Result<Vec<UsbDevice>> {
    let mut devices = Vec::new();
    for entry in fs::read_dir("/sys/class/tty")? {
        let entry = entry?;
        let name = entry.file_name();
        if !name.to_string_lossy().starts_with("ttyUSB") {
            continue;
        }
        let Ok(dev) = fs::canonicalize(entry.path().join("device")) else { continue };
        // The USB device is the closest ancestor of the tty's interface with a vendor id.
        let Some(usb) = dev.ancestors().find(|p| p.join("idVendor").exists()) else { continue };
        let attr = |name| read_attr(usb, name);
        if attr("idVendor") == "0403" && attr("idProduct").eq_ignore_ascii_case("eeef") {
            devices.push(UsbDevice {
                tty: Path::new("/dev").join(&name),
                serial: attr("serial"),
                product: attr("product"),
            });
        }
    }
    devices.sort_by(|a, b| a.tty.cmp(&b.tty));
    Ok(devices)
}

fn read_attr(dir: &Path, name: &str) -> String {
    fs::read_to_string(dir.join(name)).map(|s| s.trim().to_string()).unwrap_or_default()
}
