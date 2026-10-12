//! `install` / `uninstall`: the binary, a systemd service and a udev rule, all
//! system-wide. One privileged step through pkexec (or sudo).

use std::io::{self, BufRead, IsTerminal, Write};
use std::path::Path;
use std::process::Command;

const BIN: &str = "/usr/local/bin/basilisk-battery";
const UNIT: &str = "/etc/systemd/system/basilisk-battery.service";
const UDEV_RULE: &str = "/etc/udev/rules.d/60-basilisk-battery.rules";

const UNIT_BODY: &str = include_str!("../dist/basilisk-battery.service");
const UDEV_RULE_BODY: &str = include_str!("../dist/60-basilisk-battery.rules");

pub fn install(yes: bool) -> u8 {
    println!("basilisk-battery install\n");
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            println!("cannot locate this binary: {e}");
            return 1;
        }
    };

    println!("This writes, as root:");
    println!("  {BIN} (this binary)");
    println!("  {UNIT}: system service that polls the mouse and feeds the virtual battery");
    println!("  {UDEV_RULE}:\n{}", indent(UDEV_RULE_BODY));
    println!("then enables and starts basilisk-battery.service.\n");
    if !confirm("Apply with pkexec/sudo?", yes) {
        println!("Skipped.");
        return 1;
    }

    let script = format!(
        "set -e; \
         install -m755 {exe} {BIN}.new && mv -f {BIN}.new {BIN}; \
         printf '%s' {unit} > {UNIT}; \
         printf '%s' {rule} > {UDEV_RULE}; \
         udevadm control --reload-rules; \
         systemctl daemon-reload; \
         systemctl enable basilisk-battery.service; \
         systemctl restart basilisk-battery.service",
        exe = sh_quote(&exe.to_string_lossy()),
        unit = sh_quote(UNIT_BODY),
        rule = sh_quote(UDEV_RULE_BODY),
    );
    if !run_privileged(&script) {
        println!("Privileged step failed.");
        return 1;
    }
    println!("[service] basilisk-battery.service enabled and started");
    println!("\nThe mouse shows up in the power applet within a few seconds while it's on.");
    println!("Check: basilisk-battery status, journalctl -u basilisk-battery");
    0
}

pub fn uninstall(yes: bool) -> u8 {
    println!("basilisk-battery uninstall\n");
    let present: Vec<&str> = [BIN, UNIT, UDEV_RULE].into_iter().filter(|p| Path::new(p).exists()).collect();
    if present.is_empty() {
        println!("Nothing installed.");
        return 0;
    }
    println!("This stops the service and removes, as root:");
    for p in &present {
        println!("  {p}");
    }
    if !confirm("Apply with pkexec/sudo?", yes) {
        println!("Skipped.");
        return 1;
    }
    let script = format!(
        "systemctl disable --now basilisk-battery.service 2>/dev/null; \
         rm -f {BIN} {UNIT} {UDEV_RULE}; \
         systemctl daemon-reload; udevadm control --reload-rules"
    );
    if !run_privileged(&script) {
        println!("Privileged step failed.");
        return 1;
    }
    println!("[remove] done");
    0
}

fn confirm(question: &str, yes: bool) -> bool {
    if yes {
        println!("{question} [y/N] y");
        return true;
    }
    if !io::stdin().is_terminal() {
        println!("{question} [y/N] (not a terminal: no; pass --yes to accept)");
        return false;
    }
    print!("{question} [y/N] ");
    let _ = io::stdout().flush();
    let mut line = String::new();
    let _ = io::stdin().lock().read_line(&mut line);
    matches!(line.trim(), "y" | "Y" | "yes")
}

/// Run a shell snippet as root: directly if we already are, else via pkexec (or sudo).
fn run_privileged(script: &str) -> bool {
    let mut cmd = if unsafe { libc::geteuid() } == 0 {
        Command::new("sh")
    } else {
        let mut c = Command::new(if which("pkexec") { "pkexec" } else { "sudo" });
        c.arg("sh");
        c
    };
    cmd.args(["-c", script]).status().is_ok_and(|s| s.success())
}

fn which(cmd: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(cmd).is_file()))
}

fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn indent(s: &str) -> String {
    s.lines().map(|l| format!("    {l}\n")).collect()
}
