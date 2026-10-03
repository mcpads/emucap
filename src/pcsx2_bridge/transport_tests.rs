use super::*;
use std::os::unix::net::UnixListener;

#[test]
fn fragments_cannot_renew_the_whole_exchange_deadline() {
    for fragment_header in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("pine.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let host = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 5];
            stream.read_exact(&mut request).unwrap();
            let packet = [13, 0, 0, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8];
            let tail = if fragment_header {
                &packet[..]
            } else {
                stream.write_all(&packet[..5]).unwrap();
                &packet[5..]
            };
            for byte in tail {
                std::thread::sleep(Duration::from_millis(15));
                if stream.write_all(&[*byte]).is_err() {
                    break;
                }
            }
        });
        let mut pine = PineSocket::connect(1, Some(&path), Duration::from_secs(1)).unwrap();
        pine.stream
            .set_timeouts(Some(Duration::from_secs(1)), Some(Duration::from_secs(2)))
            .unwrap();
        let start = Instant::now();
        assert!(pine
            .transact_with_timeout(&[MSG_STATUS], Duration::from_millis(60))
            .is_err());
        assert!(start.elapsed() < Duration::from_secs(1));
        assert!(pine.is_terminal());
        assert_eq!(
            pine.stream.timeouts().unwrap(),
            (Some(Duration::from_secs(1)), Some(Duration::from_secs(2)))
        );
        assert!(pine.transact(&[MSG_STATUS]).is_err());
        drop(pine);
        host.join().unwrap();
    }
}
#[test]
fn invalid_budget_is_rejected_before_dispatch_and_keeps_transport_usable() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("pine.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let (done, finish) = std::sync::mpsc::channel();
    let host = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0; 5];
        stream.read_exact(&mut request).unwrap();
        assert_eq!(request, [5, 0, 0, 0, MSG_STATUS]);
        stream.write_all(&[5, 0, 0, 0, 0]).unwrap();
        let _ = finish.recv_timeout(Duration::from_secs(2));
    });
    let mut pine = PineSocket::connect(1, Some(&path), Duration::from_secs(1)).unwrap();
    assert!(pine
        .transact_with_timeout(&[MSG_VERSION], Duration::ZERO)
        .is_err());
    assert!(!pine.is_terminal());
    let result = pine.transact_with_timeout(&[MSG_STATUS], Duration::from_millis(200));
    assert!(result.is_ok(), "{result:?}");
    done.send(()).unwrap();
    host.join().unwrap();
}
