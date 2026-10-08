//! Fuzzing harness for consensus process_block
#![no_main]
use libfuzzer_sys::fuzz_target;
use kvnc_consensus::{ConsensusConfig, ConsensusEngine};
use kvnc_types::{block::StatementBlock, hash::Hash, AuthorityIndex, Round};
use kvnc_dag::DagStore;
use kvnc_storage::Storage;
use tempfile::tempdir;

fuzz_target!(|data: &[u8]| {
    // Create a temporary engine
    let dir = tempdir().unwrap();
    let storage = Storage::new(dir.path().join("dag.redb")).unwrap();
    let dag_store = DagStore::new(storage).unwrap();
    
    let config = ConsensusConfig::default();
    let committee = kvnc_consensus::CommitteeInfo::try_new(1, vec![]).unwrap();
    
    let mut engine = ConsensusEngine::new(
        config,
        committee,
        kvnc_consensus::engine::NodeDagStore { inner: dag_store },
        kvnc_consensus::engine::NodeBlockManager { inner: kvnc_dag::BlockManager::new(kvnc_dag::DagStore::new(kvnc_storage::Storage::new(tempdir().unwrap().path().join("dag2.redb")).unwrap()).unwrap()) },
        kvnc_crypto::generate_keypair().0,
        None,
    );
    
    // Try to deserialize a block from fuzz data
    if let Ok(block) = bincode::deserialize::<StatementBlock>(data) {
        let _ = engine.process_block(&block);
    }
});