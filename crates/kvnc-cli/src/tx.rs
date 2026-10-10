//! Offline transaction construction and encoding.
//!
//! A Kovanica [`Transaction`] carries a cached `hash` and an Ed25519 `signature`
//! over that hash. The CLI derives the hash deterministically from the signable
//! fields (sender, nonce, kind, fee) and signs it offline — private keys never
//! leave the machine.

use anyhow::{Context, Result};

use kvnc_types::transaction::{Transaction, TransactionKind};
use kvnc_types::{Address, Hash, SigningKey};

/// Deterministic hash over the signable fields of a transaction.
pub fn signable_hash(
    sender: &Address,
    nonce: u64,
    kind: &TransactionKind,
    fee: u64,
) -> Result<Hash> {
    let encoded = bincode::serialize(&(sender, nonce, kind, fee))
        .context("encoding transaction signable payload")?;
    Ok(Hash::new(&encoded))
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
}
