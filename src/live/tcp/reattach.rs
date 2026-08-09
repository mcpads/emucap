use std::net::TcpListener;

use serde_json::Value;

use super::{io_to_link, split_addr, TcpLink, AUTO_PORT_RANGE};
use crate::live::link::{EmulatorLink, LinkError};
use crate::live::{continuity, runtime};

pub(super) fn returned_generation(
    link: &mut TcpLink,
    expected_launch_id: &str,
) -> Result<Value, LinkError> {
    if link.conn.is_some() && link.caps.identity.launch_id.as_deref() != Some(expected_launch_id) {
        return Err(LinkError::Busy);
    }
    let (host, base) = split_addr(&link.addr);
    if base == 0 || link.base_port == 0 {
        return Err(LinkError::Emulator {
            kind: "unsupported".into(),
            message: "explicit reattachment requires a configured direct port range".into(),
        });
    }

    let mut matching_generations = Vec::new();
    for offset in 0..AUTO_PORT_RANGE {
        let Some(port) = link.base_port.checked_add(offset) else {
            break;
        };
        let current = link.runtime_store.read_current(port).map_err(io_to_link)?;
        if current
            .as_ref()
            .is_some_and(|current| current.launch_id == expected_launch_id)
        {
            matching_generations.push((port, current.expect("checked current")));
        }
    }
    let (port, current) = match matching_generations.len() {
        0 => {
            return Err(LinkError::Emulator {
                kind: "no_such_generation".into(),
                message: "the requested launch_id is not current in this listener range".into(),
            });
        }
        1 => matching_generations.pop().expect("one runtime generation"),
        _ => {
            return Err(LinkError::Emulator {
                kind: "ambiguous_generation".into(),
                message: "the requested launch_id appears in more than one runtime slot".into(),
            });
        }
    };
    if current.process_state() != runtime::ProcessState::Alive {
        return Err(LinkError::Emulator {
            kind: "execution_not_alive".into(),
            message: "the requested runtime generation is not verifiably alive".into(),
        });
    }
    if current
        .bridge_process_state()
        .is_some_and(|state| state != runtime::ProcessState::Alive)
    {
        return Err(LinkError::Emulator {
            kind: "backend_unavailable".into(),
            message: "the requested runtime generation's bridge is not verifiably alive".into(),
        });
    }

    let token = link
        .runtime_store
        .read_auth(port, expected_launch_id)
        .map_err(io_to_link)?
        .ok_or_else(|| LinkError::Emulator {
            kind: "reclaim_capability_unavailable".into(),
            message: "the requested runtime generation has no private reclaim capability".into(),
        })?;
    let holder = runtime::capture_process(std::process::id());
    let existing_record = link
        .runtime_store
        .read_link_json::<continuity::LinkRecord>(port, expected_launch_id)
        .map_err(io_to_link)?
        .filter(|record| record.launch_id == expected_launch_id)
        .ok_or_else(|| LinkError::Emulator {
            kind: "lease_unavailable".into(),
            message: "the requested runtime generation has no verifiable returned lease".into(),
        })?;
    let existing_lease = existing_record
        .lease
        .as_ref()
        .ok_or_else(|| LinkError::Emulator {
            kind: "lease_unavailable".into(),
            message: "the requested runtime generation has no verifiable returned lease".into(),
        })?;
    let existing_lease_view = continuity::lease_view(existing_lease, &holder);
    if !matches!(
        existing_lease_view.state,
        runtime::LeaseState::Held | runtime::LeaseState::Available
    ) {
        return Err(LinkError::Busy);
    }

    let already_bound = link.endpoint_port() == Some(port);
    let prepared_listener = if already_bound {
        None
    } else {
        let listener = TcpListener::bind(format!("{host}:{port}")).map_err(|error| {
            if error.kind() == std::io::ErrorKind::AddrInUse {
                LinkError::PortBusy {
                    addr: format!("{host}:{port}"),
                }
            } else {
                io_to_link(error)
            }
        })?;
        listener.set_nonblocking(true).map_err(io_to_link)?;
        Some(listener)
    };
    let prepared_preaccept_listener = if let Some(listener) = prepared_listener.as_ref() {
        Some(listener.try_clone().map_err(io_to_link)?)
    } else if link.conn.is_none() && link.preaccept.is_none() {
        Some(
            link.listener
                .as_ref()
                .ok_or(LinkError::NotConnected)?
                .try_clone()
                .map_err(io_to_link)?,
        )
    } else {
        None
    };

    // Publish the generation's immutable compatibility capability only while the same current
    // generation is locked and its returned lease is claimed. A rejected handoff changes neither.
    let record = continuity::claim_generation_lease(
        &link.runtime_store,
        port,
        expected_launch_id,
        holder.clone(),
        runtime::control_session_key(),
        Some(token.clone()),
    )
    .map_err(|error| match error.kind() {
        std::io::ErrorKind::PermissionDenied => LinkError::Busy,
        _ => io_to_link(error),
    })?;
    let lease = record
        .lease
        .as_ref()
        .map(|lease| continuity::lease_view(lease, &holder))
        .unwrap_or_else(runtime::LeaseView::unknown);
    if lease.state != runtime::LeaseState::Held {
        return Err(LinkError::Busy);
    }

    if let Some(listener) = prepared_listener {
        link.cancel_preaccept();
        link.drop_conn();
        link.addr = listener
            .local_addr()
            .map(|address| address.to_string())
            .unwrap_or_else(|_| format!("{host}:{port}"));
        link.listener = Some(listener);
    }
    link.staged_reclaim_token = None;
    link.session_token.clear();
    link.session_token.push_str(&token);
    *link
        .preaccept_token
        .write()
        .unwrap_or_else(|error| error.into_inner()) = token;
    link.runtime_candidates.clear();
    if let Some(listener) = prepared_preaccept_listener {
        link.start_preaccept(listener);
    } else {
        link.arm_preaccept()?;
    }

    Ok(serde_json::json!({
        "launch_id": expected_launch_id,
        "listening_port": port,
        "lease": lease,
        "connection": if link.conn.is_some() { "connected" } else { "pending" },
    }))
}
