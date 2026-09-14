//! A host network behind a stream socket: [passt](https://passt.top/), or
//! anything else that speaks QEMU's `-netdev stream` framing (#45).
//!
//! Each Ethernet frame travels as a 32-bit big-endian length and then the
//! frame, over a UNIX stream socket. passt NATs that to the host's network and
//! maps the host's loopback, so the guest can reach a server on the host –
//! `mkosi serve`, say – or the outside world:
//!
//! ```text
//! passt -f -s /tmp/passt.socket &
//! rpi-virt-fw boot --eeprom pieeprom.bin --net passt:/tmp/passt.socket --bootconf BOOT_ORDER=0xf2
//! ```
//!
//! Unlike [`super::BuiltinPeer`], the other end runs on the host's clock: a
//! frame can arrive at any time, and the guest runs far slower than real time,
//! so a host's TCP retransmit timers will fire where a real board's would not.
//! That makes a run over a real network neither deterministic nor fit for a
//! golden transcript – the boot scenarios stay on the built-in peer.

use std::io::{ErrorKind, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;

use super::NetBackend;

/// Frames longer than this are not Ethernet: the stream has lost its framing.
const MAX_FRAME: usize = 65_536;

pub struct StreamBackend {
    sock: UnixStream,
    /// Bytes read but not yet a whole frame.
    rx: Vec<u8>,
    /// Framed bytes the socket has not taken yet.
    tx: Vec<u8>,
    /// Set once the other end went away, or broke the framing.
    closed: bool,
    frames_in: u64,
    frames_out: u64,
    log: Vec<String>,
}

impl StreamBackend {
    /// Connect to the UNIX socket at `path` (passt's `-s`).
    pub fn connect(path: &Path) -> std::io::Result<StreamBackend> {
        StreamBackend::from_stream(UnixStream::connect(path)?)
    }

    /// Talk over an already connected socket.
    pub fn from_stream(sock: UnixStream) -> std::io::Result<StreamBackend> {
        // Never block the run loop: the guest's clock only moves while it runs.
        sock.set_nonblocking(true)?;
        Ok(StreamBackend {
            sock,
            rx: Vec::new(),
            tx: Vec::new(),
            closed: false,
            frames_in: 0,
            frames_out: 0,
            log: Vec::new(),
        })
    }

    fn close(&mut self, why: String) {
        if !self.closed {
            self.closed = true;
            self.log.push(why);
        }
    }

    /// Hand the socket as much of [`Self::tx`] as it takes without blocking.
    fn flush_tx(&mut self) {
        while !self.tx.is_empty() && !self.closed {
            match self.sock.write(&self.tx) {
                Ok(0) => self.close("the other end closed the socket".into()),
                Ok(n) => {
                    self.tx.drain(..n);
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(e) => self.close(format!("writing the socket: {e}")),
            }
        }
    }

    /// Read whatever has arrived, without blocking.
    fn fill_rx(&mut self) {
        let mut buf = [0u8; 16 * 1024];
        while !self.closed {
            match self.sock.read(&mut buf) {
                Ok(0) => self.close("the other end closed the socket".into()),
                Ok(n) => self.rx.extend_from_slice(&buf[..n]),
                Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(e) => self.close(format!("reading the socket: {e}")),
            }
        }
    }
}

impl NetBackend for StreamBackend {
    fn name(&self) -> &'static str {
        "stream socket"
    }

    fn send(&mut self, frame: &[u8]) {
        if self.closed {
            return;
        }
        self.tx
            .extend_from_slice(&(frame.len() as u32).to_be_bytes());
        self.tx.extend_from_slice(frame);
        self.frames_out += 1;
        self.flush_tx();
    }

    fn recv(&mut self) -> Option<Vec<u8>> {
        self.flush_tx();
        if self.rx.len() < 4 {
            self.fill_rx();
        }
        let len = u32::from_be_bytes(self.rx.get(..4)?.try_into().unwrap()) as usize;
        if len > MAX_FRAME {
            self.close(format!("a {len}-byte frame: the stream lost its framing"));
            self.rx.clear();
            return None;
        }
        if self.rx.len() < 4 + len {
            self.fill_rx();
            if self.rx.len() < 4 + len {
                return None;
            }
        }
        let frame = self.rx[4..4 + len].to_vec();
        self.rx.drain(..4 + len);
        self.frames_in += 1;
        Some(frame)
    }

    fn take_log(&mut self) -> Vec<String> {
        let mut log = std::mem::take(&mut self.log);
        log.push(format!(
            "{} frames sent, {} received",
            self.frames_out, self.frames_in
        ));
        log
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_cross_the_socket_with_a_length_prefix() {
        let (ours, mut theirs) = UnixStream::pair().unwrap();
        let mut net = StreamBackend::from_stream(ours).unwrap();
        net.send(&[1, 2, 3]);
        let mut got = [0u8; 7];
        theirs.read_exact(&mut got).unwrap();
        assert_eq!(got, [0, 0, 0, 3, 1, 2, 3]);

        assert_eq!(net.recv(), None);
        // A frame that arrives in two pieces comes out whole.
        theirs.write_all(&[0, 0, 0, 4, 9, 8]).unwrap();
        assert_eq!(net.recv(), None);
        theirs.write_all(&[7, 6, 0, 0, 0, 1, 5]).unwrap();
        assert_eq!(net.recv(), Some(vec![9, 8, 7, 6]));
        assert_eq!(net.recv(), Some(vec![5]));
        assert_eq!(net.recv(), None);
    }

    #[test]
    fn a_closed_socket_stops_the_backend() {
        let (ours, theirs) = UnixStream::pair().unwrap();
        let mut net = StreamBackend::from_stream(ours).unwrap();
        drop(theirs);
        assert_eq!(net.recv(), None);
        net.send(&[1]);
        let log = net.take_log();
        assert_eq!(log[0], "the other end closed the socket");
    }
}
