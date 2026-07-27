use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use super::{tag_text, xml_escape, BridgeResult, OpenMsxBridgeError, OpenMsxControl};

const COMMAND_TIMEOUT: Duration = Duration::from_secs(5);
const ADVANCE_TIMEOUT: Duration = Duration::from_secs(10);

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
}

impl XmlControl {
    pub fn spawn(
        binary: &Path,
        content: &Path,
        runtime_home: &Path,
        display: bool,
    ) -> BridgeResult<Self> {
        let isolated_home = runtime_home.join("home");
        fs::create_dir_all(&isolated_home)?;
        let mut command = Command::new(binary);
        command
            .args(["-machine", "C-BIOS_MSX2+", "-cart"])
            .arg(content)
            .args(["-control", "stdio"])
            .env("HOME", &isolated_home)
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
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| OpenMsxBridgeError::Protocol("openMSX stdout was not piped".into()))?;
        let terminal = Arc::new(AtomicBool::new(false));
        let reader_terminal = Arc::clone(&terminal);
        let (sender, events) = mpsc::channel();
        thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line) {
                    Ok(0) => {
                        reader_terminal.store(true, Ordering::Release);
                        let _ = sender
                            .send(XmlEvent::Terminal("openMSX control channel closed".into()));
                        break;
                    }
                    Ok(_) => {
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

    fn recv_until(&self, deadline: Instant) -> BridgeResult<XmlEvent> {
        let timeout = deadline.saturating_duration_since(Instant::now());
        match self.events.recv_timeout(timeout) {
            Ok(event) => Ok(event),
            Err(RecvTimeoutError::Timeout) => Err(OpenMsxBridgeError::Emulator(
                "openMSX control operation timed out".into(),
            )),
            Err(RecvTimeoutError::Disconnected) => Err(OpenMsxBridgeError::Protocol(
                "openMSX control event reader disconnected".into(),
            )),
        }
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
}

impl OpenMsxControl for XmlControl {
    fn command(&mut self, command: &str) -> BridgeResult<String> {
        if self.is_terminal() {
            return Err(OpenMsxBridgeError::Emulator(
                "openMSX is no longer running".into(),
            ));
        }
        let wire = format!("<command>{}</command>\n", xml_escape(command));
        self.stdin.write_all(wire.as_bytes())?;
        self.stdin.flush()?;
        let deadline = Instant::now() + COMMAND_TIMEOUT;
        loop {
            match self.recv_until(deadline)? {
                XmlEvent::Reply { ok: true, text } => return Ok(text),
                XmlEvent::Reply { ok: false, text } => {
                    return Err(OpenMsxBridgeError::Emulator(format!(
                        "openMSX rejected `{command}`: {text}"
                    )))
                }
                XmlEvent::Pause(value) => self.pause = Some(value),
                XmlEvent::Terminal(message) => {
                    self.terminal.store(true, Ordering::Release);
                    return Err(OpenMsxBridgeError::Emulator(message));
                }
                XmlEvent::Ready => {}
            }
        }
    }

    fn advance_frames(&mut self, count: u64) -> BridgeResult<()> {
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
        self.command(&format!("next_frame {count}"))?;
        let deadline = Instant::now() + ADVANCE_TIMEOUT;
        loop {
            if self.pause == Some(true) {
                return Ok(());
            }
            match self.recv_until(deadline)? {
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

impl Drop for XmlControl {
    fn drop(&mut self) {
        self.shutdown();
    }
}
