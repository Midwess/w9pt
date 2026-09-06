#[cfg(feature = "encryption-aes-siv")]
use zeroize::Zeroize;

use crate::RepresentationError;

pub(super) fn derive<const N: usize>(label: &'static str, parts: &[&[u8]]) -> [u8; N] {
    let mut hasher = blake3::Hasher::new_derive_key(label);
    for part in parts {
        hasher.update(&u64::try_from(part.len()).unwrap_or(u64::MAX).to_le_bytes());
        hasher.update(part);
    }
    let mut output = [0; N];
    hasher.finalize_xof().fill(&mut output);
    output
}

pub(super) fn encrypt(
    file_dek: &[u8; 32],
    object_key: &[u8],
    aad: &[u8],
    plaintext: &[u8],
) -> Result<Vec<u8>, RepresentationError> {
    encrypt_siv(file_dek, object_key, aad, plaintext)
}

pub(super) fn decrypt(
    file_dek: &[u8; 32],
    object_key: &[u8],
    aad: &[u8],
    ciphertext: &[u8],
) -> Result<Vec<u8>, RepresentationError> {
    decrypt_siv(file_dek, object_key, aad, ciphertext)
}

#[cfg(feature = "encryption-aes-siv")]
fn encrypt_siv(
    file_dek: &[u8; 32],
    object_key: &[u8],
    aad: &[u8],
    plaintext: &[u8],
) -> Result<Vec<u8>, RepresentationError> {
    use aes_siv::{KeyInit, siv::Aes256Siv};
    let mut key = derive::<64>(
        "w9pt v3 per-object AES-256-SIV key",
        &[file_dek, object_key],
    );
    let mut cipher = Aes256Siv::new((&key).into());
    let result = cipher
        .encrypt([aad], plaintext)
        .map_err(|_| RepresentationError::AuthenticationFailed);
    key.zeroize();
    result
}

#[cfg(not(feature = "encryption-aes-siv"))]
fn encrypt_siv(
    _file_dek: &[u8; 32],
    _object_key: &[u8],
    _aad: &[u8],
    _plaintext: &[u8],
) -> Result<Vec<u8>, RepresentationError> {
    Err(RepresentationError::UnsupportedCipher)
}

#[cfg(feature = "encryption-aes-siv")]
fn decrypt_siv(
    file_dek: &[u8; 32],
    object_key: &[u8],
    aad: &[u8],
    ciphertext: &[u8],
) -> Result<Vec<u8>, RepresentationError> {
    use aes_siv::{KeyInit, siv::Aes256Siv};
    let mut key = derive::<64>(
        "w9pt v3 per-object AES-256-SIV key",
        &[file_dek, object_key],
    );
    let mut cipher = Aes256Siv::new((&key).into());
    let result = cipher
        .decrypt([aad], ciphertext)
        .map_err(|_| RepresentationError::AuthenticationFailed);
    key.zeroize();
    result
}

#[cfg(not(feature = "encryption-aes-siv"))]
fn decrypt_siv(
    _file_dek: &[u8; 32],
    _object_key: &[u8],
    _aad: &[u8],
    _ciphertext: &[u8],
) -> Result<Vec<u8>, RepresentationError> {
    Err(RepresentationError::UnsupportedCipher)
}

#[cfg(all(test, feature = "encryption-aes-siv"))]
mod tests {
    use super::*;

    #[test]
    fn siv_is_deterministic_key_and_aad_bound() {
        let dek = [7; 32];
        let first = encrypt(&dek, b"object-a", b"aad", b"deterministic plaintext").unwrap();
        assert_eq!(
            derive::<32>("w9pt v3 vector", &[b"input"])
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>(),
            "c2d240da2166ad28c0fa0281f8091b9afb483fdac4fd146b2cf5ee289fa500b2"
        );
        assert_eq!(
            first
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>(),
            "507b06f024afb63bc619eac335406b2eb3c779a384b27f3262f2e16564952d90eb1efab89f2148"
        );
        let second = encrypt(&dek, b"object-a", b"aad", b"deterministic plaintext").unwrap();
        assert_eq!(first, second);
        assert_eq!(
            decrypt(&dek, b"object-a", b"aad", &first).unwrap(),
            b"deterministic plaintext"
        );
        assert!(decrypt(&dek, b"object-b", b"aad", &first).is_err());
        assert!(decrypt(&dek, b"object-a", b"other", &first).is_err());
        let mut tampered = first;
        *tampered.last_mut().unwrap() ^= 1;
        assert_eq!(
            decrypt(&dek, b"object-a", b"aad", &tampered),
            Err(RepresentationError::AuthenticationFailed)
        );
    }
}
