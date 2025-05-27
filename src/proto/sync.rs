use crate::{
    app::app_log,
    proto::{ProtocolEvent, acb::SignedMessage, clock::Clock, dgm::Operation},
};
use iroh::{
    NodeId,
    endpoint::{RecvStream, SendStream},
};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Serialize, Deserialize)]
pub enum Message {
    Handshake {
        group_id: u64,
        local_clock: Clock,
    },
    Welcome {
        group_id: u64,
        messages: Vec<SignedMessage<Operation>>,
    },

    Sync {
        messages: Vec<SignedMessage<Operation>>,
    },

    Ping(u32),
    Pong(u32),
    Text(String),
}

#[derive(Debug)]
pub struct Peer {
    pub tx: mpsc::UnboundedSender<Message>,
    pub cancel_token: CancellationToken,
}

impl Peer {
    pub fn send(&self, message: Message) {
        let _ = self.tx.send(message);
    }

    pub fn close(&self) {
        self.cancel_token.cancel();
    }
}

/// Sync protocol handler.
///
/// Protocol flow:
/// - A connects to B
/// - A sends handshake to B
///     - Message::Welcome if just added to group
///     - Message::Handshake if not new
/// - B sends handshake to A
/// - Peers reach connected state
/// - Peers sync ACB
#[derive(Debug, Clone)]
pub struct SyncProtocol {
    tx: UnboundedSender<ProtocolEvent>,
}

impl SyncProtocol {
    pub const ALPN: &[u8] = b"aster/0";

    pub fn new(tx: UnboundedSender<ProtocolEvent>) -> Self {
        Self { tx }
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
        self.tx
            .send(ProtocolEvent::SyncConnection(connection))
            .unwrap();

        Box::pin(async move { Ok(()) })
    }
}

// spawn tasks to bridge connection streams and message channels.
// return the receiver separately since it can't be cloned
pub fn handle_connection_streams(
    node_id: NodeId,
    mut send_stream: SendStream,
    mut recv_stream: RecvStream,
) -> (Peer, UnboundedReceiver<Message>) {
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

    let peer = Peer {
        tx: send_tx,
        cancel_token,
    };

    (peer, recv_rx)
}
