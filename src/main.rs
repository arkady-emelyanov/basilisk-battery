mod install;
mod receiver;
mod uhid;

use std::io;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use receiver::Receiver;
use uhid::VirtualBattery;

const NAME: &str = "Razer Basilisk V3 X HyperSpeed";
/// Becomes the power_supply name: hid-basilisk-v3-x-hyperspeed-battery
const UNIQ: &str = "basilisk-v3-x-hyperspeed";
const VENDOR: u32 = 0x1532;
const PRODUCT: u32 = 0x00b9;

/// An AA battery drains slowly; one small request a minute is plenty.
const DEFAULT_INTERVAL: Duration = Duration::from_secs(60);

/// The mouse stops answering a few minutes after its last movement. Keep showing the
/// last level that long; after that it is more likely switched off than asleep.
const KEEP_WHILE_SILENT: Duration = Duration::from_secs(60 * 60);

/// hid-input may drop the first input report if it arrives before the virtual device
/// finished probing, so repeat it shortly after creation.
const RESEND_AFTER_CREATE: Duration = Duration::from_millis(250);

const USAGE: &str = "\
usage: basilisk-battery <command>

commands:
  run [--interval SECONDS]  poll the mouse and publish its battery (default every 60s)
  status                    print the mouse battery level once
  install [--yes]           install the binary, systemd service and udev rule (asks for root)
  uninstall [--yes]         remove them again

All commands except install/uninstall need root (the service runs as root).";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let result = match args.as_slice() {
        ["--version" | "-V"] => {
            println!("basilisk-battery {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        ["--help" | "-h" | "help"] => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        ["install", rest @ ..] => {
            return parse_yes(rest).map_or_else(usage, |yes| ExitCode::from(install::install(yes)));
        }
        ["uninstall", rest @ ..] => {
            return parse_yes(rest).map_or_else(usage, |yes| ExitCode::from(install::uninstall(yes)));
        }
        ["status"] => print_status(),
        ["run"] => run(DEFAULT_INTERVAL),
        ["run", "--interval", secs] => match secs.parse::<u64>() {
            Ok(s) if s > 0 => run(Duration::from_secs(s)),
            _ => return usage(),
        },
        _ => return usage(),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn usage() -> ExitCode {
    eprintln!("{USAGE}");
    ExitCode::from(2)
}

fn parse_yes(rest: &[&str]) -> Option<bool> {
    match rest {
        [] => Some(false),
        ["--yes" | "-y"] => Some(true),
        _ => None,
    }
}

fn print_status() -> io::Result<()> {
    let Some(mut receiver) = Receiver::open()? else {
        return Err(io::Error::other("receiver 1532:00b9 not found"));
    };
    match receiver.battery()? {
        Some(percent) => println!("{percent}%"),
        None => println!("mouse not responding"),
    }
    Ok(())
}

fn run(interval: Duration) -> io::Result<()> {
    let mut battery: Option<VirtualBattery> = None;
    // Level currently published, and when the mouse last answered.
    let mut last: Option<u8> = None;
    let mut last_seen = Instant::now();
    let mut silent = false;
    let mut next_poll = Instant::now();
    let mut resend_at: Option<Instant> = None;

    loop {
        let now = Instant::now();
        if now >= next_poll {
            next_poll = now + interval;
            let state = match poll_mouse() {
                Poll::Level(p) => {
                    last_seen = now;
                    silent = false;
                    Some(p)
                }
                // The mouse stops answering while asleep: keep the last level for a while.
                Poll::Silent if last.is_some() && now.duration_since(last_seen) < KEEP_WHILE_SILENT => {
                    if !silent {
                        eprintln!("mouse not answering (asleep?), keeping {}%", last.unwrap());
                        silent = true;
                    }
                    last
                }
                Poll::Silent | Poll::NoReceiver => None,
            };
            if state != last {
                match state {
                    Some(p) => eprintln!("battery {p}%"),
                    None => eprintln!("mouse unavailable"),
                }
                last = state;
            }
            match state {
                Some(p) => {
                    if battery.is_none() {
                        battery = Some(VirtualBattery::create(NAME, UNIQ, VENDOR, PRODUCT)?);
                        resend_at = Some(now + RESEND_AFTER_CREATE);
                    }
                    battery.as_mut().unwrap().update(p)?;
                }
                // Removing the virtual device makes the applet drop the entry.
                None => battery = None,
            }
        }
        if let (Some(at), Some(vb), Some(p)) = (resend_at, battery.as_mut(), last)
            && now >= at
        {
            vb.update(p)?;
            resend_at = None;
        }

        let wake = resend_at.map_or(next_poll, |r| r.min(next_poll));
        let timeout = wake.saturating_duration_since(Instant::now());
        match battery.as_mut() {
            Some(vb) => {
                if receiver::poll_readable(vb.fd(), timeout)? {
                    vb.handle_event()?;
                }
            }
            None => std::thread::sleep(timeout),
        }
    }
}

enum Poll {
    Level(u8),
    /// The receiver is there but the mouse did not answer: asleep or switched off.
    Silent,
    NoReceiver,
}

/// Query the mouse. Looks the receiver up afresh every time, so it survives being
/// unplugged and replugged.
fn poll_mouse() -> Poll {
    let result = Receiver::open().and_then(|r| match r {
        Some(mut r) => Ok(r.battery()?.map_or(Poll::Silent, Poll::Level)),
        None => Ok(Poll::NoReceiver),
    });
    result.unwrap_or_else(|e| {
        eprintln!("mouse: {e}");
        Poll::NoReceiver
    })
}
