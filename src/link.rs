//! Byte transport to the device.

use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use log::trace;
use serialport::{ClearBuffer, DataBits, FlowControl, Parity, SerialPort, StopBits, TTYPort};

pub trait Link {
    fn send(&mut self, data: &[u8]) -> io::Result<()>;
    /// Next received byte, or `None` if nothing arrives within `timeout`.
    fn recv(&mut self, timeout: Duration) -> io::Result<Option<u8>>;
    fn discard_input(&mut self) -> io::Result<()>;
}

/// The device's FTDI UART: 230400 bps, 8N1, no handshake.
pub struct SerialLink {
    port: TTYPort,
    rx: VecDeque<u8>,
}

impl SerialLink {
    pub fn open(path: &Path) -> Result<Self> {
        // Opened exclusively (TIOCEXCL), so this fails while mhuxd holds the port.
        let port = serialport::new(path.to_string_lossy(), 230_400)
            .data_bits(DataBits::Eight)
            .parity(Parity::None)
            .stop_bits(StopBits::One)
            .flow_control(FlowControl::None)
            .open_native()
            .with_context(|| format!("cannot open {} (is mhuxd running?)", path.display()))?;
        let mut link = SerialLink { port, rx: VecDeque::new() };
        link.discard_input()?;
        Ok(link)
    }
}

impl Link for SerialLink {
    fn send(&mut self, data: &[u8]) -> io::Result<()> {
        trace!("tx {data:02x?}");
        self.port.write_all(data)?;
        self.port.flush()
    }

    fn recv(&mut self, timeout: Duration) -> io::Result<Option<u8>> {
        if self.rx.is_empty() {
            self.port.set_timeout(timeout)?;
            let mut buf = [0; 512];
            match self.port.read(&mut buf) {
                Ok(n) => {
                    trace!("rx {:02x?}", &buf[..n]);
                    self.rx.extend(&buf[..n]);
                }
                Err(e) if e.kind() == io::ErrorKind::TimedOut => {}
                Err(e) => return Err(e),
            }
        }
        Ok(self.rx.pop_front())
    }

    fn discard_input(&mut self) -> io::Result<()> {
        self.rx.clear();
        Ok(self.port.clear(ClearBuffer::Input)?)
    }
}
