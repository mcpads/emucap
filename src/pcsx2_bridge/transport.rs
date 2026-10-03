use super::*;
use std::time::Instant;

pub trait PineTransport {
    fn transact(&mut self, request: &[u8]) -> BridgeResult<Vec<u8>>;

    fn transact_with_timeout(
        &mut self,
        request: &[u8],
        _timeout: Duration,
    ) -> BridgeResult<Vec<u8>> {
        self.transact(request)
    }

    /// True once the current PINE stream can no longer preserve frame boundaries. A caller must
    /// replace the bridge/backend generation rather than retrying on the same stream.
    fn is_terminal(&self) -> bool {
        false
    }
}

enum PineStream {
    #[cfg(windows)]
    Tcp(TcpStream),
    #[cfg(unix)]
    Unix(UnixStream),
}

impl Read for PineStream {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        match self {
            #[cfg(windows)]
            Self::Tcp(stream) => stream.read(buffer),
            #[cfg(unix)]
            Self::Unix(stream) => stream.read(buffer),
        }
    }
}

impl Write for PineStream {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        match self {
            #[cfg(windows)]
            Self::Tcp(stream) => stream.write(buffer),
            #[cfg(unix)]
            Self::Unix(stream) => stream.write(buffer),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            #[cfg(windows)]
            Self::Tcp(stream) => stream.flush(),
            #[cfg(unix)]
            Self::Unix(stream) => stream.flush(),
        }
    }
}

pub struct PineSocket {
    stream: PineStream,
    terminal: bool,
    io_deadline: Option<Instant>,
}

impl PineSocket {
    pub fn connect(slot: u16, socket_path: Option<&Path>, timeout: Duration) -> BridgeResult<Self> {
        #[cfg(windows)]
        let stream = {
            let _ = socket_path;
            let stream = TcpStream::connect_timeout(
                &std::net::SocketAddr::from(([127, 0, 0, 1], slot)),
                timeout,
            )?;
            stream.set_read_timeout(Some(timeout))?;
            stream.set_write_timeout(Some(timeout))?;
            PineStream::Tcp(stream)
        };

        #[cfg(unix)]
        let stream = {
            let _ = slot;
            let path = socket_path.ok_or_else(|| {
                Pcsx2BridgeError::BadParams(
                    "PINE socket path is required on this platform".to_string(),
                )
            })?;
            let stream = UnixStream::connect(path)?;
            stream.set_read_timeout(Some(timeout))?;
            stream.set_write_timeout(Some(timeout))?;
            PineStream::Unix(stream)
        };

        Ok(Self {
            stream,
            terminal: false,
            io_deadline: None,
        })
    }
}

impl PineTransport for PineSocket {
    fn transact(&mut self, request: &[u8]) -> BridgeResult<Vec<u8>> {
        if self.terminal {
            return Err(Pcsx2BridgeError::Io(std::io::Error::new(
                std::io::ErrorKind::NotConnected,
                "PINE transport is terminal",
            )));
        }
        let outcome = (|| {
            let packet_len = request
                .len()
                .checked_add(4)
                .and_then(|length| u32::try_from(length).ok())
                .ok_or_else(|| Pcsx2BridgeError::BadParams("PINE request is too large".into()))?;
            self.write_packet_bytes(&packet_len.to_le_bytes())?;
            self.write_packet_bytes(request)?;
            self.stream.flush()?;

            let mut header = [0u8; 5];
            self.read_packet_bytes(&mut header)?;
            let reply_len =
                u32::from_le_bytes(header[..4].try_into().expect("four bytes")) as usize;
            if !(5..=PINE_MAX_REPLY).contains(&reply_len) {
                return Err(Pcsx2BridgeError::Protocol(format!(
                    "PINE reply length {reply_len} is outside 5..={PINE_MAX_REPLY}"
                )));
            }
            let mut payload = vec![0u8; reply_len - 5];
            self.read_packet_bytes(&mut payload)?;
            if header[4] != 0 {
                return Err(Pcsx2BridgeError::Emulator(
                    "PCSX2 rejected the PINE command".into(),
                ));
            }
            Ok(payload)
        })();
        if outcome.as_ref().is_err_and(|error| {
            matches!(
                error,
                Pcsx2BridgeError::Io(_) | Pcsx2BridgeError::Protocol(_)
            )
        }) {
            self.terminal = true;
        }
        outcome
    }

    fn transact_with_timeout(
        &mut self,
        request: &[u8],
        timeout: Duration,
    ) -> BridgeResult<Vec<u8>> {
        let deadline = Instant::now()
            .checked_add(timeout)
            .filter(|_| !timeout.is_zero())
            .ok_or_else(|| Pcsx2BridgeError::BadParams("invalid PINE exchange budget".into()))?;
        let previous = self.stream.timeouts()?;
        self.io_deadline = Some(deadline);
        let outcome = self.transact(request);
        let within_deadline = self.remaining_budget();
        self.io_deadline = None;
        let restored = self.stream.set_timeouts(previous.0, previous.1);
        if within_deadline.is_err() || restored.is_err() {
            self.terminal = true;
        }
        restored?;
        within_deadline?;
        outcome
    }

    fn is_terminal(&self) -> bool {
        self.terminal
    }
}

impl PineStream {
    fn timeouts(&self) -> std::io::Result<(Option<Duration>, Option<Duration>)> {
        match self {
            #[cfg(windows)]
            Self::Tcp(s) => Ok((s.read_timeout()?, s.write_timeout()?)),
            #[cfg(unix)]
            Self::Unix(s) => Ok((s.read_timeout()?, s.write_timeout()?)),
        }
    }
    fn set_timeouts(&self, read: Option<Duration>, write: Option<Duration>) -> std::io::Result<()> {
        // Attempt both restorations, even if one side fails.
        let (r, w) = match self {
            #[cfg(windows)]
            Self::Tcp(s) => (s.set_read_timeout(read), s.set_write_timeout(write)),
            #[cfg(unix)]
            Self::Unix(s) => (s.set_read_timeout(read), s.set_write_timeout(write)),
        };
        r?;
        w
    }
}
impl PineSocket {
    fn remaining_budget(&self) -> std::io::Result<Option<Duration>> {
        self.io_deadline
            .map(|end| {
                end.checked_duration_since(Instant::now())
                    .filter(|left| !left.is_zero())
                    .ok_or_else(|| {
                        std::io::Error::new(
                            std::io::ErrorKind::TimedOut,
                            "PINE exchange deadline expired",
                        )
                    })
            })
            .transpose()
    }
    fn prepare_io(&self) -> std::io::Result<()> {
        if let Some(left) = self.remaining_budget()? {
            self.stream.set_timeouts(Some(left), Some(left))?;
        }
        Ok(())
    }
    fn read_packet_bytes(&mut self, mut bytes: &mut [u8]) -> std::io::Result<()> {
        while !bytes.is_empty() {
            self.prepare_io()?;
            match self.stream.read(bytes) {
                Ok(0) => return Err(std::io::ErrorKind::UnexpectedEof.into()),
                Ok(count) => bytes = &mut bytes[count..],
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }
    fn write_packet_bytes(&mut self, mut bytes: &[u8]) -> std::io::Result<()> {
        while !bytes.is_empty() {
            self.prepare_io()?;
            match self.stream.write(bytes) {
                Ok(0) => return Err(std::io::ErrorKind::WriteZero.into()),
                Ok(count) => bytes = &bytes[count..],
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }
}

#[cfg(all(test, unix))]
#[path = "transport_tests.rs"]
mod tests;
