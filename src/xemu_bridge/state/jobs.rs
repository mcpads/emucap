use super::*;

use std::time::Instant;

const STATE_JOB_TIMEOUT: Duration = Duration::from_secs(10);
const STATE_JOB_POLL_INTERVAL: Duration = Duration::from_millis(10);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct BlockLayout {
    pub(super) hdd_node: String,
}

impl<Q: QmpTransport, G: GdbTransport> XemuBridge<Q, G> {
    pub(super) fn block_layout(&mut self) -> XemuResult<BlockLayout> {
        let value = self.qmp.execute("query-block", None)?;
        let blocks = value.as_array().ok_or_else(|| {
            XemuBridgeError::Emulator("xemu query-block did not return a list".into())
        })?;
        let mut writable = Vec::new();
        let mut removable = Vec::new();
        for block in blocks {
            let Some(inserted) = block.get("inserted") else {
                continue;
            };
            let read_only = inserted.get("ro").and_then(Value::as_bool).ok_or_else(|| {
                XemuBridgeError::Emulator("xemu query-block entry omitted ro".into())
            })?;
            let file = inserted
                .get("file")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    XemuBridgeError::Emulator("xemu query-block entry omitted file".into())
                })?;
            if !read_only {
                writable.push(inserted);
            }
            if block.get("removable").and_then(Value::as_bool) == Some(true) {
                removable.push(file);
            }
        }
        if writable.len() != 1 {
            return Err(XemuBridgeError::BadState(format!(
                "Xbox state requires exactly one writable block node, observed {}",
                writable.len()
            )));
        }
        let hdd = writable[0];
        if hdd.get("drv").and_then(Value::as_str) != Some("qcow2")
            || hdd.get("file").and_then(Value::as_str)
                != Some(self.state_environment.hdd.to_string_lossy().as_ref())
        {
            return Err(XemuBridgeError::BadState(
                "xemu writable block node is not the managed generation HDD".into(),
            ));
        }
        let expected_media = self
            .current_disc
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned());
        match expected_media.as_deref() {
            Some(path) if removable.as_slice() == [path] => {}
            None if removable.is_empty() => {}
            _ => {
                return Err(XemuBridgeError::BadState(
                    "xemu removable-media topology does not match the bridge's current disc".into(),
                ))
            }
        }
        let hdd_node = hdd
            .get("node-name")
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty())
            .ok_or_else(|| {
                XemuBridgeError::Emulator("managed xemu HDD omitted its block node name".into())
            })?;
        Ok(BlockLayout {
            hdd_node: hdd_node.into(),
        })
    }

    pub(super) fn next_snapshot_tag(&mut self) -> String {
        format!(
            "emucap-{}",
            ulid::Ulid::generate().to_string().to_ascii_lowercase()
        )
    }

    fn next_job_id(&mut self, operation: &str) -> XemuResult<String> {
        let id = self.next_state_job_id;
        self.next_state_job_id = id
            .checked_add(1)
            .ok_or_else(|| XemuBridgeError::Emulator("state job id space exhausted".into()))?;
        Ok(format!("emucap-{operation}-{id}"))
    }

    pub(super) fn run_snapshot_job(
        &mut self,
        command: &str,
        tag: &str,
        hdd_node: &str,
    ) -> XemuResult<()> {
        let operation = command.strip_prefix("snapshot-").unwrap_or(command);
        let job_id = self.next_job_id(operation)?;
        let arguments = if command == "snapshot-delete" {
            json!({"job-id":job_id, "tag":tag, "devices":[hdd_node]})
        } else {
            json!({
                "job-id":job_id,
                "tag":tag,
                "vmstate":hdd_node,
                "devices":[hdd_node],
            })
        };
        self.qmp.execute(command, Some(arguments))?;
        let deadline = Instant::now() + STATE_JOB_TIMEOUT;
        loop {
            let jobs = self.qmp.execute("query-jobs", None)?;
            let jobs = jobs.as_array().ok_or_else(|| {
                XemuBridgeError::Emulator("xemu query-jobs did not return a list".into())
            })?;
            if let Some(job) = jobs
                .iter()
                .find(|job| job.get("id").and_then(Value::as_str) == Some(job_id.as_str()))
            {
                let status = job.get("status").and_then(Value::as_str).ok_or_else(|| {
                    XemuBridgeError::Emulator("xemu state job omitted status".into())
                })?;
                if status == "concluded" {
                    let job_error = job.get("error").and_then(Value::as_str).map(str::to_owned);
                    self.qmp
                        .execute("job-dismiss", Some(json!({"id":job_id})))?;
                    let _ = self.qmp.drain_events();
                    if let Some(error) = job_error {
                        return Err(XemuBridgeError::Emulator(format!(
                            "xemu {command} job failed: {error}"
                        )));
                    }
                    return Ok(());
                }
            }
            if Instant::now() >= deadline {
                let (settled, cleanup) = self.cancel_state_job(&job_id);
                if !settled {
                    self.state_integrity_error = Some(format!(
                        "xemu {command} job {job_id} exceeded its deadline and cleanup was not proven: {cleanup}"
                    ));
                }
                return Err(XemuBridgeError::Emulator(format!(
                    "xemu {command} did not reach a terminal job state within {} ms; cleanup {cleanup}",
                    STATE_JOB_TIMEOUT.as_millis(),
                )));
            }
            std::thread::sleep(STATE_JOB_POLL_INTERVAL);
        }
    }

    pub(super) fn delete_snapshot(&mut self, tag: &str, hdd_node: &str) -> XemuResult<()> {
        self.run_snapshot_job("snapshot-delete", tag, hdd_node)
    }

    pub(super) fn retry_pending_snapshot_cleanup(&mut self) {
        let pending = std::mem::take(&mut self.pending_state_snapshot_cleanup);
        for (tag, node) in pending {
            if self.delete_snapshot(&tag, &node).is_err() {
                self.pending_state_snapshot_cleanup.push((tag, node));
            }
        }
    }

    fn cancel_state_job(&mut self, job_id: &str) -> (bool, String) {
        if let Err(error) = self.qmp.execute("job-cancel", Some(json!({"id":job_id}))) {
            return (false, format!("cancel failed ({error})"));
        }
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let jobs = match self.qmp.execute("query-jobs", None) {
                Ok(value) => value,
                Err(error) => return (false, format!("terminal query failed ({error})")),
            };
            let Some(jobs) = jobs.as_array() else {
                return (false, "terminal query was not a list".into());
            };
            let Some(job) = jobs
                .iter()
                .find(|job| job.get("id").and_then(Value::as_str) == Some(job_id))
            else {
                return (true, "completed; job no longer present".into());
            };
            if job.get("status").and_then(Value::as_str) == Some("concluded") {
                return match self.qmp.execute("job-dismiss", Some(json!({"id":job_id}))) {
                    Ok(_) => (true, "completed and dismissed".into()),
                    Err(error) => (false, format!("dismiss failed ({error})")),
                };
            }
            if Instant::now() >= deadline {
                return (false, "did not conclude within the cleanup deadline".into());
            }
            std::thread::sleep(STATE_JOB_POLL_INTERVAL);
        }
    }
}
