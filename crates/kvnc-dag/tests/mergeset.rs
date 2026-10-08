//! Unit tests for mergeset extraction.

use kvnc_dag::{mergeset::compute_mergeset, DagStore};
use kvnc_storage::Storage;
use kvnc_types::{
    block::{BlockReference, StatementBlock},
    crypto::SigningKey,
    AuthorityIndex, Round,
};
use tempfile::tempdir;

/// Build a test block with the given parameters.
fn make_block(
    author: AuthorityIndex,
    round: Round,
    parents: Vec<BlockReference>,
    tag: &str,
    key: &SigningKey,
) -> StatementBlock {
    let digest = StatementBlock::compute_digest(author, round, &parents, &[]);
    let signature = kvnc_crypto::sign(key, digest.as_ref());
    StatementBlock {
        author,
        round,
        parents,
        transactions: Vec::new(),
        statements: tag.as_bytes().to_vec(),
        signature,
        digest,
        merkle_root: Default::default(),
    }
}

fn block_ref(block: &StatementBlock) -> BlockReference {
    BlockReference {
        author: block.author,
        round: block.round,
        digest: block.digest,
    }
}

#[test]
fn test_mergeset_empty() {
    let dir = tempdir().unwrap();
    let storage = Storage::new(dir.path().join("test.db")).unwrap();
    let dag = DagStore::new(storage).unwrap();

    let (key, _) = kvnc_crypto::generate_keypair();
    let genesis = make_block(0, 0, Vec::new(), "genesis", &key);
    dag.put_block(&genesis).unwrap();

    let mergeset = dag.mergeset(&genesis.digest).unwrap();
    assert_eq!(mergeset.len(), 1);
    assert_eq!(mergeset[0], genesis.digest);
}

#[test]
fn test_mergeset_single_wave() {
    let dir = tempdir().unwrap();
    let storage = Storage::new(dir.path().join("test.db")).unwrap();
    let dag = DagStore::new(storage).unwrap();

    let (key, _) = kvnc_crypto::generate_keypair();

    // Genesis
    let genesis = make_block(0, 0, Vec::new(), "genesis", &key);
    dag.put_block(&genesis).unwrap();

    // Round 1 blocks
    let r1a = make_block(0, 1, vec![block_ref(&genesis)], "r1a", &key);
    let r1b = make_block(1, 1, vec![block_ref(&genesis)], "r1b", &key);
    dag.put_block(&r1a).unwrap();
    dag.put_block(&r1b).unwrap();

    // Round 2 leader (round 3 is leader round with WAVE_LENGTH=3)
    let leader = make_block(0, 3, vec![block_ref(&r1a), block_ref(&r1b)], "leader", &key);
    dag.put_block(&leader).unwrap();

    // No committed leaders yet - mergeset should include leader and all ancestors
    let mergeset = dag.mergeset(&leader.digest).unwrap();
    assert!(mergeset.contains(&leader.digest));
    assert!(mergeset.contains(&r1a.digest));
    assert!(mergeset.contains(&r1b.digest));
    assert!(mergeset.contains(&genesis.digest));
}

#[test]
fn test_mergeset_with_committed_leader() {
    let dir = tempdir().unwrap();
    let storage = Storage::new(dir.path().join("test.db")).unwrap();
    let dag = DagStore::new(storage).unwrap();

    let (key, _) = kvnc_crypto::generate_keypair();

    // Genesis
    let genesis = make_block(0, 0, Vec::new(), "genesis", &key);
    dag.put_block(&genesis).unwrap();

    // Round 1
    let r1a = make_block(0, 1, vec![block_ref(&genesis)], "r1a", &key);
    let r1b = make_block(1, 1, vec![block_ref(&genesis)], "r1b", &key);
    dag.put_block(&r1a).unwrap();
    dag.put_block(&r1b).unwrap();

    // Round 2
    let r2a = make_block(0, 2, vec![block_ref(&r1a)], "r2a", &key);
    let r2b = make_block(1, 2, vec![block_ref(&r1b)], "r2b", &key);
    dag.put_block(&r2a).unwrap();
    dag.put_block(&r2b).unwrap();

    // Round 3 - first leader (committed)
    let leader1 = make_block(
        0,
        3,
        vec![block_ref(&r2a), block_ref(&r2b)],
        "leader1",
        &key,
    );
    dag.put_block(&leader1).unwrap();
    dag.commit_leader(&leader1.digest).unwrap();

    // Round 4
    let r4a = make_block(0, 4, vec![block_ref(&leader1)], "r4a", &key);
    let r4b = make_block(1, 4, vec![block_ref(&leader1)], "r4b", &key);
    dag.put_block(&r4a).unwrap();
    dag.put_block(&r4b).unwrap();

    // Round 6 - second leader
    let leader2 = make_block(
        0,
        6,
        vec![block_ref(&r4a), block_ref(&r4b)],
        "leader2",
        &key,
    );
    dag.put_block(&leader2).unwrap();

    // Mergeset of leader2 should NOT include leader1 or its ancestors
    // (rounds <= 3 are already committed)
    let mergeset = dag.mergeset(&leader2.digest).unwrap();
    assert!(mergeset.contains(&leader2.digest));
    assert!(mergeset.contains(&r4a.digest));
    assert!(mergeset.contains(&r4b.digest));
    // leader1 and its ancestors should NOT be in mergeset
    assert!(!mergeset.contains(&leader1.digest));
    assert!(!mergeset.contains(&r2a.digest));
    assert!(!mergeset.contains(&r2b.digest));
    assert!(!mergeset.contains(&r1a.digest));
    assert!(!mergeset.contains(&r1b.digest));
    assert!(!mergeset.contains(&genesis.digest));
}

#[test]
fn test_mergeset_boundary_leader() {
    let dir = tempdir().unwrap();
    let storage = Storage::new(dir.path().join("test.db")).unwrap();
    let dag = DagStore::new(storage).unwrap();

    let (key, _) = kvnc_crypto::generate_keypair();

    // Genesis
    let genesis = make_block(0, 0, Vec::new(), "genesis", &key);
    dag.put_block(&genesis).unwrap();

    // Round 1
    let r1a = make_block(0, 1, vec![block_ref(&genesis)], "r1a", &key);
    dag.put_block(&r1a).unwrap();

    // Round 3 - first leader (committed)
    let leader1 = make_block(0, 3, vec![block_ref(&r1a)], "leader1", &key);
    dag.put_block(&leader1).unwrap();
    dag.commit_leader(&leader1.digest).unwrap();

    // Round 6 - second leader, but its parent is leader1 (boundary case)
    let leader2 = make_block(0, 6, vec![block_ref(&leader1)], "leader2", &key);
    dag.put_block(&leader2).unwrap();

    // Mergeset of leader2 should include leader2 but not leader1 (since leader1's round = 3 == committed_leader_round)
    let mergeset = dag.mergeset(&leader2.digest).unwrap();
    assert!(mergeset.contains(&leader2.digest));
    // leader1 should NOT be included because its round (3) <= committed_leader_round (3)
    assert!(!mergeset.contains(&leader1.digest));
    assert!(!mergeset.contains(&r1a.digest));
    assert!(!mergeset.contains(&genesis.digest));
}

#[test]
fn test_compute_mergeset_trait() {
    let dir = tempdir().unwrap();
    let storage = Storage::new(dir.path().join("test.db")).unwrap();
    let dag = DagStore::new(storage).unwrap();

    let (key, _) = kvnc_crypto::generate_keypair();

    // Genesis
    let genesis = make_block(0, 0, Vec::new(), "genesis", &key);
    dag.put_block(&genesis).unwrap();

    // Round 1
    let r1a = make_block(0, 1, vec![block_ref(&genesis)], "r1a", &key);
    dag.put_block(&r1a).unwrap();

    // Round 3 - leader
    let leader = make_block(0, 3, vec![block_ref(&r1a)], "leader", &key);
    dag.put_block(&leader).unwrap();

    // Test via the DagReachability trait
    let mergeset = compute_mergeset(&dag, &leader.digest).unwrap();
    assert!(mergeset.contains(&leader.digest));
    assert!(mergeset.contains(&r1a.digest));
    assert!(mergeset.contains(&genesis.digest));
}

#[test]
fn test_mergeset_multiple_authors() {
    let dir = tempdir().unwrap();
    let storage = Storage::new(dir.path().join("test.db")).unwrap();
    let dag = DagStore::new(storage).unwrap();

    let (key0, _) = kvnc_crypto::generate_keypair();
    let (key1, _) = kvnc_crypto::generate_keypair();

    // Genesis
    let genesis = make_block(0, 0, Vec::new(), "genesis", &key0);
    dag.put_block(&genesis).unwrap();

    // Round 1 - two authors
    let r1a = make_block(0, 1, vec![block_ref(&genesis)], "r1a", &key0);
    let r1b = make_block(1, 1, vec![block_ref(&genesis)], "r1b", &key1);
    dag.put_block(&r1a).unwrap();
    dag.put_block(&r1b).unwrap();

    // Round 2
    let r2a = make_block(0, 2, vec![block_ref(&r1a)], "r2a", &key0);
    let r2b = make_block(1, 2, vec![block_ref(&r1b)], "r2b", &key1);
    dag.put_block(&r2a).unwrap();
    dag.put_block(&r2b).unwrap();

    // Round 3 - leader from author 0
    let leader = make_block(
        0,
        3,
        vec![block_ref(&r2a), block_ref(&r2b)],
        "leader",
        &key0,
    );
    dag.put_block(&leader).unwrap();

    let mergeset = dag.mergeset(&leader.digest).unwrap();
    // leader(3), r2a(2), r2b(2), r1a(1), r1b(1), genesis(0) = 6
    assert_eq!(mergeset.len(), 6);
    assert!(mergeset.contains(&leader.digest));
    assert!(mergeset.contains(&r2a.digest));
    assert!(mergeset.contains(&r2b.digest));
    assert!(mergeset.contains(&r1a.digest));
    assert!(mergeset.contains(&r1b.digest));
    assert!(mergeset.contains(&genesis.digest));
}

#[test]
fn test_mergeset_diamond_structure() {
    let dir = tempdir().unwrap();
    let storage = Storage::new(dir.path().join("test.db")).unwrap();
    let dag = DagStore::new(storage).unwrap();

    let (key, _) = kvnc_crypto::generate_keypair();

    // Genesis
    let genesis = make_block(0, 0, Vec::new(), "genesis", &key);
    dag.put_block(&genesis).unwrap();

    // Round 1
    let r1a = make_block(0, 1, vec![block_ref(&genesis)], "r1a", &key);
    dag.put_block(&r1a).unwrap();

    // Round 2 - two blocks both pointing to r1a
    let r2a = make_block(0, 2, vec![block_ref(&r1a)], "r2a", &key);
    let r2b = make_block(1, 2, vec![block_ref(&r1a)], "r2b", &key);
    dag.put_block(&r2a).unwrap();
    dag.put_block(&r2b).unwrap();

    // Round 3 - leader pointing to both r2a and r2b (diamond)
    let leader = make_block(0, 3, vec![block_ref(&r2a), block_ref(&r2b)], "leader", &key);
    dag.put_block(&leader).unwrap();

    let mergeset = dag.mergeset(&leader.digest).unwrap();
    // Should have: leader, r2a, r2b, r1a, genesis = 5 (r1a only once despite diamond)
    assert!(mergeset.contains(&leader.digest));
    assert!(mergeset.contains(&r2a.digest));
    assert!(mergeset.contains(&r2b.digest));
    assert!(mergeset.contains(&r1a.digest));
    assert!(mergeset.contains(&genesis.digest));
    assert_eq!(mergeset.len(), 5);
}
