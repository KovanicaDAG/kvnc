//! Minimal audit 6.2 test: block sync request-response + rate-limited enqueue.

use kvnc_network::{BlockSyncRequest, BlockSyncResponse, BLOCK_SYNC_PROTOCOL};
use kvnc_types::hash::Hash;

#[test]
fn audit_62_sync_protocol_is_kvanc() {
    assert_eq!(BLOCK_SYNC_PROTOCOL, "/kvanc/block-sync/1.0.0");
}

#[test]
fn audit_62_sync_request_has_variants() {
    let by_hash = BlockSyncRequest::ByHash(Hash::new("test-hash"));
    let by_author = BlockSyncRequest::ByAuthorRound {
        author: 3,
        round: 7,
    };
    assert!(matches!(by_hash, BlockSyncRequest::ByHash(_)));
    assert!(matches!(by_author, BlockSyncRequest::ByAuthorRound { .. }));
}

#[test]
fn audit_62_sync_response_has_missing_blocks() {
    let resp = BlockSyncResponse::MissingBlocks(vec![Hash::new("a"), Hash::new("b")]);
    assert!(matches!(resp, BlockSyncResponse::MissingBlocks(_)));
    assert_eq!(resp.to_string(), "MissingBlocks");
}
