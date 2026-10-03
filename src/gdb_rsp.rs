//! Adapter-neutral GDB Remote Serial Protocol transport and process metadata.

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::time::{Duration, Instant};

#[cfg(test)]
#[path = "gdb_rsp_tests.rs"]
mod tests;

#[derive(Debug, thiserror::Error)]
pub enum GdbError {
    #[error("{0}")]
    Emulator(String),
    #[error("GDB transport is poisoned after a prior stream error")]
    Poisoned,
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub type GdbResult<T> = Result<T, GdbError>;

pub trait GdbTransport {
    fn send(&mut self, payload: &str) -> GdbResult<String>;
    /// Bounds the entire request/response, including fragmented packets and ACKs.
    fn send_with_timeout(&mut self, _payload: &str, _budget: Duration) -> GdbResult<String> {
        Err(GdbError::Emulator("bounded send unsupported".into()))
    }
    fn recv_reply_with_timeout(&mut self, _budget: Duration) -> GdbResult<String> {
        Err(GdbError::Emulator("bounded receive unsupported".into()))
    }
    fn send_no_reply(&mut self, payload: &str) -> GdbResult<()>;
    fn send_no_reply_with_timeout(&mut self, _payload: &str, _budget: Duration) -> GdbResult<()> {
        Err(GdbError::Emulator("bounded dispatch unsupported".into()))
    }
    fn interrupt(&mut self) -> GdbResult<String>;
    fn request_interrupt_with_timeout(&mut self, _budget: Duration) -> GdbResult<()> {
        Err(GdbError::Emulator(
            "bounded interrupt request unsupported".into(),
        ))
    }
    fn interrupt_with_timeout(&mut self, _budget: Duration) -> GdbResult<String> {
        Err(GdbError::Emulator("bounded interrupt unsupported".into()))
    }
    /// True once this transport can no longer carry another request. Front-session reconnect must
    /// not reuse a terminal backend connection.
    fn is_terminal(&self) -> bool {
        false
    }
    /// Reads the next RSP packet without first writing a command.
    ///
    /// Adapter demultiplexers use this after discarding an asynchronous stop that arrived before
    /// the actual command response.
    fn recv_reply(&mut self) -> GdbResult<String> {
        Err(GdbError::Emulator("recv_reply unsupported".into()))
    }
    fn get_timeout(&self) -> GdbResult<Duration> {
        Ok(Duration::from_secs(5))
    }
    fn set_timeout(&mut self, _timeout: Duration) -> GdbResult<()> {
        Ok(())
    }
    fn recv_nonblocking_with_timeout(&mut self, _budget: Duration) -> GdbResult<Option<String>> {
        Err(GdbError::Emulator("bounded polling unsupported".into()))
    }
    fn recv_nonblocking(&mut self) -> GdbResult<Option<String>> {
        Ok(None)
    }
}

/// Process identity and content metadata shared by GDB-backed adapters.
#[derive(Debug, Clone, Default)]
pub struct GdbBridgeEnv {
    pub name: Option<String>,
    pub session_token: Option<String>,
    pub launch_id: Option<String>,
    pub content: Option<PathBuf>,
    pub build: Option<String>,
}

impl GdbBridgeEnv {
    pub fn from_process_env() -> Self {
        Self {
            name: std::env::var("EMUCAP_NAME").ok(),
            session_token: std::env::var("EMUCAP_SESSION_TOKEN").ok(),
            launch_id: std::env::var("EMUCAP_LAUNCH_ID").ok(),
            content: std::env::var_os("EMUCAP_CONTENT").map(PathBuf::from),
            build: Some(crate::build_identity::BUILD_HASH.to_string()),
        }
    }
}

// Bound encoded RSP payloads before decoding; framing adds four bytes.
const MAX_RSP_PAYLOAD_BYTES: usize = 8 * 1024 * 1024;

pub struct GdbRspClient {
    stream: TcpStream,
    buf: VecDeque<u8>,
    poisoned: bool,
    io_deadline: Option<Instant>,
}

impl GdbRspClient {
    pub fn connect(
        host: &str,
        port: u16,
        timeout: Duration,
        connect_wait: Duration,
    ) -> std::io::Result<Self> {
        let deadline = Instant::now() + connect_wait;
        loop {
            match TcpStream::connect((host, port)) {
                Ok(stream) => {
                    stream.set_read_timeout(Some(timeout))?;
                    stream.set_write_timeout(Some(timeout))?;
                    return Ok(Self {
                        stream,
                        buf: VecDeque::new(),
                        poisoned: false,
                        io_deadline: None,
                    });
                }
                Err(err) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(300));
                    if err.kind() == std::io::ErrorKind::InvalidInput {
                        return Err(err);
                    }
                }
                Err(err) => return Err(err),
            }
        }
    }

    fn checksum(payload: &[u8]) -> u8 {
        payload.iter().fold(0u8, |sum, b| sum.wrapping_add(*b))
    }

    fn frame(payload: &str) -> Vec<u8> {
        let data = payload.as_bytes();
        let mut out = Vec::with_capacity(data.len() + 4);
        out.push(b'$');
        out.extend_from_slice(data);
        out.push(b'#');
        out.extend_from_slice(format!("{:02x}", Self::checksum(data)).as_bytes());
        out
    }

    fn remaining_budget(&self) -> std::io::Result<Option<Duration>> {
        self.io_deadline
            .map(|deadline| {
                deadline
                    .checked_duration_since(Instant::now())
                    .filter(|remaining| !remaining.is_zero())
                    .ok_or_else(|| {
                        std::io::Error::new(
                            std::io::ErrorKind::TimedOut,
                            "GDB exchange deadline expired",
                        )
                    })
            })
            .transpose()
    }

    fn write_bytes(&mut self, mut bytes: &[u8]) -> std::io::Result<()> {
        while !bytes.is_empty() {
            if let Some(remaining) = self.remaining_budget()? {
                self.stream
                    .set_write_timeout(Some(remaining))
                    .map_err(|error| {
                        std::io::Error::new(
                            error.kind(),
                            format!("GDB write timeout {remaining:?}: {error}"),
                        )
                    })?;
            }
            match self.stream.write(bytes) {
                Ok(0) => return Err(std::io::ErrorKind::WriteZero.into()),
                Ok(n) => bytes = &bytes[n..],
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    fn bounded<T>(
        &mut self,
        budget: Duration,
        operation: impl FnOnce(&mut Self) -> GdbResult<T>,
    ) -> GdbResult<T> {
        self.ensure_usable()?;
        let deadline = Instant::now()
            .checked_add(budget)
            .filter(|_| !budget.is_zero())
            .ok_or_else(|| GdbError::Emulator("invalid GDB exchange budget".into()))?;
        let read_timeout = self.stream.read_timeout()?;
        let write_timeout = self.stream.write_timeout()?;
        self.io_deadline = Some(deadline);
        let result = operation(self);
        let elapsed = self.remaining_budget();
        self.io_deadline = None;
        let restore_read = self.stream.set_read_timeout(read_timeout).map_err(|error| {
            std::io::Error::new(
                error.kind(),
                format!("GDB restore read timeout {read_timeout:?}: {error}"),
            )
        });
        let restore_write = self
            .stream
            .set_write_timeout(write_timeout)
            .map_err(|error| {
                std::io::Error::new(
                    error.kind(),
                    format!("GDB restore write timeout {write_timeout:?}: {error}"),
                )
            });
        if result.is_err() || elapsed.is_err() || restore_read.is_err() || restore_write.is_err() {
            self.poisoned = true;
        }
        if let Err(primary) = result {
            if restore_read.is_ok() && restore_write.is_ok() {
                return Err(primary);
            }
            let cleanup =
                format!("timeout restoration: read={restore_read:?}, write={restore_write:?}");
            return Err(match primary {
                GdbError::Io(error) => GdbError::Io(std::io::Error::new(
                    error.kind(),
                    format!("{error}; {cleanup}"),
                )),
                GdbError::Emulator(message) => GdbError::Emulator(format!("{message}; {cleanup}")),
                GdbError::Poisoned => GdbError::Poisoned,
            });
        }
        restore_read?;
        restore_write?;
        elapsed?;
        result
    }

    fn read_byte(&mut self) -> std::io::Result<u8> {
        let remaining = self.remaining_budget()?;
        if let Some(b) = self.buf.pop_front() {
            return Ok(b);
        }
        if let Some(remaining) = remaining {
            self.stream
                .set_read_timeout(Some(remaining))
                .map_err(|error| {
                    std::io::Error::new(
                        error.kind(),
                        format!("GDB read timeout {remaining:?}: {error}"),
                    )
                })?;
        }
        let mut chunk = [0u8; 4096];
        let n = self.stream.read(&mut chunk)?;
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "GDB connection closed",
            ));
        }
        self.buf.extend(&chunk[..n]);
        Ok(self.buf.pop_front().expect("buffer was just filled"))
    }

    fn write_packet(&mut self, payload: &str) -> std::io::Result<()> {
        if payload.len() > MAX_RSP_PAYLOAD_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "GDB payload exceeds 8 MiB",
            ));
        }
        let frame = Self::frame(payload);
        self.write_bytes(&frame)?;
        for _ in 0..8 {
            match self.read_byte()? {
                b'+' => return Ok(()),
                b'-' => self.write_bytes(&frame)?,
                b'$' => {
                    self.buf.push_front(b'$');
                    return Ok(());
                }
                _ => {}
            }
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "GDB packet was not acknowledged",
        ))
    }

    fn read_packet(&mut self) -> std::io::Result<String> {
        while self.read_byte()? != b'$' {}

        let mut raw = Vec::new();
        loop {
            let b = self.read_byte()?;
            if b == b'#' {
                break;
            }
            if raw.len() == MAX_RSP_PAYLOAD_BYTES {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "GDB payload exceeds 8 MiB",
                ));
            }
            raw.push(b);
        }
        let mut checksum = [0u8; 2];
        checksum[0] = self.read_byte()?;
        checksum[1] = self.read_byte()?;
        let expected = std::str::from_utf8(&checksum)
            .ok()
            .and_then(|s| u8::from_str_radix(s, 16).ok());
        if expected != Some(Self::checksum(&raw)) {
            let _ = self.write_bytes(b"-");
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "GDB packet checksum mismatch",
            ));
        }
        self.write_bytes(b"+")?;

        let mut out = Vec::with_capacity(raw.len());
        let mut i = 0;
        while i < raw.len() {
            if raw[i] == b'}' && i + 1 < raw.len() {
                out.push(raw[i + 1] ^ 0x20);
                i += 2;
            } else {
                out.push(raw[i]);
                i += 1;
            }
        }
        Ok(String::from_utf8_lossy(&out).into_owned())
    }

    fn ensure_usable(&self) -> GdbResult<()> {
        if self.poisoned {
            Err(GdbError::Poisoned)
        } else {
            Ok(())
        }
    }

    fn finish_io<T>(&mut self, result: std::io::Result<T>) -> GdbResult<T> {
        if result.is_err() {
            self.poisoned = true;
        }
        result.map_err(GdbError::from)
    }

    fn has_complete_buffered_packet(&self) -> bool {
        let Some(start) = self.buf.iter().position(|byte| *byte == b'$') else {
            return false;
        };
        self.buf
            .iter()
            .enumerate()
            .skip(start + 1)
            .find(|(_, byte)| **byte == b'#')
            .is_some_and(|(end, _)| self.buf.len() >= end + 3)
    }

    fn check_pending_packet_size(&self) -> std::io::Result<()> {
        if self.buf.len() > MAX_RSP_PAYLOAD_BYTES + 4 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "GDB incomplete packet exceeds 8 MiB payload budget",
            ));
        }
        Ok(())
    }

    fn recv_nonblocking_packet(&mut self) -> std::io::Result<Option<String>> {
        // A complete buffered reply remains valid even if the peer has since closed.
        if self.has_complete_buffered_packet() {
            return self.read_packet().map(Some);
        }
        self.check_pending_packet_size()?;
        let previous = self.stream.read_timeout()?;
        self.stream.set_nonblocking(true)?;
        let read = {
            let mut chunk = [0u8; 4096];
            match self.stream.read(&mut chunk) {
                Ok(0) => Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "GDB connection closed",
                )),
                Ok(n) => {
                    self.buf.extend(&chunk[..n]);
                    Ok(())
                }
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => Ok(()),
                Err(err) => Err(err),
            }
        };
        self.stream.set_nonblocking(false)?;
        self.stream.set_read_timeout(previous).map_err(|error| {
            std::io::Error::new(
                error.kind(),
                format!("GDB poll restore timeout {previous:?}: {error}"),
            )
        })?;
        read?;
        // Never enter the blocking decoder on a partial header, body or checksum.
        // Retain the prefix so a later poll can finish this same packet.
        if !self.has_complete_buffered_packet() {
            self.check_pending_packet_size()?;
            return Ok(None);
        }
        self.read_packet().map(Some)
    }
}

impl GdbTransport for GdbRspClient {
    fn is_terminal(&self) -> bool {
        self.poisoned
    }

    fn send(&mut self, payload: &str) -> GdbResult<String> {
        self.ensure_usable()?;
        let result = (|| {
            self.write_packet(payload)?;
            self.read_packet()
        })();
        self.finish_io(result)
    }

    fn send_with_timeout(&mut self, payload: &str, budget: Duration) -> GdbResult<String> {
        self.bounded(budget, |client| client.send(payload))
    }

    fn recv_reply_with_timeout(&mut self, budget: Duration) -> GdbResult<String> {
        self.bounded(budget, |client| client.recv_reply())
    }

    fn send_no_reply_with_timeout(&mut self, payload: &str, budget: Duration) -> GdbResult<()> {
        self.bounded(budget, |client| client.send_no_reply(payload))
    }

    fn send_no_reply(&mut self, payload: &str) -> GdbResult<()> {
        self.ensure_usable()?;
        let result = self.write_packet(payload);
        self.finish_io(result)
    }

    fn recv_reply(&mut self) -> GdbResult<String> {
        self.ensure_usable()?;
        let result = self.read_packet();
        self.finish_io(result)
    }

    fn request_interrupt_with_timeout(&mut self, budget: Duration) -> GdbResult<()> {
        self.bounded(budget, |client| Ok(client.write_bytes(&[3])?))
    }

    fn interrupt(&mut self) -> GdbResult<String> {
        self.ensure_usable()?;
        let result = self.write_bytes(&[0x03]);
        self.finish_io(result)?;
        // A GDB remote interrupt is not a request packet: the stub answers the raw 0x03 byte
        // asynchronously with a stop packet. Read and acknowledge that packet before writing
        // anything else. Sending `?` first makes bounded stubs interpret its leading `$` as the
        // missing ACK for the stop reply, after which they may close the connection.
        self.recv_reply()
    }

    fn interrupt_with_timeout(&mut self, budget: Duration) -> GdbResult<String> {
        self.bounded(budget, |client| client.interrupt())
    }

    fn get_timeout(&self) -> GdbResult<Duration> {
        Ok(self
            .stream
            .read_timeout()?
            .unwrap_or(Duration::from_secs(5)))
    }

    fn set_timeout(&mut self, timeout: Duration) -> GdbResult<()> {
        self.stream.set_read_timeout(Some(timeout))?;
        self.stream.set_write_timeout(Some(timeout))?;
        Ok(())
    }

    fn recv_nonblocking_with_timeout(&mut self, budget: Duration) -> GdbResult<Option<String>> {
        self.bounded(budget, |client| client.recv_nonblocking())
    }

    fn recv_nonblocking(&mut self) -> GdbResult<Option<String>> {
        self.ensure_usable()?;
        let result = self.recv_nonblocking_packet();
        self.finish_io(result)
    }
}
