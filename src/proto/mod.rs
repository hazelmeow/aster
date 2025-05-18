//! The app protocol.

use crate::app::app_log;

#[derive(Debug, Clone)]
pub struct Proto;

impl Proto {
    pub const ALPN: &[u8] = b"aster/0";
}

impl iroh::protocol::ProtocolHandler for Proto {
    /// The `accept` method is called for each incoming connection for our ALPN.
    ///
    /// The returned future runs on a newly spawned tokio task, so it can run as long as
    /// the connection lasts without blocking other connections.
    fn accept(
        &self,
        connection: iroh::endpoint::Connection,
    ) -> n0_future::boxed::BoxFuture<anyhow::Result<()>> {
        Box::pin(async move {
            // We can get the remote's node id from the connection.
            let node_id = connection.remote_node_id()?;
            app_log!("accepted connection from {node_id}");

            // Our protocol is a simple request-response protocol, so we expect the
            // connecting peer to open a single bi-directional stream.
            let (mut send, mut recv) = connection.accept_bi().await?;

            let msg = recv.read_to_end(1000).await?;
            let msg = String::from_utf8(msg)?;
            app_log!("got message: {msg}");

            // Echo any bytes received back directly.
            // This will keep copying until the sender signals the end of data on the stream.
            // let bytes_sent = tokio::io::copy(&mut recv, &mut send).await?;
            // app_log!("Copied over {bytes_sent} byte(s)");

            // By calling `finish` on the send stream we signal that we will not send anything
            // further, which makes the receive stream on the other end terminate.
            send.finish()?;

            // Wait until the remote closes the connection, which it does once it
            // received the response.
            connection.closed().await;

            app_log!("connection closed");

            Ok(())
        })
    }
}
