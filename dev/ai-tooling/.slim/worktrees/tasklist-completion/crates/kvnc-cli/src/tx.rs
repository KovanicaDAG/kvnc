//! Offline transaction construction and encoding.
//!
//! A KVNC [`Transaction`] carries a cached `hash` and an Ed25519 `signature`
//! over that hash. The CLI derives the hash deterministically from the signable
//! fields (sender, nonce, kind, fee) and signs it offline — private keys never
//! leave the machine.

use anyhow::{Context, Result};

use kvnc_types::transaction::{Transaction, TransactionKind};
use kvnc_types::{Address, Hash, SigningKey};

/// Deterministic hash over the signable fields of a transaction.
///
/// Delegates to [`Transaction::signing_hash_from`] — the single canonical
/// transaction signing payload in `kvnc-types` — so CLI-built transactions
/// verify against [`Transaction::verify_signature`] and pass DAG/mempool
/// validation. Do **not** reintroduce a separate bincode encoding here.
pub fn signable_hash(
    sender: &Address,
    nonce: u64,
    kind: &TransactionKind,
    fee: u64,
) -> Result<Hash> {
    Ok(Transaction::signing_hash_from(sender, nonce, kind, fee))
}

/// Build and sign a transaction, returning it ready for submission.
pub fn build_signed(
    sender: Address,
    nonce: u64,
    kind: TransactionKind,
    fee: u64,
    signing_key: &SigningKey,
) -> Result<Transaction> {
    let hash = signable_hash(&sender, nonce, &kind, fee)?;
    let signature = kvnc_crypto::sign(signing_key, hash.as_ref());
    Ok(Transaction {
        sender,
        nonce,
        kind,
        fee,
        signature,
        hash,
    })
}

/// Bincode-encode a signed transaction as a hex string for
/// `kvnc_sendRawTransaction`.
pub fn encode_raw(transaction: &Transaction) -> Result<String> {
    let bytes = bincode::serialize(transaction).context("encoding signed transaction")?;
    Ok(hex::encode(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use kvnc_types::transaction::TransactionKind;

    #[test]
    fn signed_transaction_roundtrips_through_bincode() {
        let (signing_key, public_key) = kvnc_crypto::generate_keypair();
        let sender = Address::from_public_key(&public_key);
        let to = Address([9u8; 32]);
        let kind = TransactionKind::Transfer { to, amount: 42 };
        let tx = build_signed(sender, 7, kind, 1, &signing_key).unwrap();

        let raw = encode_raw(&tx).unwrap();
        let decoded: Transaction = bincode::deserialize(&hex::decode(raw).unwrap()).unwrap();

        assert_eq!(decoded.sender, tx.sender);
        assert_eq!(decoded.nonce, 7);
        assert_eq!(decoded.fee, 1);
        assert_eq!(decoded.hash, tx.hash);
        assert_eq!(decoded.signature, tx.signature);
        kvnc_crypto::verify(&public_key, tx.hash.as_ref(), &tx.signature).unwrap();
    }

    #[test]
    fn hash_is_deterministic_and_nonce_sensitive() {
        let sender = Address([1u8; 32]);
        let to = Address([2u8; 32]);
        let kind = TransactionKind::Transfer { to, amount: 5 };
        let a = signable_hash(&sender, 1, &kind, 0).unwrap();
        let b = signable_hash(&sender, 1, &kind, 0).unwrap();
        let c = signable_hash(&sender, 2, &kind, 0).unwrap();
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn cli_signable_hash_matches_canonical_transaction_hash() {
        let sender = Address([3u8; 32]);
        let kind = TransactionKind::Stake { amount: 1_000 };
        let from_cli = signable_hash(&sender, 9, &kind, 4).unwrap();
        let from_types = Transaction::signing_hash_from(&sender, 9, &kind, 4);
        assert_eq!(from_cli, from_types);
    }

    #[test]
    fn cli_signed_transaction_passes_verify_signature() {
        let (signing_key, public_key) = kvnc_crypto::generate_keypair();
        let sender = Address::from_public_key(&public_key);
        // Identity address model: the sender is the raw public key.
        assert_eq!(sender.0, public_key.0);

        let to = Address([9u8; 32]);
        let kind = TransactionKind::Transfer { to, amount: 42 };
        let tx = build_signed(sender, 7, kind, 1, &signing_key).unwrap();

        // Cached hash equals the canonical signing hash, and the signature over
        // it verifies against the sender-derived public key.
        assert_eq!(tx.hash, tx.signing_hash());
        assert!(tx.verify_signature(), "CLI-signed tx must verify");

        // Round-trips through the raw encoding used for submission.
        let raw = encode_raw(&tx).unwrap();
        let decoded: Transaction = bincode::deserialize(&hex::decode(raw).unwrap()).unwrap();
        assert!(decoded.verify_signature(), "decoded tx must verify");
    }

    #[test]
    fn tampered_transaction_fails_verify_signature() {
        let (signing_key, public_key) = kvnc_crypto::generate_keypair();
        let sender = Address::from_public_key(&public_key);
        let to = Address([9u8; 32]);
        let kind = TransactionKind::Transfer { to, amount: 42 };
        let tx = build_signed(sender, 7, kind, 1, &signing_key).unwrap();
        assert!(tx.verify_signature());

        // Tamper with a signed field: the recomputed signing hash changes, so
        // the original signature no longer authenticates the transaction.
        let mut tampered = tx.clone();
        tampered.fee = 2;
        assert!(!tampered.verify_signature(), "tampered fee must not verify");

        let mut tampered = tx.clone();
        tampered.nonce = 8;
        assert!(!tampered.verify_signature(), "tampered nonce must not verify");

        let mut tampered = tx.clone();
        tampered.kind = TransactionKind::Transfer {
            to,
            amount: 43,
        };
        assert!(!tampered.verify_signature(), "tampered amount must not verify");
    }
}
