//! Background handshake lifetime belongs to the TCP link.

use super::*;
use std::sync::mpsc::{self, TryRecvError};
use std::thread;

impl TcpLink {
    pub(super) fn finish_preaccept(&mut self, wait: Duration) -> Result<bool, LinkError> {
        let Some(pre) = self.preaccept.as_ref() else {
            return Ok(false);
        };
        let msg = if wait.is_zero() {
            match pre.rx.try_recv() {
                Ok(v) => Some(v),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => Some(Err(LinkError::NotConnected)),
            }
        } else {
            match pre.rx.recv_timeout(wait) {
                Ok(v) => Some(v),
                Err(mpsc::RecvTimeoutError::Timeout) => None,
                Err(mpsc::RecvTimeoutError::Disconnected) => Some(Err(LinkError::NotConnected)),
            }
        };

        match msg {
            Some(Ok((conn, caps, token))) if token == self.expected_session_token() => {
                self.conn = Some(conn);
                self.caps = caps;
                self.runtime_candidates.clear();
                self.preaccept = None;
                Ok(true)
            }
            Some(Ok(_)) => {
                self.preaccept = None;
                Ok(false)
            }
            Some(Err(e)) => {
                self.preaccept = None;
                Err(e)
            }
            None => Ok(false),
        }
    }

    pub(super) fn arm_preaccept(&mut self) -> Result<(), LinkError> {
        if self.preaccept.is_some() || self.conn.is_some() {
            return Ok(());
        }
        let listener = self
            .listener
            .as_ref()
            .ok_or(LinkError::NotConnected)?
            .try_clone()
            .map_err(io_to_link)?;
        self.start_preaccept(listener);
        Ok(())
    }

    pub(super) fn start_preaccept(&mut self, listener: TcpListener) {
        let timeout = self.timeout;
        let token_source = Arc::clone(&self.preaccept_token);
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let (tx, rx) = mpsc::channel();
        let handle = thread::spawn(move || {
            // Establish the worker's own polling mode after socket duplication.
            if let Err(error) = listener.set_nonblocking(true) {
                let _ = tx.send(Err(io_to_link(error)));
                return;
            }
            loop {
                if thread_stop.load(Ordering::Acquire) {
                    return;
                }
                match listener.accept() {
                    Ok((stream, _)) => {
                        if thread_stop.load(Ordering::Acquire) {
                            return;
                        }
                        let session_token = token_source
                            .read()
                            .unwrap_or_else(|e| e.into_inner())
                            .clone();
                        let result = handshake_stream_with_stop(
                            stream,
                            timeout,
                            Some(&session_token),
                            Some(&thread_stop),
                        )
                        .map(|(conn, caps)| (conn, caps, session_token));
                        let _ = tx.send(result);
                        break;
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(e) => {
                        let _ = tx.send(Err(io_to_link(e)));
                        break;
                    }
                }
            }
        });
        self.preaccept = Some(Preaccept { rx, handle, stop });
    }

    pub(super) fn cancel_preaccept(&mut self) {
        let Some(preaccept) = self.preaccept.take() else {
            return;
        };
        preaccept.stop.store(true, Ordering::Release);
        let _ = preaccept.handle.join();
    }
}
