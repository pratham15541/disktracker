use std::io;
use tokio::io::{AsyncRead, AsyncWrite};

/// A trait for IPC stream connections.
pub trait IpcStream: AsyncRead + AsyncWrite + Unpin + Send + 'static {}

/// Blanket implementation for any compatible async stream.
impl<T: AsyncRead + AsyncWrite + Unpin + Send + 'static> IpcStream for T {}

/// A trait for IPC listeners accepting incoming IPC streams.
#[allow(async_fn_in_trait)]
pub trait IpcListener: Send + Sync {
    type Stream: IpcStream;

    /// Accept the next incoming connection.
    async fn accept(&mut self) -> io::Result<Self::Stream>;
}
