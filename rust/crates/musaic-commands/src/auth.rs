//! The pairing handshake's wire types and proofs (research R3).
//!
//! The secret never crosses the wire. Each side proves it holds it with an
//! HMAC-SHA256 over the pairing, the instance and both nonces, under a label
//! that differs per direction so one proof cannot stand in for the other.

use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

/// The keyring service of the agent secrets.
pub const AGENT_SERVICE: &str = "musaic-agent";

pub const CLIENT_LABEL: &str = "musaic-agent/v1/client";
pub const SERVER_LABEL: &str = "musaic-agent/v1/server";

/// How long a nonce lives, and how long a token does (seconds).
pub const NONCE_TTL_SECS: u64 = 60;
pub const TOKEN_TTL_SECS: u64 = 15 * 60;
/// The CLI renews this long before expiry.
pub const RENEW_BEFORE_SECS: u64 = 60;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HelloResponse {
    pub instance_id: String,
    pub server_nonce: String,
    pub profiles: Vec<String>,
    pub contract_version: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExchangeRequest {
    pub pairing_id: String,
    pub instance_id: String,
    pub server_nonce: String,
    pub client_nonce: String,
    pub client_proof: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExchangeResponse {
    pub token: String,
    pub expires_at: String,
    pub scopes: Vec<String>,
    pub server_proof: String,
}

/// The first 16 hex characters of SHA-1 of the resolved config dir: the same
/// derivation as an album id, so one directory has one id.
pub fn config_dir_id(resolved_config_dir: &str) -> String {
    musaic_core::album_id(resolved_config_dir)
}

/// The keyring account of a pairing's secret.
pub fn agent_account(config_dir_id: &str, pairing_id: &str) -> String {
    format!("{config_dir_id}:{pairing_id}")
}

/// The keyring account that lists the active pairing ids of a config dir
/// (ids only, comma separated, no secret). A keyring cannot be listed, so the
/// app keeps this entry in step and the CLI reads it to find its candidates.
pub fn index_account(config_dir_id: &str) -> String {
    format!("{config_dir_id}:index")
}

pub fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

pub fn hex_decode(s: &str) -> Option<Vec<u8>> {
    // Hex digits only: `from_str_radix` alone would also take a `+` sign
    // ("+f" is 15), so two spellings would decode to the same bytes.
    if !s.len().is_multiple_of(2) || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

/// `n` random bytes as lowercase hex.
pub fn random_hex(n: usize) -> String {
    let mut buf = vec![0u8; n];
    getrandom::fill(&mut buf).expect("the OS random source");
    hex_encode(&buf)
}

fn mac(secret: &[u8], label: &str, fields: [&str; 4]) -> Hmac<Sha256> {
    let mut m =
        <Hmac<Sha256> as KeyInit>::new_from_slice(secret).expect("HMAC takes any key length");
    for part in std::iter::once(label).chain(fields) {
        m.update(&(part.len() as u32).to_be_bytes());
        m.update(part.as_bytes());
    }
    m
}

fn proof(
    secret: &[u8],
    label: &str,
    pairing_id: &str,
    instance_id: &str,
    server_nonce: &str,
    client_nonce: &str,
) -> String {
    let m = mac(
        secret,
        label,
        [pairing_id, instance_id, server_nonce, client_nonce],
    );
    hex_encode(&m.finalize().into_bytes())
}

pub fn client_proof(
    secret: &[u8],
    pairing_id: &str,
    instance_id: &str,
    server_nonce: &str,
    client_nonce: &str,
) -> String {
    proof(
        secret,
        CLIENT_LABEL,
        pairing_id,
        instance_id,
        server_nonce,
        client_nonce,
    )
}

pub fn server_proof(
    secret: &[u8],
    pairing_id: &str,
    instance_id: &str,
    server_nonce: &str,
    client_nonce: &str,
) -> String {
    proof(
        secret,
        SERVER_LABEL,
        pairing_id,
        instance_id,
        server_nonce,
        client_nonce,
    )
}

/// Constant-time check of a presented proof against the one computed for
/// `label`.
fn verify(secret: &[u8], label: &str, presented_hex: &str, fields: [&str; 4]) -> bool {
    let Some(presented) = hex_decode(presented_hex) else {
        return false;
    };
    mac(secret, label, fields).verify_slice(&presented).is_ok()
}

pub fn verify_client_proof(
    secret: &[u8],
    presented_hex: &str,
    pairing_id: &str,
    instance_id: &str,
    server_nonce: &str,
    client_nonce: &str,
) -> bool {
    verify(
        secret,
        CLIENT_LABEL,
        presented_hex,
        [pairing_id, instance_id, server_nonce, client_nonce],
    )
}

pub fn verify_server_proof(
    secret: &[u8],
    presented_hex: &str,
    pairing_id: &str,
    instance_id: &str,
    server_nonce: &str,
    client_nonce: &str,
) -> bool {
    verify(
        secret,
        SERVER_LABEL,
        presented_hex,
        [pairing_id, instance_id, server_nonce, client_nonce],
    )
}

/// Constant-time equality of two byte strings (tokens, fingerprints).
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    let mut diff = (a.len() != b.len()) as u8;
    for i in 0..a.len().max(b.len()) {
        diff |= a.get(i).copied().unwrap_or(0) ^ b.get(i).copied().unwrap_or(0);
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &[u8] = b"0123456789abcdef0123456789abcdef";

    fn args() -> [&'static str; 4] {
        ["p_1", "i_1", "aa", "bb"]
    }

    #[test]
    fn known_vector() {
        // Fixed so a change of the construction (labels, framing) is noticed.
        let p = client_proof(SECRET, "p_1", "i_1", "aa", "bb");
        assert_eq!(p.len(), 64);
        assert_eq!(p, client_proof(SECRET, "p_1", "i_1", "aa", "bb"));
        assert_eq!(
            p, "2cad226cc0c0ea890f7dc967e2fda1a0edf0eb17b0e6fc20e984fadc9248dc33",
            "update the vector only for a deliberate protocol change"
        );
    }

    #[test]
    fn each_side_verifies_its_own_label_only() {
        let [p, i, s, c] = args();
        let client = client_proof(SECRET, p, i, s, c);
        let server = server_proof(SECRET, p, i, s, c);
        assert_ne!(client, server);
        assert!(verify_client_proof(SECRET, &client, p, i, s, c));
        assert!(verify_server_proof(SECRET, &server, p, i, s, c));
        assert!(!verify_client_proof(SECRET, &server, p, i, s, c));
        assert!(!verify_server_proof(SECRET, &client, p, i, s, c));
    }

    #[test]
    fn any_changed_field_or_secret_fails() {
        let proof = client_proof(SECRET, "p_1", "i_1", "aa", "bb");
        for changed in [
            ("p_2", "i_1", "aa", "bb"),
            ("p_1", "i_2", "aa", "bb"),
            ("p_1", "i_1", "ab", "bb"),
            ("p_1", "i_1", "aa", "bc"),
        ] {
            assert!(!verify_client_proof(
                SECRET, &proof, changed.0, changed.1, changed.2, changed.3
            ));
        }
        assert!(!verify_client_proof(
            b"another secret",
            &proof,
            "p_1",
            "i_1",
            "aa",
            "bb"
        ));
        assert!(!verify_client_proof(SECRET, "zz", "p_1", "i_1", "aa", "bb"));
        assert!(!verify_client_proof(SECRET, "", "p_1", "i_1", "aa", "bb"));
    }

    #[test]
    fn length_prefixing_stops_field_shifting() {
        let a = client_proof(SECRET, "p_1", "i_1", "aa", "bb");
        let b = client_proof(SECRET, "p_1i", "_1", "aa", "bb");
        assert_ne!(a, b);
    }

    #[test]
    fn config_dir_id_is_the_album_id_derivation() {
        assert_eq!(config_dir_id("/a/b"), musaic_core::album_id("/a/b"));
        assert_eq!(
            agent_account("0123456789abcdef", "p_x"),
            "0123456789abcdef:p_x"
        );
    }

    #[test]
    fn hex_round_trips_and_ct_eq_works() {
        assert_eq!(
            hex_decode(&hex_encode(&[0, 15, 255])),
            Some(vec![0, 15, 255])
        );
        assert_eq!(hex_decode("abc"), None);
        assert_eq!(hex_decode("zz"), None);
        assert_eq!(hex_decode("+f"), None, "a sign is not a hex digit");
        assert!(ct_eq(b"abc", b"abc"));
        assert!(!ct_eq(b"abc", b"abd"));
        assert!(!ct_eq(b"abc", b"abcd"));
        assert_eq!(random_hex(8).len(), 16);
    }
}
