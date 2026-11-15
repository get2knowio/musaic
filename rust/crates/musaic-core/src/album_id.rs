use sha1::{Digest, Sha1};

/// Album id: the first 16 hex characters of SHA-1 over the UTF-8 bytes of the
/// album directory's resolved absolute path, exactly as the OS returned it.
/// No Unicode normalisation: the same directory stored NFC on one
/// filesystem and NFD on another has two different ids. That is reproduced on
/// purpose: changing it would orphan existing databases.
pub fn album_id(resolved_path: &str) -> String {
    let digest = Sha1::digest(resolved_path.as_bytes());
    let mut out = String::with_capacity(16);
    for byte in &digest[..8] {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}
