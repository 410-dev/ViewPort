//! Encryption for locked bundles.
//!
//! A 256-bit key is derived with Argon2id from the password, optionally combined with
//! a key file (its SHA-256 is mixed into the password input, VeraCrypt style). Data is
//! sealed with XChaCha20-Poly1305: random 192-bit nonces make reuse a non-issue, and
//! the associated data binds each blob to what it is (database, settings, thumbnail),
//! so one cannot be swapped for another.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{Key, XChaCha20Poly1305, XNonce};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

const MAGIC: &[u8; 6] = b"VPENC1";
const NONCE_LEN: usize = 24;
const CHECK_PLAINTEXT: &[u8] = b"ViewPort bundle key check";

#[derive(Debug, thiserror::Error)]
pub enum CryptoError {
    #[error("wrong password or key file")]
    WrongKey,
    #[error("data is not encrypted or is damaged")]
    Corrupt,
    #[error("key derivation failed: {0}")]
    Kdf(String),
    #[error("no secure random source: {0}")]
    Random(String),
}

pub type Result<T> = std::result::Result<T, CryptoError>;

/// Public parameters stored in `info.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KdfParams {
    /// Always `argon2id`.
    pub algorithm: String,
    /// Base64 salt.
    pub salt: String,
    pub memory_kib: u32,
    pub iterations: u32,
    pub parallelism: u32,
    pub requires_keyfile: bool,
    /// Base64 of a known value sealed with the key, to tell a wrong password apart
    /// from damaged data.
    pub check: String,
}

/// A derived key; wiped from memory when dropped.
#[derive(Clone)]
pub struct BundleKey(Zeroizing<[u8; 32]>);

impl std::fmt::Debug for BundleKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BundleKey(..)")
    }
}

fn random<const N: usize>() -> Result<[u8; N]> {
    let mut buf = [0u8; N];
    getrandom::fill(&mut buf).map_err(|e| CryptoError::Random(e.to_string()))?;
    Ok(buf)
}

/// A random hex string of `bytes` bytes (e.g. an unguessable URL token).
pub fn random_hex(bytes: usize) -> Result<String> {
    let mut buf = vec![0u8; bytes];
    getrandom::fill(&mut buf).map_err(|e| CryptoError::Random(e.to_string()))?;
    Ok(buf.iter().map(|b| format!("{b:02x}")).collect())
}

fn derive(password: &str, keyfile: Option<&[u8]>, salt: &[u8], m: u32, t: u32, p: u32) -> Result<BundleKey> {
    let mut input = Zeroizing::new(password.as_bytes().to_vec());
    if let Some(kf) = keyfile {
        input.extend_from_slice(&Sha256::digest(kf));
    }
    let params = argon2::Params::new(m, t, p, Some(32)).map_err(|e| CryptoError::Kdf(e.to_string()))?;
    let argon = argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);
    let mut out = Zeroizing::new([0u8; 32]);
    argon.hash_password_into(&input, salt, out.as_mut()).map_err(|e| CryptoError::Kdf(e.to_string()))?;
    Ok(BundleKey(out))
}

/// Default cost: 64 MiB, 3 passes — a fraction of a second on a desktop.
pub const DEFAULT_MEMORY_KIB: u32 = 64 * 1024;
pub const DEFAULT_ITERATIONS: u32 = 3;

/// Creates a new key and the parameters to store for it.
pub fn new_key(password: &str, keyfile: Option<&[u8]>, memory_kib: u32, iterations: u32) -> Result<(BundleKey, KdfParams)> {
    let salt = random::<16>()?;
    let key = derive(password, keyfile, &salt, memory_kib, iterations, 1)?;
    let check = seal(&key, CHECK_PLAINTEXT, b"check")?;
    let params = KdfParams {
        algorithm: "argon2id".into(),
        salt: B64.encode(salt),
        memory_kib,
        iterations,
        parallelism: 1,
        requires_keyfile: keyfile.is_some(),
        check: B64.encode(check),
    };
    Ok((key, params))
}

/// Re-derives the key for an existing bundle, checking it against `params.check`.
pub fn unlock(password: &str, keyfile: Option<&[u8]>, params: &KdfParams) -> Result<BundleKey> {
    if params.algorithm != "argon2id" {
        return Err(CryptoError::Kdf(format!("unsupported algorithm {}", params.algorithm)));
    }
    let salt = B64.decode(&params.salt).map_err(|_| CryptoError::Corrupt)?;
    let key = derive(password, keyfile, &salt, params.memory_kib, params.iterations, params.parallelism)?;
    let check = B64.decode(&params.check).map_err(|_| CryptoError::Corrupt)?;
    match open(&key, &check, b"check") {
        Ok(v) if v == CHECK_PLAINTEXT => Ok(key),
        _ => Err(CryptoError::WrongKey),
    }
}

/// Encrypts `plaintext`; `label` must be given again to decrypt.
pub fn seal(key: &BundleKey, plaintext: &[u8], label: &[u8]) -> Result<Vec<u8>> {
    let cipher = XChaCha20Poly1305::new(Key::from_slice(key.0.as_ref()));
    let nonce = random::<NONCE_LEN>()?;
    let ct = cipher
        .encrypt(XNonce::from_slice(&nonce), Payload { msg: plaintext, aad: label })
        .map_err(|_| CryptoError::Corrupt)?;
    let mut out = Vec::with_capacity(MAGIC.len() + NONCE_LEN + ct.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ct);
    Ok(out)
}

pub fn open(key: &BundleKey, sealed: &[u8], label: &[u8]) -> Result<Vec<u8>> {
    if sealed.len() < MAGIC.len() + NONCE_LEN || &sealed[..MAGIC.len()] != MAGIC {
        return Err(CryptoError::Corrupt);
    }
    let (nonce, ct) = sealed[MAGIC.len()..].split_at(NONCE_LEN);
    let cipher = XChaCha20Poly1305::new(Key::from_slice(key.0.as_ref()));
    cipher.decrypt(XNonce::from_slice(nonce), Payload { msg: ct, aad: label }).map_err(|_| CryptoError::Corrupt)
}

impl BundleKey {
    /// A fresh random key (e.g. a shard's master key, which a password only wraps).
    pub fn random() -> Result<Self> {
        Ok(Self(Zeroizing::new(random::<32>()?)))
    }

    /// Hex form, for keeping the key inside an encrypted bundle.
    pub fn to_hex(&self) -> Zeroizing<String> {
        Zeroizing::new(self.0.iter().map(|b| format!("{b:02x}")).collect())
    }

    pub fn from_hex(hex: &str) -> Result<Self> {
        let hex = hex.as_bytes();
        if hex.len() != 64 {
            return Err(CryptoError::Corrupt);
        }
        let mut out = Zeroizing::new([0u8; 32]);
        for (i, pair) in hex.chunks(2).enumerate() {
            let s = std::str::from_utf8(pair).map_err(|_| CryptoError::Corrupt)?;
            out[i] = u8::from_str_radix(s, 16).map_err(|_| CryptoError::Corrupt)?;
        }
        Ok(Self(out))
    }
}

/// Encrypts `key` with `kek` (a password-derived key) for storing next to the data.
pub fn wrap_key(kek: &BundleKey, key: &BundleKey) -> Result<String> {
    Ok(B64.encode(seal(kek, key.0.as_ref(), b"wrapped-key")?))
}

pub fn unwrap_key(kek: &BundleKey, wrapped: &str) -> Result<BundleKey> {
    let sealed = B64.decode(wrapped).map_err(|_| CryptoError::Corrupt)?;
    let bytes = Zeroizing::new(open(kek, &sealed, b"wrapped-key").map_err(|_| CryptoError::WrongKey)?);
    let arr: [u8; 32] = bytes.as_slice().try_into().map_err(|_| CryptoError::Corrupt)?;
    Ok(BundleKey(Zeroizing::new(arr)))
}

/// Size of the authentication tag added to every sealed segment.
pub const TAG_LEN: usize = 16;

/// XChaCha20-Poly1305 with caller-chosen nonces, for data that is read piecewise (shard
/// segments). The caller guarantees nonce uniqueness, e.g. a random per-file prefix plus
/// the segment number, and binds each piece to its place with the associated data.
pub struct SegmentCipher(XChaCha20Poly1305);

impl SegmentCipher {
    pub fn new(key: &BundleKey) -> Self {
        Self(XChaCha20Poly1305::new(Key::from_slice(key.0.as_ref())))
    }

    /// Returns ciphertext followed by the tag (`plaintext.len() + TAG_LEN` bytes).
    pub fn seal(&self, nonce: &[u8; 24], plaintext: &[u8], aad: &[u8]) -> Result<Vec<u8>> {
        self.0.encrypt(XNonce::from_slice(nonce), Payload { msg: plaintext, aad }).map_err(|_| CryptoError::Corrupt)
    }

    pub fn open(&self, nonce: &[u8; 24], sealed: &[u8], aad: &[u8]) -> Result<Vec<u8>> {
        self.0.decrypt(XNonce::from_slice(nonce), Payload { msg: sealed, aad }).map_err(|_| CryptoError::Corrupt)
    }
}

/// Fills `buf` with secure random bytes.
pub fn fill_random(buf: &mut [u8]) -> Result<()> {
    getrandom::fill(buf).map_err(|e| CryptoError::Random(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrapped_keys_and_segments() {
        let (kek, _) = new_key("pw", None, M, T).unwrap();
        let key = BundleKey::random().unwrap();
        let wrapped = wrap_key(&kek, &key).unwrap();
        let back = unwrap_key(&kek, &wrapped).unwrap();
        assert_eq!(back.to_hex().as_str(), key.to_hex().as_str());
        assert_eq!(BundleKey::from_hex(&key.to_hex()).unwrap().to_hex().as_str(), key.to_hex().as_str());
        let (other, _) = new_key("other", None, M, T).unwrap();
        assert!(matches!(unwrap_key(&other, &wrapped), Err(CryptoError::WrongKey)));

        let c = SegmentCipher::new(&key);
        let nonce = [7u8; 24];
        let sealed = c.seal(&nonce, b"segment", b"aad").unwrap();
        assert_eq!(sealed.len(), 7 + TAG_LEN);
        assert_eq!(c.open(&nonce, &sealed, b"aad").unwrap(), b"segment");
        assert!(c.open(&nonce, &sealed, b"other").is_err());
    }

    // Small cost so tests are fast.
    const M: u32 = 64;
    const T: u32 = 1;

    #[test]
    fn round_trip_and_wrong_key() {
        let (key, params) = new_key("hunter2", None, M, T).unwrap();
        let sealed = seal(&key, b"secret data", b"db").unwrap();
        assert!(!sealed.windows(6).any(|w| w == b"secret"));
        assert_eq!(open(&key, &sealed, b"db").unwrap(), b"secret data");
        assert!(matches!(open(&key, &sealed, b"settings"), Err(CryptoError::Corrupt)), "label is bound");

        let again = unlock("hunter2", None, &params).unwrap();
        assert_eq!(open(&again, &sealed, b"db").unwrap(), b"secret data");
        assert!(matches!(unlock("wrong", None, &params), Err(CryptoError::WrongKey)));
    }

    #[test]
    fn keyfile_is_required_when_used() {
        let kf = b"some key file contents";
        let (_, params) = new_key("pw", Some(kf), M, T).unwrap();
        assert!(params.requires_keyfile);
        assert!(unlock("pw", Some(kf), &params).is_ok());
        assert!(matches!(unlock("pw", None, &params), Err(CryptoError::WrongKey)));
        assert!(matches!(unlock("pw", Some(b"other"), &params), Err(CryptoError::WrongKey)));
    }

    #[test]
    fn tamper_detected() {
        let (key, _) = new_key("pw", None, M, T).unwrap();
        let mut sealed = seal(&key, b"abc", b"x").unwrap();
        let last = sealed.len() - 1;
        sealed[last] ^= 1;
        assert!(open(&key, &sealed, b"x").is_err());
        assert!(open(&key, b"plain", b"x").is_err());
    }
}
