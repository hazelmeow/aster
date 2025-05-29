use crate::{
    app::app_log,
    proto::{ProtocolEvent, acb::SignedMessage, clock::Clock, dgm::Operation},
};
use anyhow::Context;
use futures::{SinkExt, StreamExt};
use iroh::{
    NodeId,
    endpoint::{Connection, RecvStream, SendStream},
};
use serde::{Deserialize, Serialize};
use tokio::sync::{
    mpsc::{self, UnboundedReceiver, UnboundedSender},
    oneshot,
};
use tokio_util::{
    codec::{FramedRead, FramedWrite, LengthDelimitedCodec},
    sync::CancellationToken,
};

#[derive(Debug, Serialize, Deserialize)]
pub enum Message {
    Handshake {
        group_id: u64,
        local_clock: Clock,
    },
    Welcome {
        group_id: u64,
        local_clock: Clock,
        messages: Vec<SignedMessage<Operation>>,
    },

    Sync {
        messages: Vec<SignedMessage<Operation>>,
    },

    FileList(Vec<String>),
}

struct StreamOpen(oneshot::Sender<anyhow::Result<(SendStream, RecvStream)>>);
pub struct StreamAccept(pub SendStream, pub RecvStream);

#[derive(Debug)]
pub struct Peer {
    pub node_id: NodeId,
    pub cancel_token: CancellationToken,
    pub tx: mpsc::UnboundedSender<Message>,

    stream_open_tx: mpsc::UnboundedSender<StreamOpen>,
}

impl Peer {
    pub fn close(&self) {
        self.cancel_token.cancel();
    }

    pub fn send(&self, message: Message) {
        let _ = self.tx.send(message);
    }

    pub fn open_stream(&self) -> impl Future<Output = anyhow::Result<(SendStream, RecvStream)>> {
        // only need a clone of stream_open_tx, so clone and move into a new future
        // avoids holding peer mutex locked while waiting for stream
        let stream_open_tx = self.stream_open_tx.clone();
        Box::pin(async move {
            let (stream_tx, stream_rx) = oneshot::channel();
            let req = StreamOpen(stream_tx);
            stream_open_tx.send(req)?;
            stream_rx.await?
        })
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
///     - Always Message::Handshake
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
    connection: Connection,
    mut send_stream: SendStream,
    mut recv_stream: RecvStream,
) -> (
    Peer,
    UnboundedReceiver<Message>,
    UnboundedReceiver<StreamAccept>,
) {
    let node_id = connection.remote_node_id().unwrap();

    let (send_tx, mut send_rx) = mpsc::unbounded_channel::<Message>();
    let (recv_tx, recv_rx) = mpsc::unbounded_channel::<Message>();

    let (stream_open_tx, mut stream_open_rx) = mpsc::unbounded_channel::<StreamOpen>();
    let (stream_accept_tx, stream_accept_rx) = mpsc::unbounded_channel::<StreamAccept>();

    let cancel_token = CancellationToken::new();

    // spawn a task to read from the connection, decode messages, and send to the recv channel
    tokio::spawn({
        let cancel_token = cancel_token.clone();
        async move {
            // wrap recv_stream in length delimited codec
            let mut transport = FramedRead::new(recv_stream, LengthDelimitedCodec::new());

            loop {
                tokio::select! {
                    res = transport.next() => {
                        match res {
                            Some(Ok(bytes)) => {
                                let msg: Message = postcard::from_bytes(&bytes).unwrap();
                                // app_log!("[proto] got message from {node_id}: {msg:?}");
                                recv_tx.send(msg).unwrap();
                            }
                            Some(Err(e)) => {
                                app_log!("[proto] recv stream error: {}", e.to_string());
                                cancel_token.cancel();
                                break;
                            }
                            None => {
                                // stream finished
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
            // wrap send_stream in length delimited codec
            let mut transport = FramedWrite::new(send_stream, LengthDelimitedCodec::new());

            loop {
                tokio::select! {
                    res = send_rx.recv() => {
                        match res {
                            Some(msg) => {
                                // app_log!("[proto] sending message to {node_id}: {msg:?}");
                                let buf = postcard::to_stdvec(&msg).unwrap();
                                transport.send(buf.into()).await.unwrap();
                            }
                            None => {
                                // channel closed
                                break;
                            }
                        }
                    }

                    _ = cancel_token.cancelled() => {
                        let _ = transport.into_inner().finish();
                        break;
                    }
                }
            }
        }
    });

    // spawn a task to own the connection to create streams and wait for closure
    tokio::spawn({
        let cancel_token = cancel_token.clone();
        async move {
            loop {
                tokio::select! {
                    // wait for requests for new streams
                    Some(req) = stream_open_rx.recv() => {
                        // open the stream and send it back to the requestor
                        let res = connection.open_bi().await.context("failed to open stream");
                        req.0.send(res).unwrap();
                    }

                    // accept new streams and send to main protocol
                    res = connection.accept_bi() => {
                        match res {
                            Ok((send_stream, recv_stream)) => {
                                stream_accept_tx.send(StreamAccept(send_stream, recv_stream)).unwrap();
                            }
                            Err(e) => {
                                app_log!("error accepting stream: {e:#}");
                            }
                        }
                    }

                    // if the connection closes for any reason, cancel to start cleanup
                    _ = connection.closed() => {
                        cancel_token.cancel();
                        break;
                    }

                    _ = cancel_token.cancelled() => {
                        // explicitly close the connection
                        connection.close(0u32.into(), b"closed");
                        break;
                    }
                }
            }
        }
    });

    let peer = Peer {
        node_id,
        cancel_token,
        tx: send_tx,
        stream_open_tx,
    };

    (peer, recv_rx, stream_accept_rx)
}
