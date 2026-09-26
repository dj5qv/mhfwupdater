// mhfwupdater - firmware updater for microHAM USB devices
// Copyright (C) 2026  Matthias Moeller, DJ5QV
// SPDX-License-Identifier: GPL-2.0-only

//! Device families and firmware compatibility rules.

use anyhow::{Result, bail};

use crate::cbl::Firmware;

/// Checksum of the "invalidate" pages, as given in the bootloader documentation.
const INVALIDATE_CHECKSUM: [u8; 2] = [0x01, 0xcd];

pub struct Family {
    pub product_type: u8,
    pub name: &'static str,
    pub page_len: usize,
    /// Jump back into the bootloader, placed at the start of the page data of
    /// the "invalidate" page. `None` where the documented one is doubtful.
    pub invalidate_code: Option<&'static [u8]>,
}

const JMP_0X1FC00: &[u8] = &[0x0c, 0x94, 0x00, 0xfe];

pub static FAMILIES: &[Family] = &[
    Family {
        product_type: 0x10,
        name: "micro KEYER / CW KEYER / DIGI KEYER",
        page_len: 70,
        invalidate_code: Some(&[0xff, 0xce]),
    },
    Family {
        product_type: 0x11,
        name: "micro KEYER 2R / 2R+",
        page_len: 134,
        invalidate_code: Some(&[0x0d, 0x94, 0x00, 0xfe]),
    },
    Family {
        product_type: 0x12,
        name: "micro KEYER II",
        page_len: 134,
        invalidate_code: Some(JMP_0X1FC00),
    },
    Family {
        product_type: 0x13,
        name: "Station Master",
        page_len: 134,
        invalidate_code: Some(JMP_0X1FC00),
    },
    Family {
        product_type: 0x14,
        name: "Station Master DeLuxe",
        page_len: 134,
        invalidate_code: Some(&[0x0d, 0x94, 0x00, 0xfc]),
    },
    Family {
        product_type: 0x15,
        name: "micro 2R",
        page_len: 134,
        invalidate_code: Some(JMP_0X1FC00),
    },
    Family {
        product_type: 0x16,
        name: "DIGI KEYER II",
        page_len: 134,
        invalidate_code: Some(JMP_0X1FC00),
    },
    // The documentation groups the MK3 with the MK2 (jump to 0x1FC00), but MK3
    // images are ~190 KiB, too large for a bootloader at 0x1FC00.
    Family {
        product_type: 0x17,
        name: "micro KEYER III",
        page_len: 134,
        invalidate_code: None,
    },
];

pub fn family(product_type: u8) -> Option<&'static Family> {
    FAMILIES.iter().find(|f| f.product_type == product_type)
}

pub fn family_name(product_type: u8) -> String {
    match family(product_type) {
        Some(f) => f.name.to_string(),
        None => format!("unknown product type 0x{product_type:02x}"),
    }
}

impl Family {
    /// Page that replaces the zero page during the update, so that an
    /// interrupted update leaves a device that jumps back into the bootloader.
    pub fn invalidate_page(&self) -> Option<Vec<u8>> {
        let code = self.invalidate_code?;
        let mut page = vec![0; self.page_len];
        page[4..4 + code.len()].copy_from_slice(code);
        page[self.page_len - 2..].copy_from_slice(&INVALIDATE_CHECKSUM);
        Some(page)
    }
}

/// Permanent part of the version info (bytes X0..X5), reported alike by the
/// bootloader and the firmware.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HwInfo {
    pub bootloader_minor: u8,
    pub bootloader_major: u8,
    pub product_type: u8,
    pub hardware: u8,
    pub mechanical: u8,
    pub product_subtype: u8,
}

impl HwInfo {
    pub fn from_bytes(x: &[u8; 8]) -> Self {
        HwInfo {
            bootloader_minor: x[0],
            bootloader_major: x[1],
            product_type: x[2],
            hardware: x[3],
            mechanical: x[4],
            product_subtype: x[5],
        }
    }
}

/// Checks that `fw` may be written to a device reporting `hw`.
pub fn check_compatible(fw: &Firmware, hw: &HwInfo) -> Result<&'static Family> {
    let v = &fw.version;
    let Some(family) = family(hw.product_type) else {
        bail!("device reports unknown product type 0x{:02x}", hw.product_type);
    };
    if v.product_type != hw.product_type {
        bail!(
            "firmware is for {}, but the device is a {}",
            family_name(v.product_type),
            family.name
        );
    }
    if fw.page_len() != family.page_len {
        bail!("firmware has {} byte pages, {} expected", fw.page_len(), family.page_len);
    }
    if hw.hardware < v.min_hardware || hw.mechanical < v.min_mechanical {
        bail!(
            "firmware requires hardware/mechanical version {}.{}, device has {}.{}",
            v.min_hardware,
            v.min_mechanical,
            hw.hardware,
            hw.mechanical
        );
    }
    Ok(family)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cbl::VersionSpec;

    #[test]
    fn invalidate_pages_match_documentation() {
        let mk = family(0x10).unwrap().invalidate_page().unwrap();
        assert_eq!(mk.len(), 70);
        assert_eq!(mk[..8], [0, 0, 0, 0, 0xff, 0xce, 0, 0]);
        assert!(mk[8..68].iter().all(|&b| b == 0));
        assert_eq!(mk[68..], [0x01, 0xcd]);

        let dk2 = family(0x16).unwrap().invalidate_page().unwrap();
        assert_eq!(dk2.len(), 134);
        assert_eq!(dk2[..8], [0, 0, 0, 0, 0x0c, 0x94, 0x00, 0xfe]);
        assert_eq!(dk2[132..], [0x01, 0xcd]);

        assert!(family(0x17).unwrap().invalidate_page().is_none());
    }

    #[test]
    fn compatibility() {
        let fw = |product_type, min_hardware| Firmware {
            comments: vec![],
            version: VersionSpec {
                product_type,
                min_hardware,
                min_mechanical: 0,
                version_minor: 12,
                version_major: 2,
                min_control_software: 0,
            },
            pages: vec![vec![0; 134]],
        };
        let hw = HwInfo::from_bytes(&[3, 2, 0x17, 1, 1, 1, 0, 0]);
        assert!(check_compatible(&fw(0x17, 0), &hw).is_ok());
        assert!(check_compatible(&fw(0x17, 1), &hw).is_ok());
        assert!(check_compatible(&fw(0x17, 2), &hw).is_err());
        assert!(check_compatible(&fw(0x12, 0), &hw).is_err());
    }
}
