//! Group management protocol.

use crate::{app::app_log, proto::ProtocolEvent};
use iroh::{Endpoint, NodeAddr, endpoint::Connection};
use rand::Rng;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::{Mutex, mpsc::UnboundedSender};

const CODE_TTL: Duration = Duration::from_secs(60 * 2);
const EXPIRES_SOON_THRESHOLD: Duration = Duration::from_secs(60);
const OLD_THRESHOLD: Duration = Duration::from_secs(60 * 10);

#[derive(Debug, Clone)]
enum Response {
    Success = 0,
    ErrorInvalid = 1,
    ErrorExpired = 2,
    ErrorUsed = 3,
}

impl TryFrom<u8> for Response {
    type Error = anyhow::Error;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Ok(match value {
            0 => Response::Success,
            1 => Response::ErrorInvalid,
            2 => Response::ErrorInvalid,
            3 => Response::ErrorUsed,
            _ => anyhow::bail!("failed to parse join response"),
        })
    }
}

#[derive(Debug)]
struct Code {
    code: u64,
    expires_at: Instant,
    used: bool,
}

impl Code {
    fn new() -> Self {
        let code = rand::thread_rng().r#gen();

        let now = Instant::now();
        let expires_at = now + CODE_TTL;

        Self {
            code,
            expires_at,
            used: false,
        }
    }

    fn is_expired(&self) -> bool {
        let now = Instant::now();
        now >= self.expires_at
    }

    fn expires_soon(&self) -> bool {
        let now = Instant::now();
        let expires_soon_at = self.expires_at - EXPIRES_SOON_THRESHOLD;
        now >= expires_soon_at
    }

    fn is_old(&self) -> bool {
        let now = Instant::now();
        let old_at = self.expires_at + OLD_THRESHOLD;
        now >= old_at
    }
}

#[derive(Debug, Clone)]
pub struct JoinProtocol {
    codes: Arc<Mutex<Vec<Code>>>,
    tx: UnboundedSender<ProtocolEvent>,
}

impl JoinProtocol {
    pub const ALPN: &[u8] = b"aster/join/0";
}

impl JoinProtocol {
    pub fn new(tx: UnboundedSender<ProtocolEvent>) -> Self {
        Self {
            codes: Arc::new(Mutex::new(Vec::new())),
            tx,
        }
    }

    /// Get a code that doesn't expire soon.
    pub async fn get_code(&self) -> u64 {
        self.gc_codes().await;

        let mut codes = self.codes.lock().await;
        let code = codes.iter().find(|c| !c.expires_soon() && !c.used);
        if let Some(code) = code {
            code.code
        } else {
            let code = Code::new();
            let ret = code.code;

            codes.push(code);

            ret
        }
    }

    async fn gc_codes(&self) {
        let mut codes = self.codes.lock().await;
        codes.retain(|c| !c.is_old());
    }

    pub async fn join(&self, endpoint: &Endpoint, addr: NodeAddr, code: u64) -> anyhow::Result<()> {
        // connect to address
        let connection = endpoint.connect(addr, JoinProtocol::ALPN).await?;

        // open bidirectional stream
        let (mut send_stream, mut recv_stream) = connection.open_bi().await?;

        // write code as bytes
        send_stream.write_all(&code.to_be_bytes()).await?;
        send_stream.finish()?;

        // read response
        let response = recv_stream.read_to_end(8).await?;

        // close connection
        connection.close(0u32.into(), b"done");

        // parse response
        if response.len() != 1 {
            anyhow::bail!("unexpected response from join protocol");
        }
        let response: Response = response[0].try_into()?;

        match response {
            Response::Success => Ok(()),
            Response::ErrorInvalid => anyhow::bail!("code is invalid"),
            Response::ErrorExpired => anyhow::bail!("code is expired"),
            Response::ErrorUsed => anyhow::bail!("code was already used"),
        }
    }
}

impl iroh::protocol::ProtocolHandler for JoinProtocol {
    fn accept(&self, connection: Connection) -> n0_future::boxed::BoxFuture<anyhow::Result<()>> {
        let codes = self.codes.clone();
        let tx = self.tx.clone();
        Box::pin(async move {
            let node_id = connection.remote_node_id()?;

            // accept a bidirectional stream
            let (mut send_stream, mut recv_stream) = connection.accept_bi().await?;

            // read request
            let buf = recv_stream.read_to_end(8).await?;

            // parse request
            let Ok(code) = ('code: {
                if buf.len() != 8 {
                    break 'code Err(Response::ErrorInvalid);
                }

                let Ok(code_bytes) = TryInto::<[u8; 8]>::try_into(buf) else {
                    break 'code Err(Response::ErrorInvalid);
                };

                let code = u64::from_be_bytes(code_bytes);
                Ok(code)
            }) else {
                send_stream
                    .write_all(&[Response::ErrorInvalid as u8])
                    .await?;
                send_stream.finish()?;
                connection.close(0u32.into(), b"done"); // TODO: maybe shouldn't close on this end
                return Ok(());
            };

            {
                let mut codes = codes.lock().await;
                if let Some(code) = codes.iter_mut().find(|c| c.code == code) {
                    if code.used == true {
                        app_log!(
                            "[proto/join] peer {node_id} tried to join using code {} but it was already used",
                            code.code
                        );

                        send_stream.write_all(&[Response::ErrorUsed as u8]).await?;
                    } else if code.is_expired() {
                        app_log!(
                            "[proto/join] peer {node_id} tried to join using code {} but it was expired",
                            code.code
                        );

                        send_stream
                            .write_all(&[Response::ErrorExpired as u8])
                            .await?;
                    } else {
                        // tell protocol to add user to grup
                        tx.send(ProtocolEvent::AddGroupMember(node_id)).unwrap();

                        app_log!(
                            "[proto/join] peer {node_id} joined using code {}",
                            code.code
                        );

                        code.used = true;

                        send_stream.write_all(&[Response::Success as u8]).await?;
                    }
                } else {
                    app_log!("[proto/join] peer {node_id} tried to join using invalid code",);

                    send_stream
                        .write_all(&[Response::ErrorInvalid as u8])
                        .await?;
                }
            }

            send_stream.finish()?;

            connection.closed().await;

            Ok(())
        })
    }
}
