use crate::proto::Protocol;
use anyhow::Context;
use bytes::Bytes;
use futures::Stream;
use iroh::{
    NodeId,
    endpoint::{RecvStream, SendStream},
};
use serde::{Deserialize, Serialize};
use std::{io::Cursor, pin::Pin, sync::Arc};
use stream_download::source::{DecodeError, SourceStream};
use tokio::io::{AsyncReadExt, AsyncSeekExt, BufReader};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct InitRequest {
    file_hash: u64,
    start: u64,
    end: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct InitResponse {
    content_length: u64,
}

/// Handles the connect end of a content stream.
///
/// Returns the content length and the byte stream.
pub async fn connect_stream(
    mut send_stream: SendStream,
    mut recv_stream: RecvStream,
    file_hash: u64,
    start: u64,
    end: Option<u64>,
) -> anyhow::Result<(
    u64,
    impl Stream<Item = Result<Bytes, anyhow::Error>> + Send + Sync + Unpin,
)> {
    // send init request
    let init_req = InitRequest {
        file_hash,
        start,
        end,
    };
    let init_req_buf =
        postcard::to_stdvec(&init_req).context("failed to serialize init request")?;
    send_stream
        .write_all(&init_req_buf)
        .await
        .context("failed to write init request")?;

    // receive init response
    let mut init_res_buf = [0; 1024];
    let init_res_bytes = recv_stream
        .read(&mut init_res_buf)
        .await
        .context("failed to read init response")?;
    let Some(init_res_bytes) = init_res_bytes else {
        anyhow::bail!("stream finished unexpectedly");
    };
    let init_res: InitResponse = postcard::from_bytes(&init_res_buf[0..init_res_bytes])
        .context("failed to deserialize init response")?;

    // read incoming bytes on recv_stream and convert to async stream
    let stream = async_stream::try_stream! {
        let mut chunk_buf = vec![0; 1024];
        while let Some(n_bytes) = recv_stream.read(&mut chunk_buf).await? {
            yield Bytes::from(Bytes::copy_from_slice(&chunk_buf[0..n_bytes]));
        }
    };

    Ok((init_res.content_length, Box::pin(stream)))
}

const TEST_FILE: &[u8; 77184] = include_bytes!("../../test.mp3");

/// Handles the accept end of a content stream.
pub async fn accept_stream(
    mut send_stream: SendStream,
    mut recv_stream: RecvStream,
) -> anyhow::Result<()> {
    // receive init request
    let mut init_req_buf = [0; 1024];
    let init_req_bytes = recv_stream
        .read(&mut init_req_buf)
        .await
        .context("failed to read init request")?;
    let Some(init_req_bytes) = init_req_bytes else {
        anyhow::bail!("stream finished unexpectedly");
    };
    let init_req: InitRequest = postcard::from_bytes(&init_req_buf[0..init_req_bytes])
        .context("failed to deserialize init request")?;

    // TODO: read content length
    let content_length = 14046228;

    // send init response
    let init_res = InitResponse { content_length };
    let init_res_buf =
        postcard::to_stdvec(&init_res).context("failed to serialize init response")?;
    send_stream
        .write_all(&init_res_buf)
        .await
        .context("failed to write init response")?;

    let start_pos = init_req.start;
    let end_pos = init_req.end.unwrap_or(content_length);

    // open reader
    let mut rdr = BufReader::new(Cursor::new(TEST_FILE));

    // seek reader if not starting from 0
    if start_pos != 0 {
        rdr.seek(std::io::SeekFrom::Start(start_pos)).await?;
    }

    // read until end_pos
    let len = end_pos - start_pos;
    let mut rdr = rdr.take(len);

    // read chunks and write them to the stream until reader is finished
    let mut buf = [0; 8192];
    loop {
        let n = rdr.read(&mut buf).await?;
        if n == 0 {
            break;
        };
        send_stream.write_all(&buf[0..n]).await?;
    }

    Ok(())
}

/// A stream implementing stream_download::SourceStream with support for seeking.
///
/// Opens a QUIC stream initially, then replaces the stream if seeking is needed.
pub struct ProtocolStream {
    protocol: Arc<Protocol>,
    peer_id: NodeId,
    file_hash: u64,
    content_length: u64,
    stream: Box<dyn Stream<Item = Result<Bytes, anyhow::Error>> + Send + Sync + Unpin>,
}

// defer stream implementation to inner stream
// allows for swapping streams
impl Stream for ProtocolStream {
    type Item = Result<Bytes, anyhow::Error>;

    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        Pin::new(&mut self.stream).poll_next(cx)
    }
}

impl SourceStream for ProtocolStream {
    type Params = (Arc<Protocol>, NodeId, u64);
    type StreamCreationError = StreamDownloadError;

    async fn create(params: Self::Params) -> Result<Self, Self::StreamCreationError> {
        let (protocol, peer_id, file_hash) = params;

        // open stream with peer
        let (send_stream, recv_stream) = {
            let peers = protocol.sync_peers.lock().await;
            let Some(peer) = peers.get(&peer_id) else {
                return Err(anyhow::anyhow!("peer not connected").into());
            };
            peer.open_stream().await?
        };

        // connect, init, and create async stream
        let (content_length, stream) =
            connect_stream(send_stream, recv_stream, file_hash, 0, None).await?;

        Ok(Self {
            protocol,
            peer_id,
            file_hash,
            content_length,
            stream: Box::new(stream),
        })
    }

    fn content_length(&self) -> Option<u64> {
        Some(self.content_length)
    }

    async fn seek_range(&mut self, start: u64, end: Option<u64>) -> std::io::Result<()> {
        // check if seeking to end of stream
        if start >= self.content_length {
            self.stream = Box::new(futures_util::stream::empty());
            return Ok(());
        }

        // open new stream with peer
        let (send_stream, recv_stream) = {
            let peers = self.protocol.sync_peers.lock().await;
            let Some(peer) = peers.get(&self.peer_id) else {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::HostUnreachable,
                    anyhow::anyhow!("peer not connected"),
                ));
            };
            peer.open_stream()
                .await
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::HostUnreachable, e))?
        };

        // create new stream with start and end
        let (_content_length, stream) =
            connect_stream(send_stream, recv_stream, self.file_hash, start, end)
                .await
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::ConnectionAborted, e))?;

        self.stream = Box::new(stream);

        Ok(())
    }

    async fn reconnect(&mut self, current_position: u64) -> std::io::Result<()> {
        self.seek_range(current_position, None).await
    }

    fn supports_seek(&self) -> bool {
        true
    }
}

/// Wrapper for anyhow::Error so we can implement stream_download::DecodeError
pub struct StreamDownloadError(anyhow::Error);

impl From<anyhow::Error> for StreamDownloadError {
    fn from(value: anyhow::Error) -> Self {
        Self(value)
    }
}

impl std::fmt::Debug for StreamDownloadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl std::fmt::Display for StreamDownloadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl std::error::Error for StreamDownloadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.0.source()
    }

    #[allow(deprecated)]
    fn description(&self) -> &str {
        self.0.description()
    }

    #[allow(deprecated)]
    fn cause(&self) -> Option<&dyn std::error::Error> {
        self.0.cause()
    }
}

impl DecodeError for StreamDownloadError {}
