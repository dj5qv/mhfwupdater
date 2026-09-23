# mhfwupdater

Firmware updater for microHAM USB devices on Linux. It writes the firmware
files (`*.cbl`) distributed by microHAM, which microHAM Router does on Windows.

Status: tested on a micro KEYER III (downgrade 2.12 → 2.11 and upgrade back
to 2.12). The other devices are untested.

## Supported devices

| Device                          | Firmware files |
|---------------------------------|----------------|
| micro KEYER, CW KEYER, DIGI KEYER | `mmk_*.cbl`  |
| micro KEYER II                  | `mpk_*.cbl`    |
| micro KEYER III                 | `mk3_*.cbl`    |
| DIGI KEYER II                   | `dk2_*.cbl`    |
| micro KEYER 2R, 2R+             | `mok_*.cbl`    |
| micro 2R                        | `u2r_*.cbl`    |
| Station Master                  | `sml_*.cbl`    |
| Station Master DeLuxe           | `smd_*.cbl`    |

Before anything is written, the firmware file is checked against the device:
the product type must match and the device's hardware revision must meet the
firmware's requirements. Downgrades and rewriting the installed version are
allowed.

## Requirements

- Linux. The stock `ftdi_sio` kernel driver makes microHAM devices (USB ID
  `0403:eeef`) available as `/dev/ttyUSBn`.
- Read/write access to that tty, usually via membership in the `dialout` group.
- Rust 1.85 or newer. No system libraries are needed.

## Building

```sh
cargo build --release
```

The binary is `target/release/mhfwupdater`. To install it to `~/.cargo/bin`:

```sh
cargo install --path .
```

`cargo test` runs the tests against a simulated device. If there are firmware
files in `firmware/`, the tests also check that they parse.

## Before updating

mhuxd opens the device ports exclusively, so stop it first. This takes all
devices it serves offline until it is started again:

```sh
sudo systemctl stop mhuxd
# update
sudo systemctl start mhuxd
```

The device restarts during the update. Don't transmit while updating.

## Usage

```
mhfwupdater [-v|-vv] <command>
```

`-v` prints debug messages, `-vv` also all bytes sent and received.

### List attached devices

```
$ mhfwupdater list
/dev/ttyUSB0  M32VFFGJ  micro KEYER III
```

### Show a firmware file

```
$ mhfwupdater info mk3_release_02_12.cbl
mk3_release_02_12.cbl
  device:   micro KEYER III (product type 0x17)
  version:  2.12
  requires: hardware 0, mechanical 0
  pages:    1544 x 134 bytes (193.0 KiB of flash)
  comment:  microHAM firmware file
  comment:  application mk3 2.12
```

### Query a device

```
$ mhfwupdater probe
Device:     /dev/ttyUSB0
Firmware:   micro KEYER III 2.12
```

With `--bootloader`, the device also restarts into its bootloader to report
the bootloader version, then returns to its firmware. Nothing is written.

### Update the firmware

```
$ mhfwupdater flash mk3_release_02_12.cbl
Device:     /dev/ttyUSB0
Firmware:   micro KEYER III 2.11
New:        micro KEYER III 2.12, 1544 pages
Protection: none known for this device
Write firmware? Don't interrupt or power off the device while writing. [y/N] y
Bootloader: 3.2, product type 0x17/0x01, hardware 0, mechanical 0
Writing:    page 1544/1544 (100%)
Firmware:   micro KEYER III 2.12
```

Options:

| Option            | Effect |
|-------------------|--------|
| `-d`, `--device`  | Device to update, by serial number (`-d M32VFFGJ`) or path (`-d /dev/ttyUSB1`) |
| `--dry-run`       | Check the file against the device and enter the bootloader, but write nothing |
| `--no-invalidate` | Don't use the protection against interrupted updates (see below) |
| `--recover`       | Wait for the bootloader instead of asking the firmware to restart (see below) |
| `-y`, `--yes`     | Don't ask for confirmation |

### Several devices

Without `-d`, `probe` and `flash` refuse to run when more than one device is
attached. Select one with `-d`. The serial number (see `list`) is the better
choice, because `ttyUSB` numbers can change when devices are replugged.

## Interrupted updates

If an update is interrupted (power loss, USB unplugged, program killed), the
device is left with incomplete firmware and no longer answers `probe`. The
bootloader still starts at every power-up, so the update can be finished:

```sh
mhfwupdater flash --recover mk3_release_02_12.cbl
```

Power-cycle the device when asked. The tool waits up to 60 seconds for the
bootloader.

### Protection

To keep an interrupted update from starting half-written firmware, the tool
first writes a placeholder start page that jumps straight back into the
bootloader, and writes the real start page last. A device interrupted this
way stays in its bootloader, and `--recover` should find it without a power
cycle.

This protection follows microHAM's documentation and hasn't been tested on
hardware yet. It is off for the micro KEYER III, because the documented
placeholder page doesn't fit that device's memory layout. If a device rejects
the placeholder page, nothing is written and the update continues without
the protection.

## Troubleshooting

| Message | Cause |
|---------|-------|
| `cannot open /dev/ttyUSB0 (is mhuxd running?)` | mhuxd or another program has the port open, or you lack access (`dialout` group) |
| `no answer from the device firmware` | Device switched off, or its firmware is incomplete: use `--recover` |
| `bootloader did not answer` | Try again. If it keeps failing, run with `-vv` to see the traffic |

## Source layout

| File                | Contents |
|---------------------|----------|
| `src/main.rs`       | Command line interface |
| `src/cbl.rs`        | Firmware file parser |
| `src/devices.rs`    | Device families and compatibility checks |
| `src/discover.rs`   | Finding devices through sysfs |
| `src/link.rs`       | Serial port (230400 bps, 8N1) |
| `src/keyer.rs`      | Firmware protocol: version query, restart into the bootloader |
| `src/bootloader.rs` | Bootloader protocol: handshake, page writing |
| `src/sim.rs`        | Simulated device for the tests |
