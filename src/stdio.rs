//! The host's standard input as the serial console's receive line
//! (`boot --stdin`). On a terminal the input side goes into raw mode for the
//! session, so every key — Ctrl-C included — reaches the guest and the guest's
//! own tty does the echoing; `Ctrl-A x` ends the session and `Ctrl-A Ctrl-A`
//! sends a literal Ctrl-A. Piped input passes through with no escape character.

use std::io::{IsTerminal, Read};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver};

const CTRL_A: u8 = 0x01;

pub enum HostEvent {
    Bytes(Vec<u8>),
    Quit,
}

pub struct HostInput {
    rx: Receiver<HostEvent>,
    /// `stty -g` from before raw mode, to put back.
    saved_tty: Option<String>,
}

impl HostInput {
    pub fn stdin() -> HostInput {
        let saved_tty = if std::io::stdin().is_terminal() {
            eprintln!("serial console on this terminal: keys go to the guest, Ctrl-A x quits");
            raw_mode()
        } else {
            None
        };
        let escapes = saved_tty.is_some();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut stdin = std::io::stdin().lock();
            let mut buf = [0u8; 256];
            let mut escape = false;
            loop {
                let n = match stdin.read(&mut buf) {
                    Ok(0) | Err(_) => return,
                    Ok(n) => n,
                };
                let mut out = Vec::with_capacity(n);
                let mut quit = false;
                for &b in &buf[..n] {
                    if escape {
                        escape = false;
                        match b {
                            b'x' | b'X' => {
                                quit = true;
                                break;
                            }
                            CTRL_A => out.push(CTRL_A),
                            _ => {}
                        }
                    } else if escapes && b == CTRL_A {
                        escape = true;
                    } else {
                        out.push(b);
                    }
                }
                if !out.is_empty() && tx.send(HostEvent::Bytes(out)).is_err() {
                    return;
                }
                if quit {
                    let _ = tx.send(HostEvent::Quit);
                    return;
                }
            }
        });
        HostInput { rx, saved_tty }
    }

    /// The next thing typed, if any. Never blocks.
    pub fn poll(&mut self) -> Option<HostEvent> {
        self.rx.try_recv().ok()
    }
}

impl Drop for HostInput {
    fn drop(&mut self) {
        if let Some(saved) = &self.saved_tty {
            let _ = Command::new("stty").arg(saved).status();
        }
    }
}

/// Raw input, no local echo; output post-processing stays on so the firmware's
/// bare newlines still return the carriage.
fn raw_mode() -> Option<String> {
    let saved = Command::new("stty")
        .arg("-g")
        .stdin(Stdio::inherit())
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    let saved = String::from_utf8(saved.stdout).ok()?.trim().to_string();
    Command::new("stty")
        .args(["raw", "-echo", "opost"])
        .status()
        .ok()
        .filter(|s| s.success())?;
    Some(saved)
}
