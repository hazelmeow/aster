use crate::{
    app::app_log,
    proto::{
        acb::SignedMessage,
        dgm::{GroupMembership, Operation},
    },
};
use iroh::{
    Endpoint, NodeAddr, NodeId, SecretKey,
    endpoint::{RecvStream, SendStream},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};
use tokio::sync::{Mutex, mpsc};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Serialize, Deserialize)]
pub enum Message {
    Ping(u32),
    Pong(u32),
    Text(String),
    Welcome {
        messages: Vec<SignedMessage<Operation>>,
    },
    Sync {
        messages: Vec<SignedMessage<Operation>>,
    },
}

#[derive(Debug)]
pub struct Peer {
    tx: mpsc::UnboundedSender<Message>,
    rx: mpsc::UnboundedReceiver<Message>,
    cancel_token: CancellationToken,
}

impl Peer {
    pub fn send(&self, message: Message) {
        let _ = self.tx.send(message);
    }

    pub fn close(&self) {
        self.cancel_token.cancel();
    }
}

#[derive(Debug, Clone)]
pub struct SyncProtocol {
    peers: Arc<Mutex<HashMap<NodeId, Peer>>>,
}

impl SyncProtocol {
    pub const ALPN: &[u8] = b"aster/0";

    pub fn new() -> Self {
        Self {
            peers: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn connect(&self, endpoint: &Endpoint, addr: NodeAddr) -> anyhow::Result<()> {
        let node_id = addr.node_id;
        app_log!("[proto] connecting to {}", node_id);

        // Open a connection to the accepting node
        let conn = endpoint.connect(addr, SyncProtocol::ALPN).await?;

        app_log!("[proto] connected");

        // Open a bidirectional QUIC stream
        let (send_stream, recv_stream) = conn.open_bi().await?;

        app_log!("[proto] opened stream");

        let peer = handle_connection_streams(node_id, send_stream, recv_stream);

        // spawn a task for cleanup after cancellation
        tokio::spawn({
            let peers = self.peers.clone();
            let cancel_token = peer.cancel_token.clone();
            async move {
                // wait for cancellation
                cancel_token.cancelled().await;

                // explicitly close the conncetion
                conn.close(0u32.into(), b"closed");

                // remove from peer map
                {
                    let mut peers = peers.lock().await;
                    peers.remove(&node_id);
                }

                app_log!("[proto] connection to {} closed", node_id);
            }
        });

        peer.tx.send(Message::Ping(1));

        // insert into peer map
        {
            let mut peers = self.peers.lock().await;
            peers.insert(node_id, peer);
        }

        app_log!("[proto] initialized");

        Ok(())
    }

    pub fn peers(&self) -> &Arc<Mutex<HashMap<NodeId, Peer>>> {
        &self.peers
    }
}

// TODO: error handling
impl iroh::protocol::ProtocolHandler for SyncProtocol {
    /// The `accept` method is called for each incoming connection for our ALPN.
    ///
    /// The returned future runs on a newly spawned tokio task, so it can run as long as
    /// the connection lasts without blocking other connections.
    fn accept(
        &self,
        connection: iroh::endpoint::Connection,
    ) -> n0_future::boxed::BoxFuture<anyhow::Result<()>> {
        let peers = self.peers.clone();

        Box::pin(async move {
            // get the remote node id
            let node_id = connection.remote_node_id()?;
            app_log!("[proto] accepted connection from {node_id}");

            // we expect the connecting peer to open a single bi-directional stream
            let (send_stream, recv_stream) = connection.accept_bi().await?;

            let peer = handle_connection_streams(node_id, send_stream, recv_stream);

            peer.tx.send(Message::Ping(2));

            let cancel_token = peer.cancel_token.clone();

            {
                let mut peers = peers.lock().await;
                peers.insert(node_id, peer);
            }

            // wait for connection close or cancellation
            tokio::select! {
                _ = connection.closed() => {}
                _ = cancel_token.cancelled() => {
                    connection.close(0u32.into(), b"closed");
                }
            }

            {
                let mut peers = peers.lock().await;
                peers.remove(&node_id);
            }

            app_log!("[proto] connection from {node_id} closed");

            Ok(())
        })
    }
}

fn handle_connection_streams(
    node_id: NodeId,
    mut send_stream: SendStream,
    mut recv_stream: RecvStream,
) -> Peer {
    let (send_tx, mut send_rx) = mpsc::unbounded_channel::<Message>();
    let (recv_tx, recv_rx) = mpsc::unbounded_channel::<Message>();

    let cancel_token = CancellationToken::new();

    // spawn a task to read from the connection, decode messages, and send to the recv channel
    tokio::spawn({
        let cancel_token = cancel_token.clone();
        async move {
            let mut recv_buf = [0; 1024];

            loop {
                tokio::select! {
                    res = recv_stream.read(&mut recv_buf) => {
                        match res {
                            Ok(Some(n)) => {
                                let msg: Message = postcard::from_bytes(&recv_buf[0..n]).unwrap();
                                app_log!("[proto] got message from {node_id}: {msg:?}");
                                recv_tx.send(msg).unwrap();
                            }
                            Ok(None) => {
                                // stream finished
                                cancel_token.cancel();
                                break;
                            }
                            Err(e) => {
                                app_log!("[proto] recv stream error: {}", e.to_string());
                                cancel_token.cancel();
                                break;
                            }
                        }
                    }

                    _ = cancel_token.cancelled() => {
                        break;
                    }
                }
            }
        }
    });

    // spawn a task to read from the send channel, encode messages, and write to the connection
    tokio::spawn({
        let cancel_token = cancel_token.clone();
        async move {
            loop {
                tokio::select! {
                    res = send_rx.recv() => {
                        match res {
                            Some(msg) => {
                                app_log!("[proto] sending message to {node_id}: {msg:?}");
                                let buf = postcard::to_stdvec(&msg).unwrap();
                                send_stream.write_all(&buf).await.unwrap();
                            }
                            None => {
                                // channel closed
                                break;
                            }
                        }
                    }

                    _ = cancel_token.cancelled() => {
                        let _ = send_stream.finish();
                        break;
                    }
                }
            }
        }
    });

    Peer {
        tx: send_tx,
        rx: recv_rx,
        cancel_token,
    }
}
