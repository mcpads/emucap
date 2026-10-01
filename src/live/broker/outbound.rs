//! One bounded writer per producer registration. Admission order is established by Registry.
use std::io;
use std::net::{Shutdown, TcpStream};
use std::sync::mpsc::{self, SyncSender};

// Frames are already limited to MAX_NDJSON_FRAME_BYTES. Include one in-flight frame
// when calculating the maximum retained payload: (QUEUE_FRAMES + 1) * frame limit.
const QUEUE_FRAMES: usize = 4;

pub(super) struct Outbound {
    socket: TcpStream,
    sender: SyncSender<String>,
}
impl Outbound {
    pub(super) fn new(mut writer: TcpStream) -> io::Result<Self> {
        let socket = writer.try_clone()?;
        let (sender, receiver) = mpsc::sync_channel::<String>(QUEUE_FRAMES);
        std::thread::Builder::new()
            .name("broker-producer-writer".into())
            .spawn(move || {
                while let Ok(line) = receiver.recv() {
                    if super::write_line(&mut writer, &line).is_err() {
                        break;
                    }
                }
                let _ = writer.shutdown(Shutdown::Both);
            })?;
        Ok(Self { socket, sender })
    }

    /// Called under routing admission lock; never blocks on a socket or full queue.
    pub(super) fn enqueue(&self, line: String) -> io::Result<()> {
        self.sender.try_send(line).map_err(|error| {
            self.close();
            io::Error::new(
                io::ErrorKind::BrokenPipe,
                format!("producer queue unavailable: {error}"),
            )
        })
    }

    pub(super) fn close(&self) {
        let _ = self.socket.shutdown(Shutdown::Both);
    }
}
impl Drop for Outbound {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
pub(super) fn held_queue(socket: TcpStream) -> (Outbound, mpsc::Receiver<String>) {
    let (sender, receiver) = mpsc::sync_channel(QUEUE_FRAMES);
    (Outbound { socket, sender }, receiver)
}
