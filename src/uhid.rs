//! Virtual HID device carrying a standard Battery Strength usage. The kernel's
//! hid-input turns it into a `power_supply` (hid-<uniq>-battery) that UPower picks up,
//! the same way it does for Bluetooth keyboards and mice.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};

// struct uhid_event types, <linux/uhid.h>
const UHID_DESTROY: u32 = 1;
const UHID_GET_REPORT: u32 = 9;
const UHID_GET_REPORT_REPLY: u32 = 10;
const UHID_CREATE2: u32 = 11;
const UHID_INPUT2: u32 = 12;
const UHID_SET_REPORT: u32 = 13;
const UHID_SET_REPORT_REPLY: u32 = 14;

/// sizeof(struct uhid_event): u32 type + the largest union member (uhid_create2_req).
const EVENT_SIZE: usize = 4 + 128 + 64 + 64 + 2 + 2 + 4 * 4 + 4096;
const BUS_VIRTUAL: u16 = 0x06;

const REPORT_ID: u8 = 0x01;

#[rustfmt::skip]
const DESCRIPTOR: &[u8] = &[
    0x05, 0x0c,       // Usage Page (Consumer)
    0x09, 0x01,       // Usage (Consumer Control)
    0xa1, 0x01,       // Collection (Application)
    0x85, REPORT_ID,  //   Report ID
    // Battery Strength must be the first byte: older kernels read the level of a
    // GET_REPORT reply from buf[1] regardless of the descriptor.
    0x05, 0x06,       //   Usage Page (Generic Device Controls)
    0x09, 0x20,       //   Usage (Battery Strength)
    0x15, 0x00,       //   Logical Minimum (0)
    0x25, 0x64,       //   Logical Maximum (100)
    0x75, 0x08,       //   Report Size (8)
    0x95, 0x01,       //   Report Count (1)
    0x81, 0x02,       //   Input (Data,Var,Abs)
    // hid-input only keeps a battery if the device also gets an input node,
    // so carry one (never pressed) Mute key.
    0x05, 0x0c,       //   Usage Page (Consumer)
    0x09, 0xe2,       //   Usage (Mute)
    0x25, 0x01,       //   Logical Maximum (1)
    0x75, 0x01,       //   Report Size (1)
    0x81, 0x02,       //   Input (Data,Var,Abs)
    0x75, 0x07,       //   Report Size (7)
    0x81, 0x03,       //   Input (Const) padding
    0xc0,             // End Collection
];

pub struct VirtualBattery {
    file: File,
    report: [u8; 3],
}

impl VirtualBattery {
    pub fn create(name: &str, uniq: &str, vendor: u32, product: u32) -> io::Result<Self> {
        let mut file = OpenOptions::new().read(true).write(true).open("/dev/uhid")?;

        let mut ev = vec![0u8; EVENT_SIZE];
        ev[0..4].copy_from_slice(&UHID_CREATE2.to_ne_bytes());
        put_str(&mut ev[4..132], name);
        put_str(&mut ev[132..196], "basilisk-battery");
        put_str(&mut ev[196..260], uniq);
        ev[260..262].copy_from_slice(&(DESCRIPTOR.len() as u16).to_ne_bytes());
        ev[262..264].copy_from_slice(&BUS_VIRTUAL.to_ne_bytes());
        ev[264..268].copy_from_slice(&vendor.to_ne_bytes());
        ev[268..272].copy_from_slice(&product.to_ne_bytes());
        ev[280..280 + DESCRIPTOR.len()].copy_from_slice(DESCRIPTOR);
        file.write_all(&ev)?;

        Ok(Self { file, report: [REPORT_ID, 0, 0] })
    }

    pub fn fd(&self) -> &File {
        &self.file
    }

    pub fn update(&mut self, percent: u8) -> io::Result<()> {
        self.report = [REPORT_ID, percent, 0];

        let mut ev = [0u8; 4 + 2 + 3];
        ev[0..4].copy_from_slice(&UHID_INPUT2.to_ne_bytes());
        ev[4..6].copy_from_slice(&(self.report.len() as u16).to_ne_bytes());
        ev[6..].copy_from_slice(&self.report);
        self.file.write_all(&ev)
    }

    /// Handle one pending kernel event. hid-input issues GET_REPORT when something
    /// reads the battery before an input report arrived.
    pub fn handle_event(&mut self) -> io::Result<()> {
        let mut ev = vec![0u8; EVENT_SIZE];
        let n = self.file.read(&mut ev)?;
        if n < 4 {
            return Ok(());
        }
        match u32::from_ne_bytes(ev[0..4].try_into().unwrap()) {
            UHID_GET_REPORT if n >= 10 => {
                let rnum = ev[8];
                let mut reply = [0u8; 12 + 3];
                reply[0..4].copy_from_slice(&UHID_GET_REPORT_REPLY.to_ne_bytes());
                reply[4..8].copy_from_slice(&ev[4..8]); // request id
                if rnum == REPORT_ID {
                    reply[10..12].copy_from_slice(&(self.report.len() as u16).to_ne_bytes());
                    reply[12..].copy_from_slice(&self.report);
                } else {
                    reply[8..10].copy_from_slice(&(libc::EIO as u16).to_ne_bytes());
                }
                self.file.write_all(&reply)
            }
            UHID_SET_REPORT if n >= 8 => {
                let mut reply = [0u8; 10];
                reply[0..4].copy_from_slice(&UHID_SET_REPORT_REPLY.to_ne_bytes());
                reply[4..8].copy_from_slice(&ev[4..8]);
                reply[8..10].copy_from_slice(&(libc::EIO as u16).to_ne_bytes());
                self.file.write_all(&reply)
            }
            _ => Ok(()),
        }
    }
}

impl Drop for VirtualBattery {
    fn drop(&mut self) {
        // Closing the fd destroys the device too; be explicit anyway.
        let _ = self.file.write_all(&UHID_DESTROY.to_ne_bytes());
    }
}

fn put_str(dst: &mut [u8], s: &str) {
    let n = s.len().min(dst.len() - 1);
    dst[..n].copy_from_slice(&s.as_bytes()[..n]);
}
