use super::*;
#[test]
fn owned_frame_parser_requires_identity_progress_and_stop_outcome() {
    assert!(
        FrameReply::parse("FRAME|child|interrupted|3|120|cancelled", "child", 120)
            .unwrap()
            .terminal()
    );
    assert!(
        !FrameReply::parse("FRAME|child|stopping|3|120|cancelled", "child", 120)
            .unwrap()
            .terminal()
    );
    for raw in [
        "OK",
        "FRAME|old|completed|120|120|none",
        "FRAME|child|completed|3|120|none",
        "FRAME|child|interrupted|121|120|cancelled",
        "FRAME|child|interrupted|3|120|none",
        "FRAME|child|unverified|3|120|cancelled",
        "FRAME|child|completed|120|120|none|extra",
    ] {
        assert!(FrameReply::parse(raw, "child", 120).is_err(), "{raw}");
    }
}

// Exercise the real socket decoder, including the ACK/reply split, and retain
// late bytes after expiry to prove that another command cannot reuse them.
#[test]
fn owned_exchange_tolerates_delayed_reply_but_retires_expired_stream() {
    use crate::gdb_rsp::GdbRspClient;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    for (budget, delay, succeeds) in [
        (Duration::from_millis(35), Duration::from_millis(80), false),
        (EXCHANGE_BUDGET, Duration::from_millis(80), true),
        (EXCHANGE_BUDGET, Duration::from_millis(350), true),
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (done, finish) = std::sync::mpsc::channel();
        let peer = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut byte = [0];
            loop {
                stream.read_exact(&mut byte).unwrap();
                if byte[0] == b'#' {
                    break;
                }
            }
            let mut checksum = [0; 2];
            stream.read_exact(&mut checksum).unwrap();
            stream.write_all(b"+").unwrap();
            std::thread::sleep(delay);
            let _ = stream.write_all(b"$FRAME|child|running|0|120|none#4f");
            let _ = finish.recv_timeout(Duration::from_secs(5));
        });
        let mut client = GdbRspClient::connect(
            "127.0.0.1",
            port,
            Duration::from_secs(2),
            Duration::from_secs(1),
        )
        .unwrap();
        let result = exchange(&mut client, "framepoll", "child", budget, |_| {
            panic!("unexpected stop")
        });
        if succeeds {
            assert_eq!(result.unwrap(), "FRAME|child|running|0|120|none");
            assert!(!client.is_terminal());
        } else {
            let error = result.unwrap_err();
            assert!(error.to_string().contains("MAME framepoll exchange"));
            assert!(client.is_terminal());
            assert!(matches!(client.send("?"), Err(GdbError::Poisoned)));
        }
        assert_eq!(client.get_timeout().unwrap(), Duration::from_secs(2));
        let _ = done.send(());
        peer.join().unwrap();
        if !succeeds {
            assert!(matches!(client.recv_reply(), Err(GdbError::Poisoned)));
        }
    }
}

#[test]
fn frame_exchange_uses_only_the_remaining_owner_budget() {
    struct Host {
        budgets: Vec<Duration>,
    }
    impl FrameHost for Host {
        type Error = GdbError;
        fn exchange(&mut self, _: &str, _: &str, budget: Duration) -> GdbResult<String> {
            self.budgets.push(budget);
            Ok("reply".into())
        }
        fn set_frozen(&mut self, _: bool) {}
    }
    let mut host = Host {
        budgets: Vec::new(),
    };
    let deadline = Instant::now() + Duration::from_millis(80);
    assert_eq!(
        exchange_until(&mut host, "framecancel", "child", deadline).unwrap(),
        "reply"
    );
    assert!(host.budgets[0] <= Duration::from_millis(80));
    let expired = Instant::now() - Duration::from_millis(1);
    assert!(exchange_until(&mut host, "framefinish", "child", expired).is_err());
    assert_eq!(host.budgets.len(), 1);
}
