use super::*;

impl RuntimeStore {
    /// Resolve an advertised managed launch without treating a broker port as its capsule port.
    pub(crate) fn current_for_launch(
        &self,
        launch_id: &str,
    ) -> io::Result<Option<CurrentManifest>> {
        if validate_launch_id(launch_id).is_err() {
            return Ok(None); // Outside the managed identity namespace; never form a path from it.
        }
        let entries = match fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let mut matched = None;
        let mut historical = false;
        for entry in entries {
            let entry = entry?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            let Ok(port) = name.parse::<u16>() else {
                continue;
            };
            if name != port.to_string() {
                continue;
            }
            if !entry.file_type()?.is_dir() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "runtime port entry is not a directory",
                ));
            }
            let current = self.read_current(port)?;
            if let Some(current) = current.filter(|current| current.launch_id == launch_id) {
                if matched.is_some() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "multiple current runtime capsules claim this launch identity",
                    ));
                }
                matched = Some(current);
            } else if self.generation_dir(port, launch_id).try_exists()? {
                historical = true;
            }
        }
        if historical {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "local launch generation has no matching current capsule",
            ));
        }
        Ok(matched)
    }
}
