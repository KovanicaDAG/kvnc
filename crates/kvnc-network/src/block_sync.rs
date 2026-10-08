//! Block sync request-response protocol (`/kvanc/block-sync/1.0.0`).
//!
//! Uses libp2p's request-response behaviour to fetch blocks by hash or by
//! (author, round) pair.

use futures::{AsyncReadExt, AsyncWriteExt};
use kvnc_types::{block::StatementBlock, hash::Hash, AuthorityIndex, Round};
use libp2p::request_response::{OutboundRequestId, ResponseChannel};
use serde::{Deserialize, Serialize};

/// Protocol name for block sync request-response.
pub const BLOCK_SYNC_PROTOCOL: &str = "/kvanc/block-sync/1.0.0";

/// Maximum response size (1 MB).
#[allow(dead_code)]
pub const MAX_RESPONSE_SIZE: usize = 1024 * 1024;

/// Block sync request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BlockSyncRequest {
    /// Request by block hash.
    ByHash(Hash),
    /// Request by author and round.
    ByAuthorRound {
        author: AuthorityIndex,
        round: Round,
    },
}

/// Block sync response — includes missing blocks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BlockSyncResponse {
    /// The requested block.
    Block(StatementBlock),
    /// Block not found.
    NotFound,
    /// Request invalid.
    InvalidRequest,
    /// List of missing parent blocks required before the requested block.
    MissingBlocks(Vec<Hash>),
}

impl std::fmt::Display for BlockSyncResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BlockSyncResponse::Block(_) => write!(f, "Block"),
            BlockSyncResponse::NotFound => write!(f, "NotFound"),
            BlockSyncResponse::InvalidRequest => write!(f, "InvalidRequest"),
            BlockSyncResponse::MissingBlocks(_) => write!(f, "MissingBlocks"),
        }
    }
}

/// Codec for the block sync protocol.
#[derive(Debug, Clone, Default)]
pub struct BlockSyncCodec;

impl libp2p::request_response::Codec for BlockSyncCodec {
    type Protocol = libp2p::StreamProtocol;
    type Request = BlockSyncRequest;
    type Response = BlockSyncResponse;

    async fn read_request<T>(
        &mut self,
        _: &libp2p::StreamProtocol,
        io: &mut T,
    ) -> Result<Self::Request, std::io::Error>
    where
        T: AsyncReadExt + Unpin + Send,
    {
        let mut buf = Vec::new();
        io.read_to_end(&mut buf).await?;
        bincode::deserialize(&buf)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }

    async fn read_response<T>(
        &mut self,
        _: &libp2p::StreamProtocol,
        io: &mut T,
    ) -> Result<Self::Response, std::io::Error>
    where
        T: AsyncReadExt + Unpin + Send,
    {
        let mut buf = Vec::new();
        io.read_to_end(&mut buf).await?;
        bincode::deserialize(&buf)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }

    async fn write_request<T>(
        &mut self,
        _: &libp2p::StreamProtocol,
        io: &mut T,
        req: Self::Request,
    ) -> Result<(), std::io::Error>
    where
        T: AsyncWriteExt + Unpin + Send,
    {
        let data = bincode::serialize(&req)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        io.write_all(&data).await
    }

    async fn write_response<T>(
        &mut self,
        _: &libp2p::StreamProtocol,
        io: &mut T,
        resp: Self::Response,
    ) -> Result<(), std::io::Error>
    where
        T: AsyncWriteExt + Unpin + Send,
    {
        let data = bincode::serialize(&resp)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        io.write_all(&data).await
    }
}

/// Protocol config.
pub fn block_sync_protocol() -> libp2p::request_response::Config {
    libp2p::request_response::Config::default()
        .with_request_timeout(std::time::Duration::from_secs(30))
        .with_max_concurrent_streams(100)
}

/// Request event.
#[derive(Debug)]
pub struct BlockSyncRequestEvent {
    pub peer: libp2p::PeerId,
    pub request: BlockSyncRequest,
    pub channel: ResponseChannel<BlockSyncResponse>,
}

/// Response event.
#[derive(Debug, Clone)]
pub struct BlockSyncResponseEvent {
    pub peer: libp2p::PeerId,
    pub request_id: OutboundRequestId,
    pub response: BlockSyncResponse,
}

#[cfg(test)]
mod tests {
    use super::*;
    use kvnc_types::hash::Hash;

    #[test]
    fn block_sync_request_by_hash_round_trip() {
        let hash = Hash::new("test-hash");
        let req = BlockSyncRequest::ByHash(hash);
        let encoded = bincode::serialize(&req).expect("serializes");
        let decoded: BlockSyncRequest = bincode::deserialize(&encoded).expect("deserializes");
        assert_eq!(req, decoded);
    }

    #[test]
    fn block_sync_request_by_author_round_round_trip() {
        let req = BlockSyncRequest::ByAuthorRound {
            author: 5,
            round: 42,
        };
        let encoded = bincode::serialize(&req).expect("serializes");
        let decoded: BlockSyncRequest = bincode::deserialize(&encoded).expect("deserializes");
        assert_eq!(req, decoded);
    }

    #[test]
    fn block_sync_response_round_trip() {
        let resp = BlockSyncResponse::NotFound;
        let encoded = bincode::serialize(&resp).expect("serializes");
        let decoded: BlockSyncResponse = bincode::deserialize(&encoded).expect("deserializes");
        assert_eq!(resp, decoded);
    }

    #[test]
    fn block_sync_response_missing_blocks_round_trip() {
        let resp = BlockSyncResponse::MissingBlocks(vec![Hash::new("a"), Hash::new("b")]);
        let encoded = bincode::serialize(&resp).expect("serializes");
        let decoded: BlockSyncResponse = bincode::deserialize(&encoded).expect("deserializes");
        assert_eq!(resp, decoded);
    }
}
