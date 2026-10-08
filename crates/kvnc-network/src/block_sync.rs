//! Block sync request-response protocol.
//!
//! Uses libp2p's request-response behaviour to fetch blocks by hash or by
//! (author, round) pair.

use futures::{AsyncReadExt, AsyncWriteExt};
use kvnc_types::{block::StatementBlock, hash::Hash, AuthorityIndex, Round};
use libp2p::request_response::{OutboundRequestId, ResponseChannel};
use serde::{Deserialize, Serialize};

/// Protocol name for block sync request-response.
pub const BLOCK_SYNC_PROTOCOL: &str = "/kovanica/block-sync/1.0.0";

/// Maximum size of a block sync response (1 MB).
#[allow(dead_code)]
pub const MAX_RESPONSE_SIZE: usize = 1024 * 1024;

/// Request for a block by hash or by (author, round).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BlockRequest {
    /// Request a block by its hash.
    ByHash(Hash),
    /// Request a block by author and round.
    ByAuthorRound { author: AuthorityIndex, round: Round },
}

/// Response to a block request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BlockResponse {
    /// The requested block.
    Block(StatementBlock),
    /// Block not found.
    NotFound,
    /// Request was invalid.
    InvalidRequest,
}

impl std::fmt::Display for BlockResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BlockResponse::Block(_) => write!(f, "Block"),
            BlockResponse::NotFound => write!(f, "NotFound"),
            BlockResponse::InvalidRequest => write!(f, "InvalidRequest"),
        }
    }
}

/// Codec for the block sync request-response protocol.
#[derive(Debug, Clone, Default)]
pub struct BlockSyncCodec;

impl libp2p::request_response::Codec for BlockSyncCodec {
    type Protocol = libp2p::StreamProtocol;
    type Request = BlockRequest;
    type Response = BlockResponse;

    async fn read_request<T>(&mut self, _: &libp2p::StreamProtocol, io: &mut T) -> Result<Self::Request, std::io::Error>
    where
        T: AsyncReadExt + Unpin + Send,
    {
        let mut buf = Vec::new();
        io.read_to_end(&mut buf).await?;
        bincode::deserialize(&buf).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }

    async fn read_response<T>(&mut self, _: &libp2p::StreamProtocol, io: &mut T) -> Result<Self::Response, std::io::Error>
    where
        T: AsyncReadExt + Unpin + Send,
    {
        let mut buf = Vec::new();
        io.read_to_end(&mut buf).await?;
        bincode::deserialize(&buf).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }

    async fn write_request<T>(&mut self, _: &libp2p::StreamProtocol, io: &mut T, req: Self::Request) -> Result<(), std::io::Error>
    where
        T: AsyncWriteExt + Unpin + Send,
    {
        let data = bincode::serialize(&req).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        io.write_all(&data).await
    }

    async fn write_response<T>(&mut self, _: &libp2p::StreamProtocol, io: &mut T, resp: Self::Response) -> Result<(), std::io::Error>
    where
        T: AsyncWriteExt + Unpin + Send,
    {
        let data = bincode::serialize(&resp).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        io.write_all(&data).await
    }
}

/// Request-response protocol configuration.
pub fn block_sync_protocol() -> libp2p::request_response::Config {
    libp2p::request_response::Config::default()
        .with_request_timeout(std::time::Duration::from_secs(30))
        .with_max_concurrent_streams(100)
}

/// Event emitted when a block sync request is received.
#[derive(Debug)]
pub struct BlockSyncRequestEvent {
    /// Peer that sent the request.
    pub peer: libp2p::PeerId,
    /// The request.
    pub request: BlockRequest,
    /// Channel to send the response.
    pub channel: ResponseChannel<BlockResponse>,
}

/// Event emitted when a block sync response is received.
#[derive(Debug, Clone)]
pub struct BlockSyncResponseEvent {
    /// Peer that sent the response.
    pub peer: libp2p::PeerId,
    /// The request ID this response corresponds to.
    pub request_id: OutboundRequestId,
    /// The response.
    pub response: BlockResponse,
}

#[cfg(test)]
mod tests {
    use super::*;
    use kvnc_types::hash::Hash;

    #[test]
    fn block_request_by_hash_round_trip() {
        let hash = Hash::new("test-hash");
        let req = BlockRequest::ByHash(hash);
        let encoded = bincode::serialize(&req).expect("serializes");
        let decoded: BlockRequest = bincode::deserialize(&encoded).expect("deserializes");
        assert_eq!(req, decoded);
    }

    #[test]
    fn block_request_by_author_round_round_trip() {
        let req = BlockRequest::ByAuthorRound { author: 5, round: 42 };
        let encoded = bincode::serialize(&req).expect("serializes");
        let decoded: BlockRequest = bincode::deserialize(&encoded).expect("deserializes");
        assert_eq!(req, decoded);
    }

    #[test]
    fn block_response_round_trip() {
        let resp = BlockResponse::NotFound;
        let encoded = bincode::serialize(&resp).expect("serializes");
        let decoded: BlockResponse = bincode::deserialize(&encoded).expect("deserializes");
        assert_eq!(resp, decoded);
    }
}