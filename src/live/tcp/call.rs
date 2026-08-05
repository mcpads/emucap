use super::*;

struct SocketDeadlineGuard {
    socket: TcpStream,
    restore: Duration,
}

impl SocketDeadlineGuard {
    fn new(socket: &TcpStream, restore: Duration) -> Result<Self, LinkError> {
        Ok(Self {
            socket: socket.try_clone().map_err(io_to_link)?,
            restore,
        })
    }

    fn clamp(&self, deadline: std::time::Instant) -> Result<(), LinkError> {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        let timeout = self.restore.min(remaining).max(Duration::from_millis(1));
        self.socket
            .set_read_timeout(Some(timeout))
            .and_then(|_| self.socket.set_write_timeout(Some(timeout)))
            .map_err(io_to_link)
    }
}

impl Drop for SocketDeadlineGuard {
    fn drop(&mut self) {
        let _ = self.socket.set_read_timeout(Some(self.restore));
        let _ = self.socket.set_write_timeout(Some(self.restore));
    }
}

impl TcpLink {
    /// Send a request over the admitted connection and wait for its terminal response.
    /// `status:"working"` keepalive frames only report progress and are skipped here.
    pub(super) fn raw_call(&mut self, method: &str, params: Value) -> Result<Value, LinkError> {
        self.raw_call_inner(method, params, None, None)
    }

    fn send_abort(&mut self, abort: &AbortRequest) -> Result<(), LinkError> {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        let request = Request::new(id, &abort.method, abort.params.clone());
        let conn = self.conn.as_mut().ok_or(LinkError::NotConnected)?;
        if let Err(error) = conn.writer.write_all(to_line(&request).as_bytes()) {
            self.drop_conn();
            return if is_timeout(&error) {
                Err(LinkError::Timeout)
            } else {
                Err(io_to_link(error))
            };
        }
        Ok(())
    }

    pub(super) fn raw_call_inner(
        &mut self,
        method: &str,
        params: Value,
        mut observer: Option<&mut ProgressObserver<'_>>,
        control: Option<&ProgressCallControl>,
    ) -> Result<Value, LinkError> {
        // Start the admitted host deadline before the request write. Socket reads and writes are
        // clamped to the remaining time so a short recording deadline cannot be exceeded by the
        // link's ordinary per-I/O timeout.
        let call_deadline = control
            .and_then(|control| control.max_host_ms)
            .map(Duration::from_millis)
            .unwrap_or(self.deferred_deadline)
            .min(self.deferred_deadline);
        let deadline = std::time::Instant::now() + call_deadline;
        let deadline_guard = {
            let conn = self.conn.as_ref().ok_or(LinkError::NotConnected)?;
            SocketDeadlineGuard::new(conn.reader.get_ref(), self.timeout)?
        };
        deadline_guard.clamp(deadline)?;

        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        let req = Request::new(id, method, params);

        {
            let conn = self.conn.as_mut().ok_or(LinkError::NotConnected)?;
            if let Err(e) = conn.writer.write_all(to_line(&req).as_bytes()) {
                // Drop the connection after any write failure, including timeout or broken pipe.
                // A timed-out write may have sent only part of a large request line, so appending
                // another request would corrupt the peer's NDJSON framing. Read timeouts can retain
                // partial input in `conn.pending`; partial output cannot be recovered. Dropping the
                // stream lets the next call admit a fresh client instead.
                self.drop_conn();
                if is_timeout(&e) {
                    return Err(LinkError::Timeout);
                }
                return Err(io_to_link(e));
            }
        }

        let mut abort_sent = false;
        const MAX_ID_MISMATCH: u32 = 256;
        let mut mismatch = 0u32;
        loop {
            if std::time::Instant::now() >= deadline {
                self.consecutive_timeouts = 0;
                self.drop_conn();
                return if control.is_some_and(|control| control.cancellation.is_cancelled()) {
                    Err(LinkError::Cancelled)
                } else {
                    Err(LinkError::Timeout)
                };
            }
            let read_is_deadline_clamped =
                deadline.saturating_duration_since(std::time::Instant::now()) <= self.timeout;
            deadline_guard.clamp(deadline)?;
            let read_result = {
                let conn = self.conn.as_mut().ok_or(LinkError::NotConnected)?;
                read_ndjson_frame(&mut conn.reader, &mut conn.pending)
            };
            match read_result {
                Ok(None) => {
                    self.drop_conn();
                    return Err(LinkError::NotConnected);
                }
                Ok(Some(line)) => {
                    self.consecutive_timeouts = 0;
                    let resp = match parse_response(line.trim()) {
                        Ok(response) => response,
                        Err(error) => {
                            self.drop_conn();
                            return Err(LinkError::Protocol(error.to_string()));
                        }
                    };
                    if resp.id != id {
                        mismatch += 1;
                        if mismatch > MAX_ID_MISMATCH {
                            self.drop_conn();
                            return Err(LinkError::Protocol(
                                "too many frames with a mismatched id; stream desynchronized"
                                    .into(),
                            ));
                        }
                        continue;
                    }
                    if !resp.ok {
                        return if let Some(err) = resp.error {
                            Err(LinkError::Emulator {
                                kind: err.kind,
                                message: err.message,
                            })
                        } else {
                            Err(LinkError::Protocol(
                                "response returned ok=false without an error".into(),
                            ))
                        };
                    }
                    let result = resp.result.unwrap_or(Value::Null);
                    if super::super::protocol::result_status(&result)
                        == super::super::protocol::STATUS_WORKING
                    {
                        if let Some(observer) = observer.as_deref_mut() {
                            let progress = match WorkingProgress::parse(result) {
                                Ok(progress) => progress,
                                Err(error) => {
                                    if let Some(abort) =
                                        control.and_then(|control| control.abort.as_ref())
                                    {
                                        let _ = self.send_abort(abort);
                                    }
                                    self.drop_conn();
                                    return Err(error);
                                }
                            };
                            if let Err(error) = observer(&progress) {
                                if let Some(abort) =
                                    control.and_then(|control| control.abort.as_ref())
                                {
                                    let _ = self.send_abort(abort);
                                }
                                self.drop_conn();
                                return Err(error);
                            }
                        }
                        if !abort_sent
                            && control.is_some_and(|control| control.cancellation.is_cancelled())
                        {
                            let Some(abort) = control.and_then(|control| control.abort.as_ref())
                            else {
                                self.drop_conn();
                                return Err(LinkError::Cancelled);
                            };
                            self.send_abort(abort)?;
                            abort_sent = true;
                        }
                        continue;
                    }
                    return Ok(result);
                }
                Err(ref e) if is_timeout(e) => {
                    if !abort_sent
                        && control.is_some_and(|control| control.cancellation.is_cancelled())
                    {
                        let Some(abort) = control.and_then(|control| control.abort.as_ref()) else {
                            self.drop_conn();
                            return Err(LinkError::Cancelled);
                        };
                        self.send_abort(abort)?;
                        abort_sent = true;
                        continue;
                    }
                    if read_is_deadline_clamped {
                        if std::time::Instant::now() < deadline {
                            continue;
                        }
                        self.consecutive_timeouts = 0;
                        self.drop_conn();
                        return Err(LinkError::Timeout);
                    }
                    self.consecutive_timeouts += 1;
                    if self.consecutive_timeouts >= MAX_CONSECUTIVE_TIMEOUTS {
                        self.consecutive_timeouts = 0;
                        self.drop_conn();
                    }
                    return Err(LinkError::Timeout);
                }
                Err(e) => {
                    self.drop_conn();
                    return Err(io_to_link(e));
                }
            }
        }
    }
}
