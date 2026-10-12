//! Razer mouse protocol, as spoken by the Basilisk V3 X HyperSpeed's 2.4 GHz receiver
//! (1532:00b9).
//!
//! Commands are 90-byte HID feature reports (no report ID) on USB interface 0: a
//! SET_REPORT carries the request, and after a short pause a GET_REPORT returns the
//! receiver's answer, relayed from the mouse over the air.
//!
//! Report layout:
//!   [0] status  [1] transaction id  [2..4] remaining packets  [4] protocol type
//!   [5] data size  [6] command class  [7] command id  [8..88] arguments
//!   [88] XOR of bytes 2..88  [89] reserved
//!
//! Protocol and per-model constants from OpenRazer
//! https://github.com/openrazer/openrazer (driver/razermouse_driver.c)

use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::thread::sleep;
use std::time::Duration;

const HID_ID: &str = "HID_ID=0003:00001532:000000B9";
/// Report Count (90), Feature: the command report of interface 0. The flags byte
/// that follows varies (the receiver declares it Constant), so it is not matched.
const FEATURE_90: [u8; 3] = [0x95, 0x5a, 0xb1];

const REPORT_LEN: usize = 90;
/// The mouse answers through the receiver: OpenRazer's RAZER_NEW_MOUSE_RECEIVER_WAIT_US.
const REPLY_WAIT: Duration = Duration::from_millis(31);
/// Re-read a "busy" reply this many times before giving up.
const BUSY_RETRIES: usize = 5;

const TRANSACTION_ID: u8 = 0x1f;
const CLASS_MISC: u8 = 0x07;
const CMD_BATTERY: u8 = 0x80;

const STATUS_BUSY: u8 = 0x01;
const STATUS_OK: u8 = 0x02;

pub struct Receiver {
    file: File,
}

impl Receiver {
    /// Find the receiver's command hidraw node and open it.
    pub fn open() -> io::Result<Option<Self>> {
        let Some(path) = find_hidraw()? else {
            return Ok(None);
        };
        let file = OpenOptions::new().read(true).write(true).open(&path)?;
        Ok(Some(Self { file }))
    }

    /// Ask the mouse for its battery level in percent. `None` means the mouse did not
    /// answer (switched off, asleep, or in Bluetooth mode). The mouse runs on an AA
    /// battery, so there is no charging state.
    pub fn battery(&mut self) -> io::Result<Option<u8>> {
        let Some(args) = self.query(CLASS_MISC, CMD_BATTERY, 0x02)? else {
            return Ok(None);
        };
        // arguments[1] is the level on a 0..=255 scale.
        Ok(Some(to_percent(args[1])))
    }

    /// Send a GET command and return the reply's arguments on success.
    fn query(&mut self, class: u8, cmd: u8, size: u8) -> io::Result<Option<[u8; 80]>> {
        let req = request(class, cmd, size);
        let mut buf = [0u8; REPORT_LEN + 1];
        buf[1..].copy_from_slice(&req);
        feature_ioctl(&self.file, HIDIOCSFEATURE, &mut buf)?;

        for _ in 0..=BUSY_RETRIES {
            sleep(REPLY_WAIT);
            buf = [0u8; REPORT_LEN + 1];
            feature_ioctl(&self.file, HIDIOCGFEATURE, &mut buf)?;
            let r = &buf[1..];
            if r[6] != class || r[7] != cmd {
                return Ok(None);
            }
            match r[0] {
                STATUS_OK => return Ok(Some(r[8..88].try_into().unwrap())),
                STATUS_BUSY => continue,
                // 0x03 failure, 0x04 timeout (mouse unreachable), 0x05 not supported
                _ => return Ok(None),
            }
        }
        Ok(None)
    }
}

fn request(class: u8, cmd: u8, size: u8) -> [u8; REPORT_LEN] {
    let mut r = [0u8; REPORT_LEN];
    r[1] = TRANSACTION_ID;
    r[5] = size;
    r[6] = class;
    r[7] = cmd;
    r[88] = r[2..88].iter().fold(0, |x, b| x ^ b);
    r
}

/// 0..=255 -> 0..=100, rounded.
fn to_percent(raw: u8) -> u8 {
    ((raw as u32 * 100 + 127) / 255) as u8
}

// _IOC(_IOC_READ | _IOC_WRITE, 'H', nr, len), <linux/hidraw.h>
const HIDIOCSFEATURE: u32 = 0x06;
const HIDIOCGFEATURE: u32 = 0x07;

/// Run a hidraw feature report ioctl. `buf[0]` is the report number (0: unnumbered),
/// which the kernel strips before talking to the device.
fn feature_ioctl(file: &File, nr: u32, buf: &mut [u8]) -> io::Result<()> {
    let req = (3u32 << 30) | ((buf.len() as u32) << 16) | ((b'H' as u32) << 8) | nr;
    let rc = unsafe { libc::ioctl(file.as_raw_fd(), req as _, buf.as_mut_ptr()) };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn find_hidraw() -> io::Result<Option<PathBuf>> {
    for entry in fs::read_dir("/sys/class/hidraw")? {
        let entry = entry?;
        let dev = entry.path().join("device");
        let Ok(uevent) = fs::read_to_string(dev.join("uevent")) else {
            continue;
        };
        if !uevent.lines().any(|l| l == HID_ID) {
            continue;
        }
        let rdesc = fs::read(dev.join("report_descriptor")).unwrap_or_default();
        if rdesc.windows(FEATURE_90.len()).any(|w| w == FEATURE_90) {
            return Ok(Some(PathBuf::from("/dev").join(entry.file_name())));
        }
    }
    Ok(None)
}

pub(crate) fn poll_readable(file: &File, timeout: Duration) -> io::Result<bool> {
    let mut pfd = libc::pollfd { fd: file.as_raw_fd(), events: libc::POLLIN, revents: 0 };
    let ms = timeout.as_millis().min(i32::MAX as u128) as i32;
    let rc = unsafe { libc::poll(&mut pfd, 1, ms) };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    if pfd.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
        return Err(io::Error::new(io::ErrorKind::BrokenPipe, "uhid device gone"));
    }
    Ok(rc > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn battery_request() {
        let r = request(CLASS_MISC, CMD_BATTERY, 0x02);
        assert_eq!(&r[..8], &[0x00, 0x1f, 0x00, 0x00, 0x00, 0x02, 0x07, 0x80]);
        // 0x02 ^ 0x07 ^ 0x80
        assert_eq!(r[88], 0x85);
        assert_eq!(r[89], 0x00);
    }

    #[test]
    fn percent_scale() {
        assert_eq!(to_percent(0), 0);
        assert_eq!(to_percent(255), 100);
        assert_eq!(to_percent(128), 50);
        assert_eq!(to_percent(1), 0);
        assert_eq!(to_percent(2), 1);
    }

    #[test]
    fn ioctl_numbers() {
        let req = |nr: u32, len: u32| (3u32 << 30) | (len << 16) | ((b'H' as u32) << 8) | nr;
        // HIDIOCSFEATURE(91) / HIDIOCGFEATURE(91)
        assert_eq!(req(HIDIOCSFEATURE, 91), 0xc05b4806);
        assert_eq!(req(HIDIOCGFEATURE, 91), 0xc05b4807);
    }
}
