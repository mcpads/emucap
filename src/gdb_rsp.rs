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
    fn send_no_reply(&mut self, payload: &str) -> GdbResult<()>;
    fn interrupt(&mut self) -> GdbResult<String>;
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
            build: std::env::var("EMUCAP_BUILD_HASH").ok(),
        }
    }
}

pub struct GdbRspClient {
    stream: TcpStream,
    buf: VecDeque<u8>,
    poisoned: bool,
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

    fn read_byte(&mut self) -> std::io::Result<u8> {
        if let Some(b) = self.buf.pop_front() {
            return Ok(b);
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
        let frame = Self::frame(payload);
        self.stream.write_all(&frame)?;
        for _ in 0..8 {
            match self.read_byte()? {
                b'+' => return Ok(()),
                b'-' => self.stream.write_all(&frame)?,
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
            raw.push(b);
        }
        let mut checksum = [0u8; 2];
        checksum[0] = self.read_byte()?;
        checksum[1] = self.read_byte()?;
        let expected = std::str::from_utf8(&checksum)
            .ok()
            .and_then(|s| u8::from_str_radix(s, 16).ok());
        if expected != Some(Self::checksum(&raw)) {
            let _ = self.stream.write_all(b"-");
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "GDB packet checksum mismatch",
            ));
        }
        self.stream.write_all(b"+")?;

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

    fn recv_nonblocking_packet(&mut self) -> std::io::Result<Option<String>> {
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
        self.stream.set_read_timeout(previous)?;
        read?;
        if !self.buf.iter().any(|byte| *byte == b'$') {
            return Ok(None);
        }
        Ok(Some(self.read_packet()?))
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

    fn interrupt(&mut self) -> GdbResult<String> {
        self.ensure_usable()?;
        let result = self.stream.write_all(&[0x03]);
        self.finish_io(result)?;
        // A GDB remote interrupt is not a request packet: the stub answers the raw 0x03 byte
        // asynchronously with a stop packet. Read and acknowledge that packet before writing
        // anything else. Sending `?` first makes bounded stubs interpret its leading `$` as the
        // missing ACK for the stop reply, after which they may close the connection.
        self.recv_reply()
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

    fn recv_nonblocking(&mut self) -> GdbResult<Option<String>> {
        self.ensure_usable()?;
        let result = self.recv_nonblocking_packet();
        self.finish_io(result)
    }
}
