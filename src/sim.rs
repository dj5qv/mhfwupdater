//! Simulated device (firmware and bootloader) for tests.

use std::collections::VecDeque;
use std::io;
use std::time::Duration;

use crate::bootloader::*;
use crate::devices::family;
use crate::keyer::{ControlDemux, encode_control};
use crate::link::Link;

#[derive(Debug, PartialEq)]
enum State {
    Firmware,
    WaitStart,
    WaitGetVersion,
    Pages,
}

pub struct SimDevice {
    state: State,
    demux: ControlDemux,
    tx: VecDeque<u8>,
    page: Option<Vec<u8>>,
    page_len: usize,
    hw: [u8; 8],
    attempts: usize,
    /// Answers to page write attempts (counted from 0); default is WRITE_OK.
    pub answers: Vec<(usize, u8)>,
    /// Answer CHECKSUM_ERR to pages with an all zero header.
    pub reject_invalidate: bool,
    pub written: Vec<Vec<u8>>,
}

impl SimDevice {
    pub fn new(product_type: u8) -> Self {
        SimDevice {
            state: State::Firmware,
            demux: ControlDemux::default(),
            tx: VecDeque::new(),
            page: None,
            page_len: family(product_type).unwrap().page_len,
            hw: [3, 2, product_type, 1, 1, 1, 0, 0],
            attempts: 0,
            answers: Vec::new(),
            reject_invalidate: false,
            written: Vec::new(),
        }
    }

    pub fn reset(&mut self) {
        self.state = State::WaitStart;
    }

    pub fn running_firmware(&self) -> bool {
        self.state == State::Firmware
    }

    fn control_packet(&mut self, p: &[u8]) {
        match p[0] {
            0x05 => {
                let mut answer = vec![0x05];
                answer.extend_from_slice(&self.hw);
                answer.extend_from_slice(&[0, 2, 11, 0, 0, self.hw[2], 0, 0, 0x85]);
                self.tx.extend(encode_control(&answer));
            }
            0x06 => {
                self.tx.extend(encode_control(&[0x06, 0x86]));
                self.reset();
            }
            _ => {}
        }
    }

    fn page_complete(&mut self, page: Vec<u8>) {
        let attempt = self.attempts;
        self.attempts += 1;
        let answer = if self.reject_invalidate && page[..4] == [0; 4] {
            CHECKSUM_ERR
        } else {
            self.answers
                .iter()
                .find(|(n, _)| *n == attempt)
                .map_or(WRITE_OK, |&(_, a)| a)
        };
        if answer == WRITE_OK {
            self.written.push(page);
        }
        self.tx.push_back(answer);
    }

    fn receive(&mut self, b: u8) {
        if let Some(page) = &mut self.page {
            page.push(b);
            if page.len() == self.page_len {
                let page = self.page.take().unwrap();
                self.page_complete(page);
            }
            return;
        }
        match self.state {
            State::Firmware => {
                if let Some(p) = self.demux.push(b) {
                    self.control_packet(&p);
                }
            }
            State::WaitStart | State::WaitGetVersion if b == START => {
                self.tx.push_back(STARTED);
                self.state = State::WaitGetVersion;
            }
            State::WaitGetVersion if b == GET_VERSION => {
                self.tx.push_back(VERSION);
                self.tx.extend(self.hw.map(|x| x | 0x80));
                self.state = State::Pages;
            }
            State::Pages if b == NEXT_PAGE => self.page = Some(Vec::new()),
            _ => {}
        }
    }
}

impl Link for SimDevice {
    fn send(&mut self, data: &[u8]) -> io::Result<()> {
        data.iter().for_each(|&b| self.receive(b));
        Ok(())
    }

    fn recv(&mut self, _timeout: Duration) -> io::Result<Option<u8>> {
        if let Some(b) = self.tx.pop_front() {
            return Ok(Some(b));
        }
        // The computer went quiet, so the bootloader times out.
        if self.state == State::Pages && self.page.is_none() {
            self.state = State::Firmware;
            return Ok(Some(TERMINATED));
        }
        Ok(None)
    }

    fn discard_input(&mut self) -> io::Result<()> {
        self.tx.clear();
        Ok(())
    }
}
