use std::io::Write;

use super::error::PublishError;
use super::publish::BoundedEventWriter;

impl BoundedEventWriter {
    pub fn write_record(&mut self, record: &[u8]) -> Result<(), PublishError> {
        if record.is_empty()
            || !record.ends_with(b"\n")
            || record[..record.len() - 1].contains(&b'\n')
        {
            return Err(PublishError::InvalidRecord);
        }
        let record_bytes =
            u64::try_from(record.len()).map_err(|_| PublishError::ByteLimit(self.max_bytes))?;
        if record_bytes > self.max_line_bytes {
            return Err(PublishError::LineLimit(self.max_line_bytes));
        }
        let next_events = self
            .events
            .checked_add(1)
            .ok_or(PublishError::EventLimit(self.max_events))?;
        if next_events > self.max_events {
            return Err(PublishError::EventLimit(self.max_events));
        }
        let next_bytes = self
            .bytes
            .checked_add(record_bytes)
            .ok_or(PublishError::ByteLimit(self.max_bytes))?;
        if next_bytes > self.max_bytes {
            return Err(PublishError::ByteLimit(self.max_bytes));
        }
        self.file.write_all(record)?;
        self.events = next_events;
        self.bytes = next_bytes;
        Ok(())
    }

    pub fn events(&self) -> u64 {
        self.events
    }

    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    pub fn finish(self) -> Result<(), PublishError> {
        // Publication and every in-process failure path finish at one durability boundary. An
        // abandoned staging directory is quarantined rather than promoted from an unproven
        // per-record prefix, so forcing every high-rate event to disk here is unnecessary.
        self.file.sync_all()?;
        Ok(())
    }
}
