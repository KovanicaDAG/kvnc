//! Offline transaction construction and encoding.
//!
//! A KVNC [`Transaction`] carries a cached `hash` and an Ed25519 `signature`
//! over that hash. The CLI uses `Transaction::signing_hash` (signature format v1,
//! see `docs/SIGNATURE_FORMAT.md`; commits to the `chain_id`) and signs it offline — private keys never
//! leave the machine.

use anyhow::{anyhow, bail, Context, Result};

use kvnc_types::crypto::Signature;
use kvnc_types::signing::{self, SigningContext};
use kvnc_types::transaction::{Transaction, TransactionKind};
use kvnc_types::{Address, Hash, SigningKey};

/// Build the network signing context for transaction signing from the
/// `--chain-id` / `KVNC_CHAIN_ID` setting.
///
/// There is no default: signing without knowing the network would either fail
/// verification or, worse, produce a signature for the wrong network. The
/// `chain_id` must be in the registry of `docs/SIGNATURE_FORMAT.md`
/// (1 mainnet, 2 testnet, 3 devnet, 1337 local).
pub fn signing_context(chain_id: Option<u64>) -> Result<SigningContext> {
    let chain_id = chain_id.ok_or_else(|| {
        anyhow!("--chain-id (or KVNC_CHAIN_ID) is required to sign transactions: 1 mainnet, 2 testnet, 3 devnet, 1337 local")
    })?;
    if !signing::chain_id::is_registered(chain_id) {
        bail!("unknown chain id {chain_id}: expected 1 (mainnet), 2 (testnet), 3 (devnet) or 1337 (local)");
    }
    Ok(SigningContext::new(chain_id))
}

/// Build and sign a transaction (signature format v1), returning it ready for
/// submission. The hash is [`Transaction::signing_hash`], the same one the
/// node recomputes and verifies.
pub fn build_signed(
    ctx: &SigningContext,
    sender: Address,
    nonce: u64,
    kind: TransactionKind,
    fee: u64,
    signing_key: &SigningKey,
) -> Result<Transaction> {
    let mut transaction = Transaction {
        sender,
        nonce,
        kind,
        fee,
        signature: Signature([0u8; 64]),
        hash: Hash::zero(),
    };
    let hash = transaction.signing_hash(ctx);
    transaction.signature = kvnc_crypto::sign(signing_key, hash.as_ref());
    transaction.hash = hash;
    Ok(transaction)
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

    const CTX: SigningContext = SigningContext {
        chain_id: 2,
        epoch: 0,
    };

    #[test]
    fn signed_transaction_roundtrips_through_bincode() {
        let (signing_key, public_key) = kvnc_crypto::generate_keypair();
        let sender = Address::from_public_key(&public_key);
        let to = Address([9u8; 32]);
        let kind = TransactionKind::Transfer { to, amount: 42 };
        let tx = build_signed(&CTX, sender, 7, kind, 1, &signing_key).unwrap();

        let raw = encode_raw(&tx).unwrap();
        let decoded: Transaction = bincode::deserialize(&hex::decode(raw).unwrap()).unwrap();

        assert_eq!(decoded.sender, tx.sender);
        assert_eq!(decoded.nonce, 7);
        assert_eq!(decoded.fee, 1);
        assert_eq!(decoded.hash, tx.hash);
        assert_eq!(decoded.signature, tx.signature);
        kvnc_crypto::verify(&public_key, tx.hash.as_ref(), &tx.signature).unwrap();
    }

    /// The CLI must produce exactly what the node verifies: hash equals
    /// `signing_hash(ctx)` and `verify_signature(ctx)` holds, but only for the
    /// signing network.
    #[test]
    fn cli_transaction_verifies_like_the_node_does() {
        let (signing_key, public_key) = kvnc_crypto::generate_keypair();
        let sender = Address::from_public_key(&public_key);
        let kind = TransactionKind::Transfer {
            to: Address([2u8; 32]),
            amount: 5,
        };
        let tx = build_signed(&CTX, sender, 1, kind, 1, &signing_key).unwrap();
        assert_eq!(tx.hash, tx.signing_hash(&CTX));
        assert!(tx.verify_signature(&CTX));
        assert!(!tx.verify_signature(&SigningContext::new(1)));
    }

    #[test]
    fn hash_is_deterministic_nonce_and_chain_sensitive() {
        let (signing_key, public_key) = kvnc_crypto::generate_keypair();
        let sender = Address::from_public_key(&public_key);
        let kind = TransactionKind::Transfer {
            to: Address([2u8; 32]),
            amount: 5,
        };
        let a = build_signed(&CTX, sender, 1, kind.clone(), 1, &signing_key).unwrap();
        let b = build_signed(&CTX, sender, 1, kind.clone(), 1, &signing_key).unwrap();
        let c = build_signed(&CTX, sender, 2, kind.clone(), 1, &signing_key).unwrap();
        let d = build_signed(&SigningContext::new(3), sender, 1, kind, 1, &signing_key).unwrap();
        assert_eq!(a.hash, b.hash);
        assert_ne!(a.hash, c.hash);
        assert_ne!(a.hash, d.hash);
    }

    #[test]
    fn chain_id_is_required_and_must_be_registered() {
        assert!(signing_context(None).is_err());
        assert!(signing_context(Some(0)).is_err());
        assert!(signing_context(Some(4)).is_err());
        for id in [1, 2, 3, 1337] {
            assert_eq!(signing_context(Some(id)).unwrap().chain_id, id);
        }
    }
}
