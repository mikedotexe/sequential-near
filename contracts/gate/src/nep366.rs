//! NEP-366 / NEP-461 wire format + on-chain signature verification.
//!
//! Byte-for-byte compatible with `near-primitives::action::delegate`
//! (DelegateAction, SignedDelegateAction), so delegates built by
//! near-api-js, the Pagoda relayer, or any other NEP-366-compliant
//! client deserialize and verify here.
//!
//! v0.1 scope: only the `FunctionCall` inner action variant is
//! supported. Other variants (including valid non-delegate ones like
//! Transfer) fail deserialization with a clear error. Variant 8
//! (Delegate) fails with "nested DelegateAction forbidden", matching
//! `NonDelegateAction`'s enforcement in near-primitives.
//!
//! Signature scheme (per NEP-461): the hash to sign is
//!   sha256( borsh(u32 discriminant) || borsh(DelegateAction) )
//! where `discriminant = (1u32 << 30) + 366 = 1073742190`. Only
//! ed25519 keys are accepted in v0.1.

use borsh::{BorshDeserialize, BorshSerialize};
use near_sdk::{env, AccountId};
use std::io::{Error, ErrorKind, Read, Write};

/// The on-chain discriminant base per NEP-461's signed-message scheme
/// (see `near-primitives::signable_message`).
pub const MIN_ON_CHAIN_DISCRIMINANT: u32 = 1 << 30;

/// Discriminant for NEP-366 DelegateAction signed messages.
/// Bytes (little-endian): `0x6E 0x01 0x00 0x40` (= 1073742190).
pub const NEP_366_DELEGATE_DISCRIMINANT: u32 = MIN_ON_CHAIN_DISCRIMINANT + 366;

const ACTION_TAG_FUNCTION_CALL: u8 = 2;
const ACTION_TAG_DELEGATE: u8 = 8;

/// Ed25519 public key. Serializes as `0u8 || [u8; 32]`, matching
/// `near_crypto::PublicKey::ED25519`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ed25519PublicKey(pub [u8; 32]);

impl BorshSerialize for Ed25519PublicKey {
    fn serialize<W: Write>(&self, writer: &mut W) -> std::io::Result<()> {
        0u8.serialize(writer)?;
        writer.write_all(&self.0)
    }
}

impl BorshDeserialize for Ed25519PublicKey {
    fn deserialize_reader<R: Read>(reader: &mut R) -> std::io::Result<Self> {
        let tag = u8::deserialize_reader(reader)?;
        if tag != 0 {
            return Err(Error::new(
                ErrorKind::InvalidData,
                format!("expected ed25519 public key (tag 0), got tag {}", tag),
            ));
        }
        let mut bytes = [0u8; 32];
        reader.read_exact(&mut bytes)?;
        Ok(Self(bytes))
    }
}

/// Ed25519 signature. Serializes as `0u8 || [u8; 64]`, matching
/// `near_crypto::Signature::ED25519`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ed25519Signature(pub [u8; 64]);

impl BorshSerialize for Ed25519Signature {
    fn serialize<W: Write>(&self, writer: &mut W) -> std::io::Result<()> {
        0u8.serialize(writer)?;
        writer.write_all(&self.0)
    }
}

impl BorshDeserialize for Ed25519Signature {
    fn deserialize_reader<R: Read>(reader: &mut R) -> std::io::Result<Self> {
        let tag = u8::deserialize_reader(reader)?;
        if tag != 0 {
            return Err(Error::new(
                ErrorKind::InvalidData,
                format!("expected ed25519 signature (tag 0), got tag {}", tag),
            ));
        }
        let mut bytes = [0u8; 64];
        reader.read_exact(&mut bytes)?;
        Ok(Self(bytes))
    }
}

/// FunctionCall action. Fields and order match
/// `near-primitives::action::FunctionCallAction`.
#[derive(BorshSerialize, BorshDeserialize, Debug, Clone, PartialEq, Eq)]
pub struct FunctionCallAction {
    pub method_name: String,
    pub args: Vec<u8>,
    pub gas: u64,
    pub deposit: u128,
}

/// The inner action of a NEP-366 delegate.
///
/// v0.1 accepts only the `FunctionCall` variant. All other NEP-366
/// non-delegate variants (CreateAccount, DeployContract, Transfer,
/// Stake, AddKey, DeleteKey, DeleteAccount) fail to deserialize with
/// a clear error. Variant 8 (Delegate) fails with a specific nested-
/// delegate error. This keeps the MVP tight without breaking wire
/// compatibility — a fuller version can extend the enum.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NonDelegateAction {
    FunctionCall(FunctionCallAction),
}

impl BorshSerialize for NonDelegateAction {
    fn serialize<W: Write>(&self, writer: &mut W) -> std::io::Result<()> {
        match self {
            Self::FunctionCall(fc) => {
                ACTION_TAG_FUNCTION_CALL.serialize(writer)?;
                fc.serialize(writer)
            }
        }
    }
}

impl BorshDeserialize for NonDelegateAction {
    fn deserialize_reader<R: Read>(reader: &mut R) -> std::io::Result<Self> {
        let tag = u8::deserialize_reader(reader)?;
        match tag {
            ACTION_TAG_FUNCTION_CALL => Ok(Self::FunctionCall(
                FunctionCallAction::deserialize_reader(reader)?,
            )),
            ACTION_TAG_DELEGATE => Err(Error::new(
                ErrorKind::InvalidInput,
                "nested DelegateAction forbidden (NEP-366)",
            )),
            other => Err(Error::new(
                ErrorKind::InvalidInput,
                format!(
                    "action variant {} not supported in v0.1 (only FunctionCall)",
                    other
                ),
            )),
        }
    }
}

/// NEP-366 DelegateAction. Field order and types match
/// `near-primitives::action::delegate::DelegateAction` byte-for-byte.
#[derive(BorshSerialize, BorshDeserialize, Debug, Clone, PartialEq, Eq)]
pub struct DelegateAction {
    pub sender_id: AccountId,
    pub receiver_id: AccountId,
    pub actions: Vec<NonDelegateAction>,
    pub nonce: u64,
    pub max_block_height: u64,
    pub public_key: Ed25519PublicKey,
}

/// NEP-366 SignedDelegateAction. Layout matches near-primitives.
#[derive(BorshSerialize, BorshDeserialize, Debug, Clone, PartialEq, Eq)]
pub struct SignedDelegateAction {
    pub delegate_action: DelegateAction,
    pub signature: Ed25519Signature,
}

impl DelegateAction {
    /// Canonical NEP-461 signed-message hash:
    /// `sha256( borsh(discriminant: u32) || borsh(self) )`.
    pub fn signed_message_hash(&self) -> [u8; 32] {
        let mut buf: Vec<u8> = Vec::with_capacity(128);
        NEP_366_DELEGATE_DISCRIMINANT
            .serialize(&mut buf)
            .expect("borsh u32 serialize is infallible into Vec");
        self.serialize(&mut buf)
            .expect("borsh DelegateAction serialize is infallible into Vec");
        let digest = env::sha256(&buf);
        let mut out = [0u8; 32];
        out.copy_from_slice(&digest);
        out
    }

    /// Exactly one action, and it must be a FunctionCall. Returns the
    /// inner FunctionCall or an error with a precise reason.
    pub fn require_single_function_call(&self) -> Result<&FunctionCallAction, &'static str> {
        if self.actions.len() != 1 {
            return Err("v0.1 gate requires exactly one action per delegate");
        }
        match &self.actions[0] {
            NonDelegateAction::FunctionCall(fc) => Ok(fc),
        }
    }
}

impl SignedDelegateAction {
    /// Verify the ed25519 signature using the NEAR host function
    /// `env::ed25519_verify`. Returns true iff the signature is
    /// valid for the delegate_action under the embedded public_key.
    pub fn verify(&self) -> bool {
        let hash = self.delegate_action.signed_message_hash();
        env::ed25519_verify(
            &self.signature.0,
            &hash,
            &self.delegate_action.public_key.0,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};
    use near_sdk::test_utils::VMContextBuilder;
    use near_sdk::testing_env;
    use rand::rngs::OsRng;
    use rand::RngCore;

    fn ctx() {
        testing_env!(VMContextBuilder::new().build());
    }

    fn make_keypair() -> SigningKey {
        let mut secret = [0u8; 32];
        OsRng.fill_bytes(&mut secret);
        SigningKey::from_bytes(&secret)
    }

    fn sample_fc_action() -> FunctionCallAction {
        FunctionCallAction {
            method_name: "set".to_string(),
            args: br#"{"value":"42"}"#.to_vec(),
            gas: 50_000_000_000_000,
            deposit: 0,
        }
    }

    fn sample_delegate(pk: [u8; 32]) -> DelegateAction {
        DelegateAction {
            sender_id: "alice.testnet".parse().unwrap(),
            receiver_id: "register.testnet".parse().unwrap(),
            actions: vec![NonDelegateAction::FunctionCall(sample_fc_action())],
            nonce: 42,
            max_block_height: 1_000_000_000,
            public_key: Ed25519PublicKey(pk),
        }
    }

    #[test]
    fn discriminant_value() {
        assert_eq!(NEP_366_DELEGATE_DISCRIMINANT, 1_073_742_190);
    }

    #[test]
    fn discriminant_borsh_bytes_little_endian() {
        let mut bytes = Vec::new();
        NEP_366_DELEGATE_DISCRIMINANT.serialize(&mut bytes).unwrap();
        assert_eq!(bytes, vec![0x6E, 0x01, 0x00, 0x40]);
    }

    #[test]
    fn delegate_round_trip() {
        let kp = make_keypair();
        let pk = kp.verifying_key().to_bytes();
        let da = sample_delegate(pk);
        let bytes = borsh::to_vec(&da).unwrap();
        let parsed = DelegateAction::try_from_slice(&bytes).unwrap();
        assert_eq!(da, parsed);
    }

    #[test]
    fn signed_delegate_round_trip() {
        ctx();
        let kp = make_keypair();
        let pk = kp.verifying_key().to_bytes();
        let da = sample_delegate(pk);
        let hash = da.signed_message_hash();
        let sig = kp.sign(&hash).to_bytes();
        let sda = SignedDelegateAction {
            delegate_action: da.clone(),
            signature: Ed25519Signature(sig),
        };
        let bytes = borsh::to_vec(&sda).unwrap();
        let parsed = SignedDelegateAction::try_from_slice(&bytes).unwrap();
        assert_eq!(sda, parsed);
    }

    #[test]
    fn verify_accepts_valid_signature() {
        ctx();
        let kp = make_keypair();
        let pk = kp.verifying_key().to_bytes();
        let da = sample_delegate(pk);
        let hash = da.signed_message_hash();
        let sig = kp.sign(&hash).to_bytes();
        let sda = SignedDelegateAction {
            delegate_action: da,
            signature: Ed25519Signature(sig),
        };
        assert!(sda.verify());
    }

    #[test]
    fn verify_rejects_tampered_signature() {
        ctx();
        let kp = make_keypair();
        let pk = kp.verifying_key().to_bytes();
        let da = sample_delegate(pk);
        let hash = da.signed_message_hash();
        let mut sig = kp.sign(&hash).to_bytes();
        sig[0] ^= 0x01;
        let sda = SignedDelegateAction {
            delegate_action: da,
            signature: Ed25519Signature(sig),
        };
        assert!(!sda.verify());
    }

    #[test]
    fn verify_rejects_tampered_payload() {
        ctx();
        let kp = make_keypair();
        let pk = kp.verifying_key().to_bytes();
        let da = sample_delegate(pk);
        let hash = da.signed_message_hash();
        let sig = kp.sign(&hash).to_bytes();
        let mut tampered = da.clone();
        tampered.nonce += 1;
        let sda = SignedDelegateAction {
            delegate_action: tampered,
            signature: Ed25519Signature(sig),
        };
        assert!(!sda.verify());
    }

    #[test]
    fn verify_rejects_wrong_public_key() {
        ctx();
        let kp_real = make_keypair();
        let kp_other = make_keypair();
        let da = sample_delegate(kp_other.verifying_key().to_bytes());
        let hash = da.signed_message_hash();
        let sig = kp_real.sign(&hash).to_bytes();
        let sda = SignedDelegateAction {
            delegate_action: da,
            signature: Ed25519Signature(sig),
        };
        assert!(!sda.verify());
    }

    #[test]
    fn nested_delegate_variant_rejected() {
        let mut bytes = Vec::new();
        (1u32).serialize(&mut bytes).unwrap();
        ACTION_TAG_DELEGATE.serialize(&mut bytes).unwrap();
        let err = Vec::<NonDelegateAction>::try_from_slice(&bytes).unwrap_err();
        assert!(
            err.to_string().contains("nested DelegateAction forbidden"),
            "unexpected error: {}",
            err
        );
    }

    #[test]
    fn unsupported_variants_rejected() {
        for tag in [0u8, 1, 3, 4, 5, 6, 7] {
            let mut bytes = Vec::new();
            (1u32).serialize(&mut bytes).unwrap();
            tag.serialize(&mut bytes).unwrap();
            let err = Vec::<NonDelegateAction>::try_from_slice(&bytes).unwrap_err();
            let s = err.to_string();
            assert!(
                s.contains("not supported in v0.1"),
                "variant {} gave unexpected error: {}",
                tag,
                s
            );
        }
    }

    #[test]
    fn require_single_function_call_happy_path() {
        let kp = make_keypair();
        let da = sample_delegate(kp.verifying_key().to_bytes());
        let fc = da.require_single_function_call().unwrap();
        assert_eq!(fc.method_name, "set");
    }

    #[test]
    fn require_single_function_call_rejects_empty() {
        let kp = make_keypair();
        let mut da = sample_delegate(kp.verifying_key().to_bytes());
        da.actions.clear();
        assert!(da.require_single_function_call().is_err());
    }

    #[test]
    fn require_single_function_call_rejects_multi() {
        let kp = make_keypair();
        let mut da = sample_delegate(kp.verifying_key().to_bytes());
        da.actions
            .push(NonDelegateAction::FunctionCall(sample_fc_action()));
        assert!(da.require_single_function_call().is_err());
    }

    #[test]
    fn ed25519_pk_tag_mismatch_rejected() {
        let bytes = [1u8; 33];
        let err = Ed25519PublicKey::try_from_slice(&bytes).unwrap_err();
        assert!(err.to_string().contains("ed25519"));
    }

    #[test]
    fn ed25519_sig_tag_mismatch_rejected() {
        let bytes = [1u8; 65];
        let err = Ed25519Signature::try_from_slice(&bytes).unwrap_err();
        assert!(err.to_string().contains("ed25519"));
    }
}
