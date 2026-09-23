//! The small part of the keyer protocol (spoken by the device firmware) needed
//! here: reading the version and restarting into the bootloader.
//!
//! Only the CONTROL channel is used. It is carried in the last byte of the
//! second frame of each sequence, so every control byte costs two frames.

use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use log::debug;

use crate::devices::HwInfo;
use crate::link::Link;

const CMD_GET_VERSION: u8 = 0x05;
const CMD_START_BOOTLOADER: u8 = 0x06;
const CMD_NOT_SUPPORTED: u8 = 0x7f;

/// The documentation allows 100 ms; leave room for USB latency.
const REPLY_TIMEOUT: Duration = Duration::from_millis(300);
const QUERY_ATTEMPTS: usize = 3;
const MAX_PACKET_LEN: usize = 256;

/// Encodes a control packet (starting command, content, finishing command).
pub fn encode_control(packet: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(packet.len() * 8);
    for (i, &b) in packet.iter().enumerate() {
        // The first and last byte of a packet are sent as "invalid".
        let valid = i != 0 && i != packet.len() - 1;
        let sync = 0x40 | if valid { 0x08 } else { 0 } | (b >> 7);
        out.extend_from_slice(&[0x00, 0x80, 0x80, 0x80]);
        out.extend_from_slice(&[sync, 0x80, 0x80, 0x80 | (b & 0x7f)]);
    }
    out
}

/// Extracts control packets from a received byte stream.
#[derive(Default)]
pub struct ControlDemux {
    sync: Option<u8>,
    frame_no: usize,
    byte_no: usize,
    packet: Option<Vec<u8>>,
}

impl ControlDemux {
    /// Feeds one byte, returns a complete packet (including the starting and
    /// finishing command).
    pub fn push(&mut self, c: u8) -> Option<Vec<u8>> {
        if c & 0x80 == 0 {
            self.frame_no = if c & 0x40 == 0 { 0 } else { self.frame_no + 1 };
            self.byte_no = 0;
            self.sync = Some(c);
            return None;
        }
        self.byte_no += 1;
        let sync = self.sync?;
        if self.frame_no != 1 || self.byte_no != 3 {
            return None;
        }

        let byte = (c & 0x7f) | ((sync & 1) << 7);
        if sync & 0x08 != 0 {
            if let Some(p) = &mut self.packet {
                if p.len() < MAX_PACKET_LEN {
                    p.push(byte);
                } else {
                    self.packet = None;
                }
            }
        } else if byte & 0x80 == 0 {
            // Starting command. 0x00 is NOP, which stands alone.
            if byte != 0 {
                self.packet = Some(vec![byte]);
            }
        } else if let Some(mut p) = self.packet.take() {
            if p[0] | 0x80 == byte {
                p.push(byte);
                return Some(p);
            }
            debug!("control packet mismatch: {:02x} / {byte:02x}", p[0]);
        }
        None
    }
}

/// Sends a control packet and returns the device's answer to it.
fn query(link: &mut dyn Link, packet: &[u8], attempts: usize) -> Result<Vec<u8>> {
    for _ in 0..attempts {
        link.send(&encode_control(packet))?;
        let mut demux = ControlDemux::default();
        let deadline = Instant::now() + REPLY_TIMEOUT;
        while let Some(timeout) = deadline.checked_duration_since(Instant::now()) {
            let Some(b) = link.recv(timeout)? else { break };
            let Some(answer) = demux.push(b) else { continue };
            match answer[0] {
                cmd if cmd == packet[0] => return Ok(answer),
                CMD_NOT_SUPPORTED => bail!("device does not support command 0x{:02x}", packet[0]),
                _ => debug!("ignoring control packet {answer:02x?}"),
            }
        }
    }
    bail!("device did not answer command 0x{:02x}", packet[0])
}

/// Version info reported by the running firmware.
#[derive(Debug, Clone, Copy)]
pub struct FirmwareInfo {
    pub hw: HwInfo,
    pub version_major: u8,
    pub version_minor: u8,
    pub appl_product_type: u8,
}

pub fn get_version(link: &mut dyn Link) -> Result<FirmwareInfo> {
    let answer = query(link, &[CMD_GET_VERSION, CMD_GET_VERSION | 0x80], QUERY_ATTEMPTS)?;
    let x = &answer[1..answer.len() - 1];
    if x.len() < 16 {
        bail!("version answer too short: {answer:02x?}");
    }
    Ok(FirmwareInfo {
        hw: HwInfo::from_bytes(x[..8].try_into().unwrap()),
        version_major: x[9],
        version_minor: x[10],
        appl_product_type: x[13],
    })
}

/// Makes the firmware reset the device into the bootloader.
pub fn start_bootloader(link: &mut dyn Link) -> Result<()> {
    // Not repeated: a second request would go to the bootloader.
    query(link, &[CMD_START_BOOTLOADER, CMD_START_BOOTLOADER | 0x80], 1).map(drop)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_like_documentation_example() {
        // ARE YOU THERE, from "Example of communication (computer to firmware)".
        let seq = encode_control(&[0x7e, 0xfe]);
        assert_eq!(seq[4..8], [0x40, 0x80, 0x80, 0xfe]);
        assert_eq!(seq[12..16], [0x41, 0x80, 0x80, 0xfe]);
    }

    #[test]
    fn demux_roundtrip() {
        let packet = [0x05, 0x00, 0x01, 0x17, 0x80, 0xff, 0x7f, 0x85];
        let mut demux = ControlDemux::default();
        // Start mid-sequence and with a NOP to check synchronization.
        let mut stream = vec![0x81, 0x82, 0x00, 0x80, 0x80, 0x80, 0x40, 0x80, 0x80, 0x80];
        stream.extend(encode_control(&packet));
        let found: Vec<_> = stream.iter().filter_map(|&b| demux.push(b)).collect();
        assert_eq!(found, [packet.to_vec()]);
    }

    #[test]
    fn demux_ignores_other_frames() {
        // R1 RADIO data and FLAGS in frame0 must not end up in control packets.
        let mut stream = encode_control(&[0x06, 0x86]);
        stream.splice(8..8, [0x28, 0xbf, 0x80, 0x88, 0x40, 0x80, 0x80, 0x80]);
        let mut demux = ControlDemux::default();
        let found: Vec<_> = stream.iter().filter_map(|&b| demux.push(b)).collect();
        assert_eq!(found, [vec![0x06, 0x86]]);
    }
}
