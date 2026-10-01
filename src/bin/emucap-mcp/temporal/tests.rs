use super::*;
use crate::Emucap;
use emucap::live::link::{Capabilities, ProgressCallControl, ProgressObserver};
use emucap::live::reconnect::cancellation::OperationKey;
use emucap::live::temporal::CancellationCapability;
use rmcp::ServiceExt;
use serde_json::{json, Value};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, ReadHalf, WriteHalf};
use tokio::sync::Notify;

struct Rpc {
    reader: BufReader<ReadHalf<DuplexStream>>,
    writer: WriteHalf<DuplexStream>,
    task: tokio::task::JoinHandle<()>,
}
impl Rpc {
    async fn new(server: Emucap) -> Self {
        let (transport, client) = tokio::io::duplex(65536);
        let task = tokio::spawn(async move {
            let (read, write) = tokio::io::split(transport);
            let transport = server.connection.transport(
                rmcp::transport::async_rw::AsyncRwTransport::new_server(read, write),
            );
            server
                .serve(transport)
                .await
                .unwrap()
                .waiting()
                .await
                .unwrap();
        });
        let (reader, writer) = tokio::io::split(client);
        let mut rpc = Self {
            reader: BufReader::new(reader),
            writer,
            task,
        };
        rpc.send(
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
                "protocolVersion":"2024-11-05","capabilities":{},
                "clientInfo":{"name":"temporal-test","version":"0"}
            }}),
        )
        .await;
        assert_eq!(rpc.response().await["id"], 1);
        rpc.send(json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
            .await;
        rpc
    }
    async fn send(&mut self, value: Value) {
        self.writer
            .write_all(format!("{value}\n").as_bytes())
            .await
            .unwrap();
    }
    async fn call(&mut self, id: u64, name: &str, arguments: Value) {
        self.send(json!({"jsonrpc":"2.0","id":id,"method":"tools/call",
            "params":{"name":name,"arguments":arguments}}))
            .await;
    }
    async fn response(&mut self) -> Value {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let mut line = String::new();
                assert_ne!(self.reader.read_line(&mut line).await.unwrap(), 0);
                let value: Value = serde_json::from_str(&line).unwrap();
                if value.get("id").is_some() {
                    return value;
                }
            }
        })
        .await
        .expect("MCP response deadline")
    }
    async fn close(self) {
        drop(self.reader);
        drop(self.writer);
        tokio::time::timeout(Duration::from_secs(5), self.task)
            .await
            .unwrap()
            .unwrap();
    }
}

pub(crate) async fn request(server: Emucap, name: &str, arguments: Value) -> Value {
    let mut rpc = Rpc::new(server).await;
    rpc.call(2, name, arguments).await;
    let response = rpc.response().await;
    assert_eq!(response["id"], 2);
    assert!(response.get("error").is_none(), "{response}");
    rpc.close().await;
    response["result"].clone()
}

struct Signals {
    started: Notify,
    cancelled: Notify,
    finished: Notify,
}
struct Native {
    caps: Capabilities,
    signals: Arc<Signals>,
    release: mpsc::Receiver<()>,
    held: bool,
    frozen: bool,
}
impl EmulatorLink for Native {
    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }
    fn attachment_id(&self) -> Option<&str> {
        Some("attachment-a")
    }
    fn begin_temporal_control(&mut self, key: &OperationKey) -> Result<(), LinkError> {
        assert_eq!(key.owner_id, "attachment-a");
        Ok(())
    }
    fn finish_temporal_control(
        &mut self,
        _: &OperationKey,
        verified: bool,
    ) -> Result<(), LinkError> {
        assert!(verified);
        assert!(self.frozen);
        assert!(!self.held);
        self.signals.finished.notify_one();
        Ok(())
    }
    fn call(&mut self, method: &str, params: Value) -> Result<Value, LinkError> {
        match method {
            "begin_temporal_operation" => {
                return Ok(json!({"status":"admitted","parent":params["parent"]}))
            }
            "finish_temporal_operation" => {
                assert!(self.frozen && !self.held);
                return Ok(json!({"status":"completed","parent":params["parent"],
                    "cleanup_verified":true,"effects_started":true,"state":"frozen","released_ports":[0]}));
            }
            "pause" => self.frozen = true,
            "set_input" => self.held = !params["buttons"].as_array().unwrap().is_empty(),
            "status" => (),
            _ => panic!("unexpected native call: {method}"),
        }
        Ok(json!({"state":if self.frozen {"frozen"} else {"running"},"frame":1}))
    }
    fn call_with_progress(
        &mut self,
        method: &str,
        params: Value,
        _: &mut ProgressObserver<'_>,
        control: &ProgressCallControl,
    ) -> Result<Value, LinkError> {
        if method != "step" {
            assert!(control.max_host_ms.unwrap() <= control.temporal_stop_ms.unwrap());
            assert_eq!(
                control.abort.as_ref().unwrap().params,
                params["_temporal_owner"]
            );
            if method == "finish_temporal_operation" {
                assert!(!control.cancellation.is_cancelled());
            }
            return self.call(method, params);
        }
        assert_eq!(method, "step");
        assert!(self.held);
        self.frozen = false;
        self.signals.started.notify_one();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !control.cancellation.is_cancelled() {
            assert!(Instant::now() < deadline, "cancellation was not delivered");
            std::thread::sleep(Duration::from_millis(1));
        }
        self.signals.cancelled.notify_one();
        self.release
            .recv_timeout(Duration::from_secs(5))
            .expect("cleanup release");
        self.frozen = true;
        Ok(
            json!({"status":"interrupted","reason":"cancelled","unit":"frames","count":1,"state":"frozen"}),
        )
    }
}
async fn notified(signal: &Notify) {
    tokio::time::timeout(Duration::from_secs(5), signal.notified())
        .await
        .unwrap();
}
async fn fixture() -> (Rpc, Emucap, Arc<Signals>, mpsc::Sender<()>) {
    let mut caps = Capabilities::empty();
    caps.methods = ["step", "status", "set_input", "pause"]
        .map(String::from)
        .to_vec();
    caps.identity.launch_id = Some("launch-a".into());
    caps.features.temporal_cancellation = Some(CancellationCapability {
        methods: vec!["step".into()],
        control_service_ms: 25,
        stop_host_ms: 500,
    });
    let signals = Arc::new(Signals {
        started: Notify::new(),
        cancelled: Notify::new(),
        finished: Notify::new(),
    });
    let (release, receiver) = mpsc::channel();
    let server = Emucap::new(Arc::new(Mutex::new(Native {
        caps,
        signals: signals.clone(),
        release: receiver,
        held: false,
        frozen: true,
    })));
    let mut rpc = Rpc::new(server.clone()).await;
    rpc.call(
        2,
        "tap",
        json!({"buttons":["a"],"press_frames":6,"after_frames":120}),
    )
    .await;
    notified(&signals.started).await;
    (rpc, server, signals, release)
}
async fn idle(server: &Emucap) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while server.control_slot.available_permits() != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn cancellation_holds_admission_until_stop_and_input_release() {
    let (mut rpc, server, signals, release) = fixture().await;
    for id in [3, 4] {
        if id == 4 {
            rpc.send(json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":2}})).await;
            notified(&signals.cancelled).await;
        }
        rpc.call(id, "status", json!({})).await;
        let response = rpc.response().await;
        assert_eq!(
            response["id"], id,
            "cancelled call must not publish a result"
        );
        assert_eq!(
            response["result"]["structuredContent"]["error"]["code"], "busy",
            "{response}"
        );
    }
    release.send(()).unwrap();
    notified(&signals.finished).await;
    idle(&server).await;
    rpc.call(5, "status", json!({})).await;
    let response = rpc.response().await;
    assert_eq!(response["id"], 5);
    assert_ne!(response["result"]["isError"], true, "{response}");
    rpc.close().await;
}

#[tokio::test]
async fn transport_loss_keeps_worker_ownership_through_cleanup() {
    let (rpc, server, signals, release) = fixture().await;
    drop(rpc.reader);
    drop(rpc.writer);
    notified(&signals.cancelled).await;
    assert_eq!(server.control_slot.available_permits(), 0);
    release.send(()).unwrap();
    notified(&signals.finished).await;
    idle(&server).await;
    tokio::time::timeout(Duration::from_secs(5), rpc.task)
        .await
        .unwrap()
        .unwrap();
}
