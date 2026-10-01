//! Tool admission is held by the native worker even if its MCP response future disappears.
use emucap::live::link::RequestCancellation;
use rmcp::service::{RequestContext, RoleServer};
use std::sync::{Arc, Mutex};
use tokio::sync::OwnedSemaphorePermit;

#[derive(Clone)]
pub struct Admission(
    Arc<Mutex<Option<OwnedSemaphorePermit>>>,
    RequestCancellation,
);
impl Admission {
    pub fn new(permit: OwnedSemaphorePermit, cancellation: RequestCancellation) -> Self {
        Self(Arc::new(Mutex::new(Some(permit))), cancellation)
    }
    pub fn cancellation(context: &RequestContext<RoleServer>) -> RequestCancellation {
        context
            .extensions
            .get::<Self>()
            .map(|value| value.1.clone())
            .unwrap_or_default()
    }
    pub fn take(context: &RequestContext<RoleServer>) -> Option<OwnedSemaphorePermit> {
        context
            .extensions
            .get::<Self>()
            .and_then(|value| value.0.lock().unwrap_or_else(|e| e.into_inner()).take())
    }
}

pub struct CancelOnDrop(pub RequestCancellation);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

/// The transport owns disconnect observation; rmcp may drain handlers before dropping them.
#[derive(Clone, Default)]
pub struct Connection(Arc<Mutex<ConnectionState>>);
#[derive(Default)]
struct ConnectionState {
    closed: bool,
    active: Option<RequestCancellation>,
}
impl Connection {
    pub fn register(&self) -> RequestCancellation {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let cancellation = RequestCancellation::default();
        if state.closed {
            cancellation.cancel();
        }
        state.active = Some(cancellation.clone());
        cancellation
    }
    fn close(&self) {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        state.closed = true;
        if let Some(active) = &state.active {
            active.cancel();
        }
    }
    pub fn transport<T>(&self, inner: T) -> OwnedTransport<T> {
        OwnedTransport {
            inner,
            connection: self.clone(),
        }
    }
}

pub struct OwnedTransport<T> {
    inner: T,
    connection: Connection,
}
impl<T> Drop for OwnedTransport<T> {
    fn drop(&mut self) {
        self.connection.close();
    }
}
impl<T: rmcp::transport::Transport<RoleServer>> rmcp::transport::Transport<RoleServer>
    for OwnedTransport<T>
{
    type Error = T::Error;
    fn send(
        &mut self,
        item: rmcp::service::TxJsonRpcMessage<RoleServer>,
    ) -> impl std::future::Future<Output = Result<(), Self::Error>> + Send + 'static {
        let send = self.inner.send(item);
        let connection = self.connection.clone();
        async move {
            let result = send.await;
            if result.is_err() {
                connection.close();
            }
            result
        }
    }
    async fn receive(&mut self) -> Option<rmcp::service::RxJsonRpcMessage<RoleServer>> {
        let message = self.inner.receive().await;
        if message.is_none() {
            self.connection.close();
        }
        message
    }
    async fn close(&mut self) -> Result<(), Self::Error> {
        self.connection.close();
        self.inner.close().await
    }
}
