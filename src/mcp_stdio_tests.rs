use crate::mcp_stdio::{BoundedStdioTransport, MAX_MCP_STDIN_LINE_BYTES};
use rmcp::{
    model::{ClientJsonRpcMessage, JsonRpcMessage},
    transport::Transport,
    RoleServer,
};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

fn modern_discover(id: u64) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "server/discover",
        "params": {
            "_meta": {
                "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                "io.modelcontextprotocol/clientInfo": {
                    "name": "bounded-stdio-test",
                    "version": "1"
                },
                "io.modelcontextprotocol/clientCapabilities": {}
            }
        }
    })
}

async fn response_line(reader: &mut BufReader<tokio::io::DuplexStream>) -> serde_json::Value {
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    serde_json::from_str(&line).unwrap()
}

#[tokio::test]
async fn malformed_first_request_is_rejected_before_valid_discover_is_admitted() {
    let (mut client_in, server_in) = tokio::io::duplex(4096);
    let (server_out, client_out) = tokio::io::duplex(4096);
    let mut responses = BufReader::new(client_out);
    let mut transport = BoundedStdioTransport::new(server_in, server_out);

    client_in
        .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"server/discover\",\"params\":{}}\n")
        .await
        .unwrap();
    client_in
        .write_all(format!("{}\n", modern_discover(2)).as_bytes())
        .await
        .unwrap();

    let admitted = Transport::<RoleServer>::receive(&mut transport)
        .await
        .expect("valid discover must be admitted");
    let ClientJsonRpcMessage::Request(request) = admitted else {
        panic!("expected request");
    };
    assert_eq!(request.id.to_string(), "2");

    let rejected = response_line(&mut responses).await;
    assert_eq!(rejected["id"], 1);
    assert_eq!(rejected["error"]["code"], -32600);
    assert_eq!(
        rejected["error"]["data"]["missing"],
        json!([
            "io.modelcontextprotocol/protocolVersion",
            "io.modelcontextprotocol/clientCapabilities"
        ])
    );
}

#[tokio::test]
async fn oversized_first_line_is_bounded_and_next_request_is_admitted() {
    let capacity = MAX_MCP_STDIN_LINE_BYTES + 4096;
    let (mut client_in, server_in) = tokio::io::duplex(capacity);
    let (server_out, client_out) = tokio::io::duplex(4096);
    let mut responses = BufReader::new(client_out);
    let mut transport = BoundedStdioTransport::new(server_in, server_out);

    let mut oversized = vec![b'x'; MAX_MCP_STDIN_LINE_BYTES + 1];
    oversized.push(b'\n');
    client_in.write_all(&oversized).await.unwrap();
    client_in
        .write_all(format!("{}\n", modern_discover(3)).as_bytes())
        .await
        .unwrap();

    let admitted = Transport::<RoleServer>::receive(&mut transport)
        .await
        .expect("valid request after oversized line must be admitted");
    assert!(matches!(admitted, JsonRpcMessage::Request(_)));

    let rejected = response_line(&mut responses).await;
    assert_eq!(rejected["id"], Value::Null);
    assert_eq!(rejected["error"]["code"], -32600);
    assert!(rejected["error"]["message"]
        .as_str()
        .unwrap()
        .contains("1048576-byte limit"));
}
