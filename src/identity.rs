//! Signing identity: the `ph login` keypair, used to sign every action.
//!
//! # Why the CLI signs
//!
//! A Switchboard signs every UNSIGNED action it applies with its own Renown
//! identity and stamps the `user` from its own session — so on a shared
//! server every write from every client is attributed to whoever ran
//! `ph login` on the server (or to nobody). An action that arrives already
//! signed is stored untouched. Signing here therefore turns "the server
//! wrote this" into "this CLI, acting for this address, wrote this" — and
//! `app.name` lets an agent name itself (`powerhouse-knowledge`) so a vault
//! can tell agent writes from a human's in Connect.
//!
//! # What is signed, byte for byte (mirrors `@renown/sdk` `RenownCryptoSigner`)
//!
//! ```text
//! actionHash = base64( sha256( scope + type + JSON.stringify(input) ) )
//! params     = [ unixSeconds, did:key, actionHash, prevStateHash ]
//! message    = "\x19Signed Operation:\n" + len(params.join("")) + params.join("")
//! signature  = ECDSA P-256 / SHA-256 over message, raw r||s, "0x" + hex
//! tuple      = params + [signature]      — transported joined by ", "
//! ```
//!
//! The leading `0x19` byte is the EIP-191 domain prefix and is invisible in a
//! terminal; leave it out and every signature fails verification. `input` is
//! serialized with the key order the caller wrote (`serde_json` with
//! `preserve_order`), which is also the order sent on the wire, so anyone
//! recomputing the hash from the stored input gets the same bytes.
//!
//! `prevStateHash` is left empty, as the Switchboard's own signer does when an
//! action carries no `prevOpHash`; the wire records it rather than enforcing it.
//!
//! # did:key
//!
//! `did:key:z` + base58btc( 0x80 0x24 ‖ compressed P-256 point ). The Renown
//! credential in `.renown.json` names the did:key it authorised as
//! `credentialSubject.id`; loading refuses a keypair that does not derive to
//! that did — the credential would then be vouching for a different key.

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use p256::ecdsa::{Signature, SigningKey, signature::Signer};
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// File names `ph login` writes into its `.ph` directory.
pub const KEYPAIR_FILE: &str = ".keypair.json";
pub const RENOWN_FILE: &str = ".renown.json";

/// EIP-191-style prefix every signed operation message starts with.
pub const SIGNED_OPERATION_PREFIX: &str = "\u{19}Signed Operation:\n";

/// Separator the wire uses to join a signature tuple into one string.
pub const SIGNATURE_SEPARATOR: &str = ", ";

/// Multicodec prefix for a P-256 public key (varint 0x1200).
const P256_MULTICODEC: [u8; 2] = [0x80, 0x24];

#[derive(Deserialize)]
struct KeyPairFile {
    #[serde(rename = "keyPair")]
    key_pair: KeyPairJwk,
}

#[derive(Deserialize)]
struct KeyPairJwk {
    #[serde(rename = "publicKey")]
    public_key: Jwk,
    #[serde(rename = "privateKey")]
    private_key: Jwk,
}

#[derive(Deserialize)]
struct Jwk {
    kty: String,
    crv: Option<String>,
    x: String,
    y: String,
    d: Option<String>,
}

#[derive(Deserialize)]
struct RenownFile {
    user: RenownUser,
}

#[derive(Deserialize, Clone)]
pub struct RenownUser {
    pub address: String,
    #[serde(rename = "networkId")]
    pub network_id: String,
    #[serde(rename = "chainId")]
    pub chain_id: i64,
    pub credential: Option<RenownCredential>,
}

#[derive(Deserialize, Clone)]
pub struct RenownCredential {
    #[serde(rename = "credentialSubject")]
    pub credential_subject: Option<CredentialSubject>,
    #[serde(rename = "expirationDate")]
    pub expiration_date: Option<String>,
}

#[derive(Deserialize, Clone)]
pub struct CredentialSubject {
    pub id: Option<String>,
}

/// A loaded, validated signing identity.
pub struct Identity {
    signing_key: SigningKey,
    /// `did:key:z…` of the public key — the `app.key` on every signature.
    pub did: String,
    pub user: RenownUser,
    /// Directory the identity was loaded from.
    pub ph_dir: PathBuf,
}

impl Identity {
    /// Load `.keypair.json` + `.renown.json` from `ph_dir` and check that the
    /// keypair is the one the Renown credential authorised.
    pub fn load(ph_dir: &Path) -> Result<Identity> {
        let keypair_path = ph_dir.join(KEYPAIR_FILE);
        let renown_path = ph_dir.join(RENOWN_FILE);
        let keypair_raw = std::fs::read_to_string(&keypair_path)
            .with_context(|| format!("cannot read {}", keypair_path.display()))?;
        let renown_raw = std::fs::read_to_string(&renown_path)
            .with_context(|| format!("cannot read {}", renown_path.display()))?;
        let keypair: KeyPairFile =
            serde_json::from_str(&keypair_raw).context("malformed .keypair.json")?;
        let renown: RenownFile =
            serde_json::from_str(&renown_raw).context("malformed .renown.json")?;
        Identity::from_parts(&keypair.key_pair, renown.user, ph_dir.to_path_buf())
    }

    fn from_parts(jwk: &KeyPairJwk, user: RenownUser, ph_dir: PathBuf) -> Result<Identity> {
        let private = &jwk.private_key;
        if private.kty != "EC" || private.crv.as_deref() != Some("P-256") {
            bail!(
                "keypair is not an EC P-256 key (kty={}, crv={:?})",
                private.kty,
                private.crv
            );
        }
        let d = private
            .d
            .as_deref()
            .ok_or_else(|| anyhow!("private JWK has no `d` (secret scalar)"))?;
        let d_bytes = base64url_decode(d).context("invalid base64url in JWK `d`")?;
        let signing_key = SigningKey::from_slice(&d_bytes)
            .map_err(|e| anyhow!("invalid P-256 private key: {e}"))?;

        // Derive the did from the PRIVATE key's public point and cross-check
        // against the public JWK — a keypair file whose halves disagree is
        // corrupt, not merely surprising.
        let did = did_key_from_public(signing_key.verifying_key())?;
        let public_did = did_key_from_jwk_public(&jwk.public_key)?;
        if did != public_did {
            bail!(
                "keypair file is inconsistent: private key derives {did}, public key is {public_did}"
            );
        }

        let authorised = user
            .credential
            .as_ref()
            .and_then(|c| c.credential_subject.as_ref())
            .and_then(|s| s.id.as_deref());
        if let Some(subject) = authorised.filter(|subject| *subject != did) {
            bail!(
                "the Renown credential authorises {subject}, but this keypair is {did}. \
                 Run `ph login` in that directory again so the credential and key match."
            );
        }

        Ok(Identity {
            signing_key,
            did,
            user,
            ph_dir,
        })
    }

    /// The credential's expiry, if the file carries one (ISO-8601).
    pub fn credential_expires(&self) -> Option<&str> {
        self.user
            .credential
            .as_ref()
            .and_then(|c| c.expiration_date.as_deref())
    }

    /// True when the credential has an expiry and it is in the past.
    pub fn credential_expired(&self) -> bool {
        match self.credential_expires() {
            Some(iso) => iso < crate::cli::docs::iso_now().as_str(),
            None => false,
        }
    }

    /// Sign one action in place: sets `context.signer` with the user, the
    /// app (`app_name`, this did) and a single signature tuple. An action
    /// that already carries signatures is left alone — the reactor treats
    /// those as authoritative and so must we.
    pub fn sign_action(&self, action: &mut Value, app_name: &str) -> Result<()> {
        let obj = action
            .as_object_mut()
            .ok_or_else(|| anyhow!("action is not a JSON object"))?;
        let already_signed = obj
            .get("context")
            .and_then(|c| c.get("signer"))
            .and_then(|s| s.get("signatures"))
            .and_then(|s| s.as_array())
            .is_some_and(|sigs| !sigs.is_empty());
        if already_signed {
            return Ok(());
        }
        let scope = obj
            .get("scope")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("action has no `scope`"))?
            .to_string();
        let action_type = obj
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("action has no `type`"))?
            .to_string();
        let input = obj
            .get("input")
            .cloned()
            .unwrap_or(Value::Object(Default::default()));

        let tuple = self.sign(&scope, &action_type, &input, "", unix_seconds_now());
        let signer = serde_json::json!({
            "user": {
                "address": self.user.address,
                "networkId": self.user.network_id,
                "chainId": self.user.chain_id,
            },
            "app": { "name": app_name, "key": self.did },
            "signatures": [tuple.join(SIGNATURE_SEPARATOR)],
        });
        let context = obj
            .entry("context")
            .or_insert_with(|| Value::Object(Default::default()));
        match context {
            Value::Object(map) => {
                map.insert("signer".to_string(), signer);
            }
            other => {
                *other = serde_json::json!({ "signer": signer });
            }
        }
        Ok(())
    }

    /// Produce the 5-element signature tuple for an action's parts.
    pub fn sign(
        &self,
        scope: &str,
        action_type: &str,
        input: &Value,
        prev_state_hash: &str,
        unix_seconds: u64,
    ) -> [String; 5] {
        let hash = action_hash(scope, action_type, input);
        let params = [
            unix_seconds.to_string(),
            self.did.clone(),
            hash,
            prev_state_hash.to_string(),
        ];
        let message = signature_message(&params);
        let signature: Signature = self.signing_key.sign(&message);
        let hex = format!("0x{}", hex_encode(&signature.to_bytes()));
        let [a, b, c, d] = params;
        [a, b, c, d, hex]
    }
}

/// `base64(sha256(scope + type + JSON.stringify(input)))`.
pub fn action_hash(scope: &str, action_type: &str, input: &Value) -> String {
    let payload = format!("{scope}{action_type}{}", compact_json(input));
    base64::engine::general_purpose::STANDARD.encode(Sha256::digest(payload.as_bytes()))
}

/// The bytes that are signed: prefix + length + concatenated params.
pub fn signature_message(params: &[String; 4]) -> Vec<u8> {
    let joined = params.concat();
    // `.length` in JS counts UTF-16 units; every param here is ASCII
    // (digits, base58, base64, hex), so chars == bytes == UTF-16 units.
    format!(
        "{SIGNED_OPERATION_PREFIX}{}{joined}",
        joined.chars().count()
    )
    .into_bytes()
}

/// `JSON.stringify` parity: compact, insertion-ordered (preserve_order).
fn compact_json(v: &Value) -> String {
    serde_json::to_string(v).unwrap_or_default()
}

fn did_key_from_public(key: &p256::ecdsa::VerifyingKey) -> Result<String> {
    let point = key.to_encoded_point(true);
    let mut bytes = P256_MULTICODEC.to_vec();
    bytes.extend_from_slice(point.as_bytes());
    Ok(format!("did:key:z{}", bs58::encode(bytes).into_string()))
}

fn did_key_from_jwk_public(jwk: &Jwk) -> Result<String> {
    let x = base64url_decode(&jwk.x).context("invalid base64url in JWK `x`")?;
    let y = base64url_decode(&jwk.y).context("invalid base64url in JWK `y`")?;
    if x.len() != 32 || y.len() != 32 {
        bail!("public JWK coordinates must be 32 bytes each");
    }
    let parity = if y[31] & 1 == 1 { 0x03 } else { 0x02 };
    let mut bytes = P256_MULTICODEC.to_vec();
    bytes.push(parity);
    bytes.extend_from_slice(&x);
    Ok(format!("did:key:z{}", bs58::encode(bytes).into_string()))
}

fn base64url_decode(s: &str) -> Result<Vec<u8>> {
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(s.trim_end_matches('='))
        .map_err(|e| anyhow!("{e}"))
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unix_seconds_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Resolve where the identity should be read from when the user gives no
/// `--ph-dir`: the project's `.ph` if it holds a login, else `~/.ph`.
pub fn default_ph_dir() -> Option<PathBuf> {
    let candidates = [
        std::env::current_dir().ok().map(|d| d.join(".ph")),
        dirs::home_dir().map(|h| h.join(".ph")),
    ];
    candidates
        .into_iter()
        .flatten()
        .find(|d| d.join(RENOWN_FILE).is_file() && d.join(KEYPAIR_FILE).is_file())
}

#[cfg(test)]
mod tests {
    use super::*;
    use p256::ecdsa::signature::Verifier;

    fn test_identity() -> Identity {
        // A throwaway key generated for the test — never a real login.
        let signing_key = SigningKey::from_slice(&[7u8; 32]).unwrap();
        let did = did_key_from_public(signing_key.verifying_key()).unwrap();
        Identity {
            signing_key,
            did,
            user: RenownUser {
                address: "0x00000000000000000000000000000000000000aa".into(),
                network_id: "eip155".into(),
                chain_id: 1,
                credential: None,
            },
            ph_dir: PathBuf::from("/nonexistent"),
        }
    }

    #[test]
    fn action_hash_matches_js_sha256_of_scope_type_input() {
        // node: crypto.createHash("sha256").update("globalSET_TITLE{}").digest("base64")
        assert_eq!(
            action_hash("global", "SET_TITLE", &serde_json::json!({})),
            "WzeFpsIT3tXHGq4vMdGkPGH3+lVzmdc1aeScx6N/AQI="
        );
    }

    #[test]
    fn action_hash_preserves_the_callers_key_order() {
        // {"b":1,"a":2} must NOT be re-sorted — JSON.stringify keeps insertion order.
        let input: Value = serde_json::from_str(r#"{"b":1,"a":2}"#).unwrap();
        assert_eq!(compact_json(&input), r#"{"b":1,"a":2}"#);
    }

    #[test]
    fn message_has_eip191_prefix_and_length() {
        let m = signature_message(&["1".into(), "did".into(), "h".into(), "".into()]);
        assert_eq!(m, b"\x19Signed Operation:\n51didh".to_vec());
        assert_eq!(m[0], 0x19);
    }

    #[test]
    fn did_key_is_p256_multicodec_base58btc() {
        let id = test_identity();
        assert!(
            id.did.starts_with("did:key:zDn"),
            "P-256 did:keys start with zDn…: {}",
            id.did
        );
        // Round-trip: decode and check the multicodec + compressed point length.
        let decoded = bs58::decode(&id.did["did:key:z".len()..])
            .into_vec()
            .unwrap();
        assert_eq!(&decoded[..2], &[0x80, 0x24]);
        assert_eq!(decoded.len(), 2 + 33);
        assert!(decoded[2] == 0x02 || decoded[2] == 0x03);
    }

    #[test]
    fn signature_verifies_against_the_did_key() {
        let id = test_identity();
        let input = serde_json::json!({ "title": "t", "updatedAt": "2026-09-02T00:00:00.000Z" });
        let tuple = id.sign("global", "SET_TITLE", &input, "", 1788349213);
        assert_eq!(tuple[0], "1788349213");
        assert_eq!(tuple[1], id.did);
        assert_eq!(tuple[2], action_hash("global", "SET_TITLE", &input));
        assert_eq!(tuple[3], "");
        assert!(tuple[4].starts_with("0x") && tuple[4].len() == 2 + 128);

        let message = signature_message(&[
            tuple[0].clone(),
            tuple[1].clone(),
            tuple[2].clone(),
            tuple[3].clone(),
        ]);
        let sig_bytes: Vec<u8> = (0..64)
            .map(|i| u8::from_str_radix(&tuple[4][2 + 2 * i..4 + 2 * i], 16).unwrap())
            .collect();
        let sig = Signature::from_slice(&sig_bytes).unwrap();
        assert!(
            id.signing_key
                .verifying_key()
                .verify(&message, &sig)
                .is_ok()
        );
        // A different message must not verify.
        assert!(
            id.signing_key
                .verifying_key()
                .verify(b"other", &sig)
                .is_err()
        );
    }

    #[test]
    fn sign_action_sets_context_signer_and_respects_existing_signatures() {
        let id = test_identity();
        let mut action = serde_json::json!({
            "id": "a1", "type": "SET_TITLE", "scope": "global", "timestampUtcMs": "2026-09-02T00:00:00.000Z",
            "input": { "title": "t", "updatedAt": "2026-09-02T00:00:00.000Z" }
        });
        id.sign_action(&mut action, "switchboard-cli").unwrap();
        let signer = &action["context"]["signer"];
        assert_eq!(signer["app"]["name"], "switchboard-cli");
        assert_eq!(signer["app"]["key"], id.did);
        assert_eq!(signer["user"]["address"], id.user.address);
        let sigs = signer["signatures"].as_array().unwrap();
        assert_eq!(sigs.len(), 1);
        let parts: Vec<&str> = sigs[0]
            .as_str()
            .unwrap()
            .split(SIGNATURE_SEPARATOR)
            .collect();
        assert_eq!(parts.len(), 5);
        assert_eq!(parts[1], id.did);

        // Already signed → untouched.
        let before = action.clone();
        id.sign_action(&mut action, "other-app").unwrap();
        assert_eq!(action, before);
    }

    #[test]
    fn loading_refuses_a_key_the_credential_did_not_authorise() {
        let signing_key = SigningKey::from_slice(&[9u8; 32]).unwrap();
        let point = signing_key.verifying_key().to_encoded_point(false);
        let b64 = |b: &[u8]| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b);
        let jwk = KeyPairJwk {
            public_key: Jwk {
                kty: "EC".into(),
                crv: Some("P-256".into()),
                x: b64(point.x().unwrap()),
                y: b64(point.y().unwrap()),
                d: None,
            },
            private_key: Jwk {
                kty: "EC".into(),
                crv: Some("P-256".into()),
                x: b64(point.x().unwrap()),
                y: b64(point.y().unwrap()),
                d: Some(b64(&signing_key.to_bytes())),
            },
        };
        let user = RenownUser {
            address: "0xaa".into(),
            network_id: "eip155".into(),
            chain_id: 1,
            credential: Some(RenownCredential {
                credential_subject: Some(CredentialSubject {
                    id: Some("did:key:zDnaSomeoneElse".into()),
                }),
                expiration_date: None,
            }),
        };
        let err = Identity::from_parts(&jwk, user, PathBuf::from("/x"))
            .err()
            .unwrap();
        assert!(
            err.to_string()
                .contains("authorises did:key:zDnaSomeoneElse"),
            "{err}"
        );
    }
}
