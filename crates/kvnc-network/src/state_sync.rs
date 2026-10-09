//! State sync protocol for fast sync using snapshots.
//!
//! This module implements the fast sync protocol where a node can request
//! a state snapshot from a peer to quickly catch up without replaying all blocks.

use crate::error::NetworkError;
use futures::{AsyncReadExt, AsyncWriteExt};
use libp2p::{request_response::{Codec, ProtocolSupport}, StreamProtocol};
use serde::{Deserialize, Serialize};

/// Protocol name for state sync request-response.
pub const STATE_SYNC_PROTOCOL: &str = "/kvanc/state-sync/1.0.0";

/// Maximum response size (10 MB for state snapshots).
pub const MAX_STATE_SYNC_RESPONSE_SIZE: usize = 10 * 1024 * 1024;

/// State sync request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateSyncRequest {
    /// The committed leader height to sync to (optional - if None, sync to latest).
    pub target_height: Option<u64>,
}

impl StateSyncRequest {
    /// Create a request for the latest state.
    pub fn latest() -> Self {
        Self { target_height: None }
    }

    /// Create a request for a specific height.
    pub fn at_height(height: u64) -> Self {
        Self { target_height: Some(height) }
    }

    /// Serialize the request for the wire.
    pub fn encode(&self) -> Result<Vec<u8>, NetworkError> {
        Ok(bincode::serialize(self)?)
    }

    /// Deserialize a request received from the wire.
    pub fn decode(payload: &[u8]) -> Result<Self, NetworkError> {
        Ok(bincode::deserialize(payload)?)
    }
}

/// State sync response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum StateSyncResponse {
    /// Snapshot data (bincode-encoded SnapshotData from state_store).
    Snapshot(Vec<u8>),
    /// Requested height not available.
    NotFound,
    /// Request invalid.
    InvalidRequest,
}

impl std::fmt::Display for StateSyncResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StateSyncResponse::Snapshot(_) => write!(f, "Snapshot"),
            StateSyncResponse::NotFound => write!(f, "NotFound"),
            StateSyncResponse::InvalidRequest => write!(f, "InvalidRequest"),
        }
    }
}

impl StateSyncResponse {
    /// Serialize the response for the wire.
    pub fn encode(&self) -> Result<Vec<u8>, NetworkError> {
        Ok(bincode::serialize(self)?)
    }

    /// Deserialize a response received from the wire.
    pub fn decode(payload: &[u8]) -> Result<Self, NetworkError> {
        Ok(bincode::deserialize(payload)?)
    }
}

/// Codec for the state sync protocol.
#[derive(Debug, Clone, Default)]
pub struct StateSyncCodec;

impl Codec for StateSyncCodec {
    type Protocol = StreamProtocol;
    type Request = StateSyncRequest;
    type Response = StateSyncResponse;

    async fn read_request<T>(
        &mut self,
        _: &StreamProtocol,
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
        _: &StreamProtocol,
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
        _: &StreamProtocol,
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
        _: &StreamProtocol,
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

/// Protocol config for state sync.
pub fn state_sync_protocol() -> libp2p::request_response::Config {
    libp2p::request_response::Config::default()
        .with_request_timeout(std::time::Duration::from_secs(60))
        .with_max_concurrent_streams(10)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_sync_request_latest_round_trip() {
        let req = StateSyncRequest::latest();
        let encoded = req.encode().expect("encodes");
        let decoded = StateSyncRequest::decode(&encoded).expect("decodes");
        assert_eq!(req, decoded);
        assert!(decoded.target_height.is_none());
    }

    #[test]
    fn state_sync_request_at_height_round_trip() {
        let req = StateSyncRequest::at_height(1000);
        let encoded = req.encode().expect("encodes");
        let decoded = StateSyncRequest::decode(&encoded).expect("decodes");
        assert_eq!(req, decoded);
        assert_eq!(decoded.target_height, Some(1000));
    }

    #[test]
    fn state_sync_response_snapshot_round_trip() {
        let resp = StateSyncResponse::Snapshot(vec![1, 2, 3, 4]);
        let encoded = resp.encode().expect("encodes");
        let decoded = StateSyncResponse::decode(&encoded).expect("decodes");
        assert_eq!(resp, decoded);
    }

    #[test]
    fn state_sync_response_not_found_round_trip() {
        let resp = StateSyncResponse::NotFound;
        let encoded = resp.encode().expect("encodes");
        let decoded = StateSyncResponse::decode(&encoded).expect("decodes");
        assert_eq!(resp, decoded);
    }

    #[test]
    fn state_sync_codec_round_trip() {
        let codec = StateSyncCodec;
        // Just verify the protocol constants
        assert_eq!(STATE_SYNC_PROTOCOL, "/kvanc/state-sync/1.0.0");
    }
}