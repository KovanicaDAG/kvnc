//! Shared helpers for the kvnc-consensus test suite.
//!
//! Provides block/committee builders plus an in-memory [`MockDag`] that
//! implements [`DagStoreTrait`] with the same `get_ancestors` semantics as the
//! real `kvnc_dag::DagStore` (BFS over parents, `min_round` filtering, self
//! excluded, duplicate pushes on diamond paths).

#![allow(dead_code)]

use kvnc_consensus::engine::{BlockManagerTrait, DagStoreTrait};
use kvnc_consensus::{AuthorityInfo, CommitteeInfo, LeaderInfo, LeaderStatus};
use kvnc_dag::{BlockManagerError, DagStoreError};
use kvnc_types::block::{BlockReference, StatementBlock};
use kvnc_types::hash::Hash;
use kvnc_types::{Address, AuthorityIndex, PublicKey, Round, Signature, SigningKey, Stake};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// Committee of `n` authorities with equal stake of 1.
pub fn committee(n: u16) -> CommitteeInfo {
    committee_with_stakes(&vec![1u64; n as usize])
}

/// Committee with explicit per-authority stakes (index = position).
pub fn committee_with_stakes(stakes: &[Stake]) -> CommitteeInfo {
    let authorities = stakes
        .iter()
        .enumerate()
        .map(|(i, &stake)| AuthorityInfo {
            index: i as AuthorityIndex,
            stake,
            public_key: PublicKey([i as u8; 32]),
            address: Address([i as u8; 32]),
            network_address: format!("/ip4/127.0.0.1/tcp/90{:02}", i),
        })
        .collect();
    CommitteeInfo::new(0, authorities)
}

/// Build a block. `tag` must be unique within a test; it determines the digest.
pub fn make_block(
    author: AuthorityIndex,
    round: Round,
    parents: Vec<BlockReference>,
    tag: &str,
) -> StatementBlock {
    StatementBlock {
        author,
        round,
        parents,
        transactions: Vec::new(),
        statements: tag.as_bytes().to_vec(),
        signature: Signature([0u8; 64]),
        digest: Hash::new(format!("kvnc-test/{tag}").as_bytes()),
    }
}

/// Reference to a block (author + round + digest).
pub fn block_ref(block: &StatementBlock) -> BlockReference {
    BlockReference {
        author: block.author,
        round: block.round,
        digest: block.digest,
    }
}

/// Canonical genesis block used by most tests.
pub fn genesis() -> StatementBlock {
    make_block(0, 0, Vec::new(), "genesis")
}

/// Build a [`LeaderInfo`] with the given status and votes.
pub fn leader_info(
    round: Round,
    author: AuthorityIndex,
    block_hash: Option<Hash>,
    status: LeaderStatus,
    votes: &[(AuthorityIndex, Hash)],
) -> LeaderInfo {
    LeaderInfo {
        round,
        author,
        block_hash,
        status,
        votes: votes.iter().copied().collect(),
    }
}

/// Total stake of the voters present in `votes` (unknown voters contribute 0).
pub fn voter_stake(committee: &CommitteeInfo, votes: &HashMap<AuthorityIndex, Hash>) -> Stake {
    votes
        .keys()
        .filter_map(|idx| committee.stake_of(*idx))
        .sum()
}

/// In-memory DAG store implementing [`DagStoreTrait`].
///
/// `get_ancestors` mirrors `kvnc_dag::DagStore::get_ancestors`: BFS from
/// `hash`, self excluded, a parent is reported (and traversed) only when its
/// round `>= min_round`, missing blocks surface as `NotFound`.
#[derive(Default)]
pub struct MockDag {
    blocks: parking_lot::RwLock<HashMap<Hash, StatementBlock>>,
}

impl MockDag {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_blocks(blocks: impl IntoIterator<Item = StatementBlock>) -> Self {
        let dag = Self::new();
        for block in blocks {
            dag.put(block);
        }
        dag
    }

    pub fn put(&self, block: StatementBlock) {
        self.blocks.write().insert(block.digest, block);
    }

    pub fn contains(&self, digest: &Hash) -> bool {
        self.blocks.read().contains_key(digest)
    }
}

impl DagStoreTrait for MockDag {
    fn get_block(&self, hash: &Hash) -> Result<StatementBlock, DagStoreError> {
        self.blocks
            .read()
            .get(hash)
            .cloned()
            .ok_or_else(|| DagStoreError::NotFound(format!("block {hash}")))
    }

    fn get_ancestors(&self, hash: &Hash, min_round: Round) -> Result<Vec<Hash>, DagStoreError> {
        let blocks = self.blocks.read();
        let mut ancestors = Vec::new();
        let mut visited: HashSet<Hash> = HashSet::new();
        let mut queue: VecDeque<Hash> = VecDeque::new();
        queue.push_back(*hash);

        while let Some(current) = queue.pop_front() {
            if visited.contains(&current) {
                continue;
            }
            visited.insert(current);

            let current_block = blocks
                .get(&current)
                .ok_or_else(|| DagStoreError::NotFound(format!("block {current}")))?;
            for parent in &current_block.parents {
                if parent.digest == current {
                    continue;
                }
                let parent_block = blocks
                    .get(&parent.digest)
                    .ok_or_else(|| DagStoreError::NotFound(format!("block {}", parent.digest)))?;
                if parent_block.round >= min_round {
                    ancestors.push(parent.digest);
                    queue.push_back(parent.digest);
                }
            }
        }

        Ok(ancestors)
    }

    fn get_block_by_author_round(
        &self,
        author: AuthorityIndex,
        round: Round,
    ) -> Result<Option<StatementBlock>, DagStoreError> {
        Ok(self
            .blocks
            .read()
            .values()
            .find(|b| b.author == author && b.round == round)
            .cloned())
    }

    fn get_blocks_by_round(&self, round: Round) -> Result<Vec<StatementBlock>, DagStoreError> {
        let mut out: Vec<StatementBlock> = self
            .blocks
            .read()
            .values()
            .filter(|b| b.round == round)
            .cloned()
            .collect();
        out.sort_by_key(|b| (b.author, b.digest.0));
        Ok(out)
    }

    fn has_block(&self, hash: &Hash) -> Result<bool, DagStoreError> {
        Ok(self.blocks.read().contains_key(hash))
    }

    fn put_block(&self, block: &StatementBlock) -> Result<(), DagStoreError> {
        self.blocks.write().insert(block.digest, block.clone());
        Ok(())
    }

    fn find_parents(
        &self,
        _round: Round,
        _max_parents: usize,
    ) -> Result<Vec<BlockReference>, DagStoreError> {
        Ok(Vec::new())
    }

    fn commit_leader(&self, _leader_hash: &Hash) -> Result<u64, DagStoreError> {
        Ok(0)
    }

    fn mark_round_decided(&self, _round: Round, _leader_hash: &Hash) -> Result<(), DagStoreError> {
        Ok(())
    }
}

/// In-memory block manager for `kvnc_consensus::ConsensusEngine` tests.
pub struct MockBlockManager {
    authority: AuthorityIndex,
    fail_process: AtomicBool,
    process_calls: AtomicUsize,
    propose_calls: AtomicUsize,
    signing_key_set: AtomicBool,
}

impl MockBlockManager {
    pub fn new(authority: AuthorityIndex) -> Self {
        Self {
            authority,
            fail_process: AtomicBool::new(false),
            process_calls: AtomicUsize::new(0),
            propose_calls: AtomicUsize::new(0),
            signing_key_set: AtomicBool::new(false),
        }
    }

    /// Make `process_block` fail (used for error-propagation tests).
    pub fn set_fail_process(&self, fail: bool) {
        self.fail_process.store(fail, Ordering::SeqCst);
    }

    pub fn process_calls(&self) -> usize {
        self.process_calls.load(Ordering::SeqCst)
    }

    pub fn propose_calls(&self) -> usize {
        self.propose_calls.load(Ordering::SeqCst)
    }

    pub fn signing_key_set(&self) -> bool {
        self.signing_key_set.load(Ordering::SeqCst)
    }
}

impl BlockManagerTrait for MockBlockManager {
    fn propose_block(&self, round: Round) -> Result<StatementBlock, BlockManagerError> {
        let block = make_block(
            self.authority,
            round,
            Vec::new(),
            &format!("proposed/{}/{}", self.authority, round),
        );
        self.propose_calls.fetch_add(1, Ordering::SeqCst);
        Ok(block)
    }

    fn process_block(&self, _block: &StatementBlock) -> Result<(), BlockManagerError> {
        self.process_calls.fetch_add(1, Ordering::SeqCst);
        if self.fail_process.load(Ordering::SeqCst) {
            return Err(BlockManagerError::InvalidBlock(
                "mock block manager rejects".into(),
            ));
        }
        Ok(())
    }

    fn our_authority(&self) -> AuthorityIndex {
        self.authority
    }

    fn set_signing_key(&self, _key: SigningKey) {
        self.signing_key_set.store(true, Ordering::SeqCst);
    }
}

/// Drive `try_commit` to quiescence, returning every committed sub-DAG in order.
///
/// Panics instead of looping forever: a committer that keeps returning
/// `Some(...)` for the same or an earlier round would hang (and that panic is
/// exactly the signal we want from a broken implementation).
pub fn drive_try_commit(
    committer: &kvnc_consensus::UniversalCommitter,
    dag: &MockDag,
) -> Vec<kvnc_consensus::CommittedSubDag> {
    let mut out = Vec::new();
    for _ in 0..10_000 {
        match committer.try_commit(dag) {
            Some(subdag) => out.push(subdag),
            None => return out,
        }
    }
    panic!("try_commit did not quiesce within 10000 iterations (repeated commits?)");
}

/// Build a `ConsensusEngine` wired to fresh mock DAG store + block manager.
pub fn make_engine(
    authority: AuthorityIndex,
    committee: kvnc_consensus::CommitteeInfo,
    config: kvnc_consensus::ConsensusConfig,
) -> (
    kvnc_consensus::ConsensusEngine<MockDag, MockBlockManager>,
    std::sync::Arc<MockDag>,
    std::sync::Arc<parking_lot::RwLock<MockBlockManager>>,
) {
    let dag = std::sync::Arc::new(MockDag::new());
    let manager = std::sync::Arc::new(parking_lot::RwLock::new(MockBlockManager::new(authority)));
    let engine =
        kvnc_consensus::ConsensusEngine::new(config, committee, dag.clone(), manager.clone());
    (engine, dag, manager)
}
