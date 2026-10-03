use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use crate::launch::openmsx::PreparedSession;

use super::{tag_text, xml_escape, BridgeResult, OpenMsxBridgeError, OpenMsxControl};

const COMMAND_TIMEOUT: Duration = Duration::from_secs(5);
const CANCEL_SERVICE: Duration = Duration::from_millis(25);
const CANCEL_CLEANUP: Duration = Duration::from_millis(500);
const ADVANCE_TIMEOUT: Duration = Duration::from_secs(10);

// Match the public transport budget; XML batch payloads are hex, not inline images.
const MAX_CONTROL_LINE_BYTES: usize = crate::live::protocol::MAX_NDJSON_FRAME_BYTES;

fn read_control_line<R: BufRead>(reader: &mut R) -> io::Result<Option<String>> {
    let mut line = String::new();
    // One delimiter plus one excess byte lets us distinguish exact-limit success.
    let count = reader
        .take((MAX_CONTROL_LINE_BYTES + 2) as u64)
        .read_line(&mut line)?;
    if count == 0 {
        return Ok(None);
    }
    let payload = count - usize::from(line.ends_with('\n'));
    if payload > MAX_CONTROL_LINE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "openMSX control line exceeds 8 MiB",
        ));
    }
    if !line.ends_with('\n') {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "truncated openMSX control line",
        ));
    }
    Ok(Some(line))
}

enum XmlEvent {
    Ready,
    Reply { ok: bool, text: String },
    Pause(bool),
    Terminal(String),
}

pub struct XmlControl {
    child: Child,
    stdin: ChildStdin,
    events: Receiver<XmlEvent>,
    pause: Option<bool>,
    terminal: Arc<AtomicBool>,
    finished: bool,
    command_deadline: Option<Instant>,
    request_cancellation: crate::live::link::RequestCancellation,
    cancellation_deadline: Option<Instant>,
}

impl XmlControl {
    pub fn spawn(
        binary: &Path,
        session: &PreparedSession,
        runtime_home: &Path,
        display: bool,
    ) -> BridgeResult<Self> {
        let isolated_home = runtime_home.join("home");
        fs::create_dir_all(&isolated_home)?;
        let mut command = Command::new(binary);
        command
            .args(["-machine", &session.machine])
            .arg(session.media.kind.command_switch())
            .arg(&session.media.mounted_path)
            .args(["-control", "stdio"])
            .env("HOME", &isolated_home)
            .env("OPENMSX_HOME", &isolated_home)
            .env("OPENMSX_USER_DATA", &session.user_data)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        if !display {
            command.args(["-command", "set renderer none"]);
        }
        let mut child = command.spawn().map_err(|error| {
            OpenMsxBridgeError::Emulator(format!(
                "failed to start openMSX at {}: {error}",
                binary.display()
            ))
        })?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| OpenMsxBridgeError::Protocol("openMSX stdin was not piped".into()))?;
        if let Err(error) = configure_stdin(&stdin) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error.into());
        }
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| OpenMsxBridgeError::Protocol("openMSX stdout was not piped".into()))?;
        let terminal = Arc::new(AtomicBool::new(false));
        let reader_terminal = Arc::clone(&terminal);
        let (sender, events) = mpsc::channel();
        thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                match read_control_line(&mut reader) {
                    Ok(None) => {
                        reader_terminal.store(true, Ordering::Release);
                        let _ = sender
                            .send(XmlEvent::Terminal("openMSX control channel closed".into()));
                        break;
                    }
                    Ok(Some(line)) => {
                        let trimmed = line.trim_start();
                        if trimmed.contains("<openmsx-output>") {
                            let _ = sender.send(XmlEvent::Ready);
                        } else if trimmed.starts_with("<reply ") {
                            let ok = trimmed.contains("result=\"ok\"");
                            let text = tag_text(trimmed, "reply").unwrap_or_default();
                            let _ = sender.send(XmlEvent::Reply { ok, text });
                        } else if trimmed.starts_with("<update ")
                            && trimmed.contains("type=\"setting\"")
                            && trimmed.contains("name=\"pause\"")
                        {
                            if let Some(value) = tag_text(trimmed, "update") {
                                let _ = sender.send(XmlEvent::Pause(value == "true"));
                            }
                        } else if trimmed.starts_with("<log ") {
                            if let Some(message) = tag_text(trimmed, "log") {
                                eprintln!("[openMSX] {message}");
                            }
                        }
                    }
                    Err(error) => {
                        reader_terminal.store(true, Ordering::Release);
                        let _ = sender.send(XmlEvent::Terminal(format!(
                            "failed to read openMSX control channel: {error}"
                        )));
                        break;
                    }
                }
            }
        });

        let mut control = Self {
            child,
            stdin,
            events,
            pause: None,
            terminal,
            finished: false,
            command_deadline: None,
            request_cancellation: Default::default(),
            cancellation_deadline: None,
        };
        match control.events.recv_timeout(COMMAND_TIMEOUT).map_err(|_| {
            OpenMsxBridgeError::Emulator(
                "openMSX did not open its XML control stream in time".into(),
            )
        })? {
            XmlEvent::Ready => {}
            XmlEvent::Terminal(message) => return Err(OpenMsxBridgeError::Emulator(message)),
            _ => {
                return Err(OpenMsxBridgeError::Protocol(
                    "openMSX sent a control event before opening the XML stream".into(),
                ))
            }
        }
        control
            .stdin
            .write_all(b"<openmsx-control>\n")
            .map_err(OpenMsxBridgeError::Io)?;
        control.stdin.flush().map_err(OpenMsxBridgeError::Io)?;
        Ok(control)
    }

    fn shutdown(&mut self) {
        if self.finished {
            return;
        }
        let _ = self
            .stdin
            .write_all(b"<command>quit</command>\n</openmsx-control>\n");
        let _ = self.stdin.flush();
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            match self.child.try_wait() {
                Ok(Some(_)) => {
                    self.finished = true;
                    return;
                }
                Ok(None) => thread::sleep(Duration::from_millis(20)),
                Err(_) => break,
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.finished = true;
    }

    pub fn terminal_handle(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.terminal)
    }

    fn effective_deadline(&mut self, deadline: Instant) -> Instant {
        if self.request_cancellation.is_cancelled() && self.cancellation_deadline.is_none() {
            self.cancellation_deadline = Some(Instant::now() + CANCEL_CLEANUP);
        }
        self.cancellation_deadline
            .map_or(deadline, |cancel| deadline.min(cancel))
    }

    fn write_until(&mut self, mut bytes: &[u8], deadline: Instant) -> std::io::Result<()> {
        while !bytes.is_empty() {
            let deadline = self.effective_deadline(deadline);
            if Instant::now() >= deadline {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "openMSX command write deadline expired",
                ));
            }
            match self.stdin.write(bytes) {
                Ok(0) => return Err(std::io::ErrorKind::WriteZero.into()),
                Ok(count) => bytes = &bytes[count..],
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => (),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(
                        deadline
                            .saturating_duration_since(Instant::now())
                            .min(Duration::from_millis(1)),
                    );
                }
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    fn retire(&mut self) {
        self.terminal.store(true, Ordering::Release);
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.finished = true;
    }

    fn command_reply(&mut self, command: &str, deadline: Instant) -> BridgeResult<String> {
        loop {
            let deadline = self.effective_deadline(deadline);
            if Instant::now() >= deadline {
                self.retire();
                return Err(OpenMsxBridgeError::HostDeadline(
                    "openMSX command deadline expired".into(),
                ));
            }
            let wait = deadline
                .saturating_duration_since(Instant::now())
                .min(CANCEL_SERVICE);
            let event = match self.events.recv_timeout(wait) {
                Ok(event) => event,
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => {
                    self.retire();
                    return Err(OpenMsxBridgeError::Protocol(
                        "openMSX control event reader disconnected".into(),
                    ));
                }
            };
            let deadline = self.effective_deadline(deadline);
            if Instant::now() >= deadline {
                self.retire();
                return Err(OpenMsxBridgeError::HostDeadline(
                    "openMSX reply arrived after command deadline".into(),
                ));
            }
            match event {
                XmlEvent::Reply { ok: true, text } => return Ok(text),
                XmlEvent::Reply { ok: false, text } => {
                    return Err(OpenMsxBridgeError::Emulator(format!(
                        "openMSX rejected `{command}`: {text}"
                    )))
                }
                XmlEvent::Pause(value) => self.pause = Some(value),
                XmlEvent::Terminal(message) => {
                    self.terminal.store(true, Ordering::Release);
                    self.retire();
                    return Err(OpenMsxBridgeError::Emulator(message));
                }
                XmlEvent::Ready => {}
            }
        }
    }
}

impl OpenMsxControl for XmlControl {
    fn set_request_cancellation(&mut self, cancellation: crate::live::link::RequestCancellation) {
        self.request_cancellation = cancellation;
        self.cancellation_deadline = None;
    }
    fn set_command_deadline(&mut self, deadline: Option<Instant>) {
        self.command_deadline = deadline;
    }
    fn command(&mut self, command: &str) -> BridgeResult<String> {
        if self.is_terminal() {
            return Err(OpenMsxBridgeError::Emulator(
                "openMSX is no longer running".into(),
            ));
        }
        let deadline = self
            .command_deadline
            .map_or(Instant::now() + COMMAND_TIMEOUT, |deadline| {
                deadline.min(Instant::now() + COMMAND_TIMEOUT)
            });
        let deadline = self.effective_deadline(deadline);
        if Instant::now() >= deadline {
            self.retire();
            return Err(OpenMsxBridgeError::HostDeadline(
                "openMSX cleanup deadline expired before command".into(),
            ));
        }
        let wire = format!("<command>{}</command>\n", xml_escape(command));
        if let Err(error) = self.write_until(wire.as_bytes(), deadline) {
            self.retire();
            return Err(error.into());
        }
        self.command_reply(command, deadline)
    }

    fn advance_frames(&mut self, count: u64) -> BridgeResult<()> {
        self.advance_frames_cancellable(count, &Default::default())
    }

    fn advance_frames_cancellable(
        &mut self,
        count: u64,
        cancellation: &crate::live::link::RequestCancellation,
    ) -> BridgeResult<()> {
        if cancellation.is_cancelled() {
            return Err(OpenMsxBridgeError::Cancelled);
        }
        while let Ok(event) = self.events.try_recv() {
            match event {
                XmlEvent::Pause(value) => self.pause = Some(value),
                XmlEvent::Terminal(_) => self.terminal.store(true, Ordering::Release),
                _ => {}
            }
        }
        self.pause = None;
        // `advance_frame` preserves the current dot and can cross one extra
        // VDP frame counter boundary when restored exactly at a boundary.
        // `next_frame` targets the start of the Nth following frame instead.
        self.command(&format!("::emucap::next_frame {count}"))?;
        let deadline = Instant::now() + ADVANCE_TIMEOUT;
        loop {
            if self.pause == Some(true) {
                return Ok(());
            }
            if cancellation.is_cancelled() {
                self.effective_deadline(deadline);
                return Err(OpenMsxBridgeError::Cancelled);
            }
            if Instant::now() >= deadline {
                return Err(OpenMsxBridgeError::HostDeadline(format!(
                    "frame advance exceeded its {}-second host budget",
                    ADVANCE_TIMEOUT.as_secs()
                )));
            }
            let wait = deadline
                .saturating_duration_since(Instant::now())
                .min(Duration::from_millis(25));
            let event = match self.events.recv_timeout(wait) {
                Ok(event) => event,
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(OpenMsxBridgeError::Protocol(
                        "openMSX control event reader disconnected".into(),
                    ))
                }
            };
            match event {
                XmlEvent::Pause(value) => self.pause = Some(value),
                XmlEvent::Terminal(message) => {
                    self.terminal.store(true, Ordering::Release);
                    return Err(OpenMsxBridgeError::Emulator(message));
                }
                XmlEvent::Reply { .. } => {
                    return Err(OpenMsxBridgeError::Protocol(
                        "unexpected openMSX reply after advance_frame".into(),
                    ))
                }
                XmlEvent::Ready => {}
            }
        }
    }

    fn is_terminal(&self) -> bool {
        self.terminal.load(Ordering::Acquire)
    }

    fn child_pid(&self) -> u32 {
        self.child.id()
    }
}

#[cfg(all(test, unix))]
#[path = "xml_tests.rs"]
mod tests;

impl Drop for XmlControl {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(unix)]
fn configure_stdin(stdin: &ChildStdin) -> std::io::Result<()> {
    use std::os::fd::AsRawFd;
    let fd = stdin.as_raw_fd();
    // We own this pipe descriptor for the child's lifetime.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(not(unix))]
fn configure_stdin(_: &ChildStdin) -> std::io::Result<()> {
    // Non-Unix pipe writes still require a cancellable host transport before capability admission.
    Ok(())
}
