//! Content signatures are independent of archives, channels and network origins.
use ring::signature::{ED25519, Ed25519KeyPair, KeyPair, UnparsedPublicKey};
use serde::{Deserialize, Serialize};

pub const SIGNATURE_FILE: &str = "signature.json";
pub const DOMAIN: &[u8] = b"wonderland-wplug-signature-v1\0";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PackageSignature {
    pub schema_version: u32,
    pub algorithm: String,
    pub public_key: String,
    pub signature: String,
}

fn message(checksums: &[u8]) -> Vec<u8> {
    [DOMAIN, checksums].concat()
}

pub fn verify(checksums: &[u8], envelope: &[u8]) -> Result<PackageSignature, String> {
    if envelope.len() > 4096 {
        return Err("INVALID_PACKAGE_SIGNATURE: signature is too large".into());
    }
    let value: PackageSignature = serde_json::from_slice(envelope)
        .map_err(|_| "INVALID_PACKAGE_SIGNATURE: malformed signature container")?;
    if value.schema_version != 1 || value.algorithm != "ed25519" {
        return Err("UNSUPPORTED_PACKAGE_SIGNATURE: unknown signature format".into());
    }
    let lower_hex = |s: &str, n| {
        s.len() == n
            && s.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    };
    if !lower_hex(&value.public_key, 64) || !lower_hex(&value.signature, 128) {
        return Err("INVALID_PACKAGE_SIGNATURE: invalid public key or signature".into());
    }
    let key = hex::decode(&value.public_key).map_err(|_| "INVALID_PACKAGE_SIGNATURE")?;
    let signature = hex::decode(&value.signature).map_err(|_| "INVALID_PACKAGE_SIGNATURE")?;
    UnparsedPublicKey::new(&ED25519, key)
        .verify(&message(checksums), &signature)
        .map_err(|_| "INVALID_PACKAGE_SIGNATURE: content signature does not match")?;
    Ok(value)
}

pub fn sign(checksums: &[u8], seed: &[u8]) -> Result<PackageSignature, String> {
    let key = Ed25519KeyPair::from_seed_unchecked(seed)
        .map_err(|_| "INVALID_SIGNING_KEY: expected an Ed25519 seed")?;
    Ok(PackageSignature {
        schema_version: 1,
        algorithm: "ed25519".into(),
        public_key: hex::encode(key.public_key().as_ref()),
        signature: hex::encode(key.sign(&message(checksums)).as_ref()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn node_vector_uses_identical_domain_and_raw_bytes() {
        let mut vector: serde_json::Value =
            serde_json::from_str(include_str!("../tests/vector.json")).unwrap();
        let checksums = vector
            .as_object_mut()
            .unwrap()
            .remove("checksums")
            .unwrap()
            .as_str()
            .unwrap()
            .as_bytes()
            .to_vec();
        let expected: PackageSignature = serde_json::from_value(vector).unwrap();
        assert_eq!(sign(&checksums, &[42; 32]).unwrap(), expected);
        assert_eq!(
            verify(&checksums, &serde_json::to_vec(&expected).unwrap()).unwrap(),
            expected
        );
    }
    #[test]
    fn content_bytes_and_format_are_authenticated() {
        let checksums = b"{\"algorithm\":\"sha256\",\"files\":[]}\n";
        let envelope = sign(checksums, &[42; 32]).unwrap();
        let encoded = serde_json::to_vec(&envelope).unwrap();
        assert_eq!(verify(checksums, &encoded).unwrap(), envelope);
        assert!(verify(b"{}", &encoded).is_err());
        let mut unsupported = envelope;
        unsupported.schema_version = 2;
        assert!(
            verify(checksums, &serde_json::to_vec(&unsupported).unwrap())
                .unwrap_err()
                .starts_with("UNSUPPORTED_PACKAGE_SIGNATURE")
        );
    }
}
