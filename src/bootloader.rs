// mhfwupdater - firmware updater for microHAM USB devices
// Copyright (C) 2026  Matthias Moeller, DJ5QV
// SPDX-License-Identifier: GPL-2.0-only

//! Bootloader protocol: entering the bootloader, reading its version and
//! writing firmware pages. Unlike the keyer protocol, it uses plain bytes.

use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail};
use log::debug;

use crate::cbl::Firmware;
use crate::devices::HwInfo;
use crate::keyer;
use crate::link::Link;

// From the bootloader.
pub const STARTED: u8 = 0x01;
pub const VERSION: u8 = 0x02;
pub const WRITE_OK: u8 = 0x03;
pub const CHECKSUM_ERR: u8 = 0x04;
pub const STARTING_FIRMWARE: u8 = 0x05;
pub const WRITE_FAILED: u8 = 0x06;
pub const TERMINATED: u8 = 0x07;
// From the computer.
pub const START: u8 = 0x42;
pub const GET_VERSION: u8 = 0x43;
pub const NEXT_PAGE: u8 = 0x44;

/// The bootloader listens for START for 0.5 s (2 s before v1.2) after reset.
const START_INTERVAL: Duration = Duration::from_millis(20);
const RESET_WINDOW: Duration = Duration::from_secs(3);
const REPLY_TIMEOUT: Duration = Duration::from_millis(500);
const PAGE_TIMEOUT: Duration = Duration::from_millis(1500);
/// The bootloader gives up 2 s after the last byte it received.
const TERMINATE_TIMEOUT: Duration = Duration::from_secs(4);
pub const PAGE_ATTEMPTS: usize = 3;

pub enum Entry {
    /// Ask the running firmware to reset into the bootloader.
    Command,
    /// Wait for a reset caused otherwise (power cycle, or a looping
    /// bootloader after an interrupted update).
    Wait(Duration),
}

fn recv_until(link: &mut dyn Link, until: Instant) -> Result<Option<u8>> {
    match until.checked_duration_since(Instant::now()) {
        Some(timeout) => Ok(link.recv(timeout)?),
        None => Ok(None),
    }
}

/// Brings the device into the bootloader and returns the bootloader's info.
/// Once this returned, the bootloader waits at most 2 s for the next byte.
pub fn enter(link: &mut dyn Link, entry: Entry) -> Result<HwInfo> {
    let window = match entry {
        Entry::Command => {
            if let Err(e) = keyer::start_bootloader(link) {
                debug!("{e:#}");
            }
            link.discard_input()?;
            RESET_WINDOW
        }
        Entry::Wait(window) => window,
    };
    if !catch(link, window)? {
        bail!("bootloader did not answer");
    }
    get_version(link)
}

/// Sends START repeatedly until the bootloader answers or `window` has passed.
fn catch(link: &mut dyn Link, window: Duration) -> Result<bool> {
    let deadline = Instant::now() + window;
    while Instant::now() < deadline {
        link.send(&[START])?;
        let until = Instant::now() + START_INTERVAL;
        while let Some(b) = recv_until(link, until)? {
            match b {
                STARTED => return Ok(true),
                STARTING_FIRMWARE => debug!("bootloader is starting the firmware"),
                _ => {}
            }
        }
    }
    Ok(false)
}

fn get_version(link: &mut dyn Link) -> Result<HwInfo> {
    link.send(&[GET_VERSION])?;
    let until = Instant::now() + REPLY_TIMEOUT;
    // Answers to further START bytes may still be on their way.
    let mut b = recv_until(link, until)?;
    while b == Some(STARTED) {
        b = recv_until(link, until)?;
    }
    match b {
        Some(VERSION) => {}
        Some(b) => bail!("unexpected answer 0x{b:02x} to GET VERSION"),
        None => bail!("no answer to GET VERSION"),
    }
    let mut x = [0; 8];
    for v in &mut x {
        let b = recv_until(link, until)?.ok_or_else(|| anyhow!("bootloader version info truncated"))?;
        if b & 0x80 == 0 {
            bail!("invalid bootloader version info byte 0x{b:02x}");
        }
        *v = b & 0x7f;
    }
    Ok(HwInfo::from_bytes(&x))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageStatus {
    Written,
    ChecksumError,
    WriteFailed,
}

fn write_page(link: &mut dyn Link, page: &[u8]) -> Result<PageStatus> {
    let mut msg = Vec::with_capacity(1 + page.len());
    msg.push(NEXT_PAGE);
    msg.extend_from_slice(page);
    link.send(&msg)?;
    match recv_until(link, Instant::now() + PAGE_TIMEOUT)? {
        Some(WRITE_OK) => Ok(PageStatus::Written),
        Some(CHECKSUM_ERR) => Ok(PageStatus::ChecksumError),
        Some(WRITE_FAILED) => Ok(PageStatus::WriteFailed),
        Some(TERMINATED) => bail!("bootloader terminated"),
        Some(b) => bail!("unexpected answer 0x{b:02x} to page"),
        None => bail!("bootloader did not answer page"),
    }
}

fn write_page_retry(link: &mut dyn Link, page: &[u8]) -> Result<PageStatus> {
    let mut status = PageStatus::WriteFailed;
    for attempt in 1..=PAGE_ATTEMPTS {
        status = write_page(link, page)?;
        if status == PageStatus::Written {
            break;
        }
        debug!("page attempt {attempt}: {status:?}");
    }
    Ok(status)
}

/// Waits until the bootloader times out and starts the firmware.
pub fn wait_terminated(link: &mut dyn Link) -> Result<()> {
    let until = Instant::now() + TERMINATE_TIMEOUT;
    loop {
        match recv_until(link, until)? {
            Some(TERMINATED) => return Ok(()),
            Some(b) => debug!("ignoring 0x{b:02x} while waiting for bootloader to terminate"),
            None => bail!("bootloader did not terminate"),
        }
    }
}

/// Writes all pages of `fw` and waits for the bootloader to start the new
/// firmware. With `invalidate`, that page is written first and the firmware's
/// zero page last, so that an interrupted update leaves a device that jumps
/// straight back into the bootloader. Returns whether `invalidate` was used.
pub fn flash(
    link: &mut dyn Link,
    fw: &Firmware,
    invalidate: Option<&[u8]>,
    progress: &mut dyn FnMut(usize, usize),
) -> Result<bool> {
    let mut order: Vec<usize> = (0..fw.pages.len()).collect();
    let mut invalidated = false;

    if let Some(page) = invalidate {
        match write_page_retry(link, page)? {
            PageStatus::Written => {
                invalidated = true;
                order.rotate_left(1);
            }
            // Nothing was written, the device's firmware is still intact.
            PageStatus::ChecksumError => {}
            PageStatus::WriteFailed => bail!("writing the invalidate page failed"),
        }
    }

    let total = order.len();
    for (n, &i) in order.iter().enumerate() {
        match write_page_retry(link, &fw.pages[i])? {
            PageStatus::Written => progress(n + 1, total),
            status => bail!("firmware page {i}: {status:?} after {PAGE_ATTEMPTS} attempts"),
        }
    }
    wait_terminated(link)?;
    Ok(invalidated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::devices::{check_compatible, family};
    use crate::sim::SimDevice;

    fn firmware(product_type: u8, pages: usize) -> Firmware {
        let mut data = vec![0x03, 6, product_type, 0, 0, 12, 2, 0];
        for i in 0..pages {
            data.extend_from_slice(&[0x01, 134]);
            data.extend(std::iter::repeat_n(i as u8 + 1, 134));
        }
        Firmware::parse(&data).unwrap()
    }

    fn update(dev: &mut SimDevice, fw: &Firmware, invalidate: bool) -> Result<bool> {
        let hw = enter(dev, Entry::Command)?;
        let family = check_compatible(fw, &hw)?;
        let page = family.invalidate_page().filter(|_| invalidate);
        flash(dev, fw, page.as_deref(), &mut |_, _| {})
    }

    #[test]
    fn flashes_in_file_order_without_invalidation() {
        let mut dev = SimDevice::new(0x16);
        let fw = firmware(0x16, 4);
        assert!(!update(&mut dev, &fw, false).unwrap());
        assert_eq!(dev.written, fw.pages);
        assert!(dev.running_firmware());
    }

    #[test]
    fn writes_zero_page_last_with_invalidation() {
        let mut dev = SimDevice::new(0x16);
        let fw = firmware(0x16, 4);
        assert!(update(&mut dev, &fw, true).unwrap());
        let invalidate = family(0x16).unwrap().invalidate_page().unwrap();
        let expected = [&invalidate, &fw.pages[1], &fw.pages[2], &fw.pages[3], &fw.pages[0]];
        assert_eq!(dev.written.iter().collect::<Vec<_>>(), expected);
    }

    #[test]
    fn continues_without_rejected_invalidate_page() {
        let mut dev = SimDevice::new(0x16);
        dev.reject_invalidate = true;
        let fw = firmware(0x16, 3);
        assert!(!update(&mut dev, &fw, true).unwrap());
        assert_eq!(dev.written, fw.pages);
    }

    #[test]
    fn retries_pages() {
        let mut dev = SimDevice::new(0x16);
        dev.answers = vec![(1, CHECKSUM_ERR), (2, WRITE_FAILED)];
        let fw = firmware(0x16, 3);
        update(&mut dev, &fw, false).unwrap();
        assert_eq!(dev.written, fw.pages);
    }

    #[test]
    fn gives_up_on_persistent_errors() {
        let mut dev = SimDevice::new(0x16);
        dev.answers = (1..=PAGE_ATTEMPTS).map(|n| (n, CHECKSUM_ERR)).collect();
        assert!(update(&mut dev, &firmware(0x16, 3), false).is_err());
        assert_eq!(dev.written.len(), 1);
    }

    #[test]
    fn writes_nothing_to_incompatible_device() {
        let mut dev = SimDevice::new(0x17);
        assert!(update(&mut dev, &firmware(0x16, 3), false).is_err());
        assert!(dev.written.is_empty());
    }

    #[test]
    fn enters_bootloader_by_waiting() {
        let mut dev = SimDevice::new(0x17);
        dev.reset();
        let hw = enter(&mut dev, Entry::Wait(Duration::from_secs(1))).unwrap();
        assert_eq!(hw.product_type, 0x17);
        assert_eq!((hw.bootloader_major, hw.bootloader_minor), (2, 3));
    }
}
