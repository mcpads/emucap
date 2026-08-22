use std::path::Path;

/// Preserve the legacy `rom_sha1` slot as the opaque Tracking identifier. Exact composite content
/// identity takes precedence over emulator layout IDs and entry-file-only hashes.
pub(crate) fn normalize_rom_sha1(
    value: &mut serde_json::Value,
    content_identity: Option<&emucap::content_identity::ContentIdentity>,
) {
    fn valid(value: Option<&str>) -> Option<&str> {
        value.filter(|value| !value.is_empty() && *value != "skipped:too_large")
    }
    let Some(object) = value.as_object_mut() else {
        return;
    };
    if let Some(identity) = content_identity {
        object.insert(
            "content_identity".into(),
            serde_json::to_value(identity).expect("content identity is serializable"),
        );
        object.insert(
            "content_identity_binding".into(),
            serde_json::json!("prelaunch"),
        );
        object.insert("rom_sha1".into(), serde_json::json!(identity.tracking_id()));
        return;
    }
    if object.contains_key("rom_sha1") {
        return;
    }
    let canonical = valid(object.get("content_md5").and_then(|value| value.as_str()))
        .or_else(|| valid(object.get("sha1").and_then(|value| value.as_str())))
        .map(String::from);
    if let Some(canonical) = canonical {
        object.insert("rom_sha1".into(), serde_json::json!(canonical));
    }
}

fn same_content_path(expected: &str, observed: &str) -> bool {
    let expected = Path::new(expected);
    let observed = Path::new(observed);
    match (expected.canonicalize(), observed.canonicalize()) {
        (Ok(expected), Ok(observed)) => expected == observed,
        _ => expected == observed,
    }
}

/// Return the identity captured before a managed launch. This read-only status path never expands
/// an adapter-reported descriptor path into indirect filesystem reads.
pub(crate) fn content_identity_for_rom_info(
    value: &serde_json::Value,
    endpoint_port: Option<u16>,
    live_launch_id: Option<&str>,
) -> std::io::Result<Option<emucap::content_identity::ContentIdentity>> {
    content_identity_for_rom_info_with_store(
        value,
        endpoint_port,
        live_launch_id,
        &emucap::live::runtime::RuntimeStore::discover(),
    )
}

pub(crate) fn content_identity_for_rom_info_with_store(
    value: &serde_json::Value,
    endpoint_port: Option<u16>,
    live_launch_id: Option<&str>,
    store: &emucap::live::runtime::RuntimeStore,
) -> std::io::Result<Option<emucap::content_identity::ContentIdentity>> {
    let observed_path = value.get("path").and_then(serde_json::Value::as_str);
    let observed_adapter = value.get("adapter").and_then(serde_json::Value::as_str);
    if let (Some(port), Some(live_launch_id)) = (endpoint_port, live_launch_id) {
        if let Some(current) = store.read_current(port)? {
            if current.launch_id == live_launch_id {
                if let Some(bound) = current.content_identity {
                    if observed_path
                        .is_some_and(|observed| !same_content_path(&current.content, observed))
                    {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            "managed adapter content path does not match its launch generation",
                        ));
                    }
                    return Ok(Some(bound));
                }
            }
        }
    }
    let descriptor = observed_path
        .and_then(|path| Path::new(path).extension())
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "cue" | "gdi" | "ccd" | "toc" | "m3u"
            )
        });
    if descriptor {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!(
                "unmanaged {} descriptor identity is unavailable without launch-time member approval",
                observed_adapter.unwrap_or("emulator")
            ),
        ));
    }
    Ok(None)
}
