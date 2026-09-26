// mhfwupdater - firmware updater for microHAM USB devices
// Copyright (C) 2026  Matthias Moeller, DJ5QV
// SPDX-License-Identifier: GPL-2.0-only

//! Parser for microHAM firmware files (`*.cbl`, file format version 2.1).
//!
//! A file is a sequence of blocks: type byte, length byte, content. Flash data
//! blocks hold encoded pages that are passed to the bootloader unchanged.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};

const BLOCK_FLASH: u8 = 0x01;
const BLOCK_EEPROM: u8 = 0x02;
const BLOCK_VERSION: u8 = 0x03;
const BLOCK_COMMENT: u8 = 0x20;

/// Page length of the MK family.
pub const PAGE_LEN_SHORT: usize = 70;
/// Page length of all other families.
pub const PAGE_LEN_LONG: usize = 134;

/// Version specification block: what the firmware requires from the device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VersionSpec {
    pub product_type: u8,
    pub min_hardware: u8,
    pub min_mechanical: u8,
    pub version_minor: u8,
    pub version_major: u8,
    pub min_control_software: u8,
}

#[derive(Debug, Clone)]
pub struct Firmware {
    pub comments: Vec<String>,
    pub version: VersionSpec,
    /// Encoded flash pages in file order. The first one is the zero page.
    pub pages: Vec<Vec<u8>>,
}

impl Firmware {
    pub fn load(path: &Path) -> Result<Self> {
        let data = fs::read(path).with_context(|| format!("cannot read {}", path.display()))?;
        Self::parse(&data).with_context(|| format!("{} is not a valid firmware file", path.display()))
    }

    pub fn parse(data: &[u8]) -> Result<Self> {
        let mut comments = Vec::new();
        let mut version = None;
        let mut pages: Vec<Vec<u8>> = Vec::new();
        let mut pos = 0;

        while pos < data.len() {
            let (kind, len) = match data[pos..] {
                [kind, len, ..] => (kind, len as usize),
                _ => bail!("truncated block header at offset {pos}"),
            };
            let body = data
                .get(pos + 2..pos + 2 + len)
                .ok_or_else(|| anyhow!("block at offset {pos} is truncated"))?;

            match kind {
                BLOCK_FLASH => {
                    if len != PAGE_LEN_SHORT && len != PAGE_LEN_LONG {
                        bail!("flash block at offset {pos} has unexpected length {len}");
                    }
                    if pages.first().is_some_and(|p| p.len() != len) {
                        bail!("flash blocks of different lengths");
                    }
                    pages.push(body.to_vec());
                }
                BLOCK_VERSION => {
                    if version.is_some() {
                        bail!("more than one version block");
                    }
                    let [product_type, min_hardware, min_mechanical, version_minor, version_major, min_control_software, ..] =
                        *body
                    else {
                        bail!("version block at offset {pos} is too short");
                    };
                    version = Some(VersionSpec {
                        product_type,
                        min_hardware,
                        min_mechanical,
                        version_minor,
                        version_major,
                        min_control_software,
                    });
                }
                BLOCK_COMMENT => comments.push(String::from_utf8_lossy(body).trim_end().to_string()),
                BLOCK_EEPROM => bail!("EEPROM data block at offset {pos}, not supported by the bootloader"),
                _ => bail!("unknown block type 0x{kind:02x} at offset {pos}"),
            }
            pos += 2 + len;
        }

        let version = version.ok_or_else(|| anyhow!("no version block"))?;
        if pages.is_empty() {
            bail!("no flash data");
        }
        Ok(Firmware { comments, version, pages })
    }

    pub fn page_len(&self) -> usize {
        self.pages[0].len()
    }

    /// Flash bytes covered by the pages, assuming each page carries a 4 byte
    /// header and a 2 byte checksum like the documented "invalidate" pages.
    pub fn payload_len(&self) -> usize {
        self.pages.len() * (self.page_len() - 6)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(pages: &[&[u8]]) -> Vec<u8> {
        let mut f = vec![BLOCK_COMMENT, 4, b'a', b'b', b' ', b'\n'];
        f.extend_from_slice(&[BLOCK_VERSION, 6, 0x17, 0, 0, 12, 2, 0]);
        for p in pages {
            f.push(BLOCK_FLASH);
            f.push(p.len() as u8);
            f.extend_from_slice(p);
        }
        f
    }

    #[test]
    fn parses_blocks() {
        let fw = Firmware::parse(&file(&[&[1; 134], &[2; 134]])).unwrap();
        assert_eq!(fw.comments, ["ab"]);
        assert_eq!(fw.version.product_type, 0x17);
        assert_eq!((fw.version.version_major, fw.version.version_minor), (2, 12));
        assert_eq!(fw.pages.len(), 2);
        assert_eq!(fw.pages[1], [2; 134]);
        assert_eq!(fw.payload_len(), 256);
    }

    #[test]
    fn rejects_bad_files() {
        let good = file(&[&[1; 134]]);
        assert!(Firmware::parse(&good[..good.len() - 1]).is_err(), "truncated");
        assert!(Firmware::parse(&file(&[&[1; 100]])).is_err(), "odd page length");
        assert!(Firmware::parse(&file(&[&[1; 134], &[1; 70]])).is_err(), "mixed page lengths");
        assert!(Firmware::parse(&file(&[])).is_err(), "no pages");
        assert!(Firmware::parse(&good[6..6 + 8]).is_err(), "no pages, version only");
        assert!(Firmware::parse(&good[..6]).is_err(), "no version");

        let mut eeprom = good.clone();
        eeprom.extend_from_slice(&[BLOCK_EEPROM, 1, 0]);
        assert!(Firmware::parse(&eeprom).is_err());

        let mut unknown = good;
        unknown.extend_from_slice(&[0x04, 0]);
        assert!(Firmware::parse(&unknown).is_err());
    }

    /// Parses the firmware files in `firmware/`, if there are any.
    #[test]
    fn parses_real_files() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("firmware");
        let Ok(entries) = fs::read_dir(dir) else { return };
        for entry in entries {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|e| e == "cbl") {
                let fw = Firmware::load(&path).unwrap();
                let family = crate::devices::family(fw.version.product_type).unwrap();
                assert_eq!(fw.page_len(), family.page_len, "{}", path.display());
            }
        }
    }
}
