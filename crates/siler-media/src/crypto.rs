//! AES-128-CBC segment decryption with PKCS#7 padding (HLS `METHOD=AES-128`). Pure: bytes
//! in, bytes out. Keys are held in zeroizing memory and never printed.

use aes::Aes128;
use cbc::cipher::{BlockModeDecrypt, KeyIvInit};
use zeroize::Zeroizing;

type Dec = cbc::Decryptor<Aes128>;
type Block = cbc::cipher::Block<Dec>;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CryptoError {
    #[error("an AES-128 key is 16 bytes, not {0}")]
    KeyLength(usize),
    #[error("the encrypted data isn't a whole number of blocks")]
    Length,
    #[error("the encrypted data has bad padding (wrong key?)")]
    Padding,
}

/// A 128-bit key.
pub struct AesKey(Zeroizing<[u8; 16]>);

impl AesKey {
    /// Reads a key response: it must be exactly 16 bytes.
    pub fn from_bytes(bytes: &[u8]) -> Result<AesKey, CryptoError> {
        let arr: [u8; 16] = bytes
            .try_into()
            .map_err(|_| CryptoError::KeyLength(bytes.len()))?;
        Ok(AesKey(Zeroizing::new(arr)))
    }
}

impl std::fmt::Debug for AesKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AesKey(..)")
    }
}

/// The IV used when a playlist gives none: the segment's media sequence number as a 128-bit
/// big-endian integer.
pub fn iv_from_sequence(seq: u64) -> [u8; 16] {
    let mut iv = [0u8; 16];
    iv[8..].copy_from_slice(&seq.to_be_bytes());
    iv
}

/// Decrypts a segment as it arrives. Feed it with [`Decryptor::update`] and finish with
/// [`Decryptor::finish`], which checks and removes the padding.
pub struct Decryptor {
    inner: Dec,
    /// Ciphertext not yet decrypted. Between 16 and 31 bytes once it has data (the last
    /// block may be the padding block, so it is held back).
    pending: Vec<u8>,
}

impl Decryptor {
    pub fn new(key: &AesKey, iv: &[u8; 16]) -> Decryptor {
        let inner = Dec::new_from_slices(&key.0[..], iv).unwrap_or_else(|_| {
            // Both lengths are fixed at 16 bytes, so this is unreachable; the fallback keeps
            // the function total without a panic path.
            Dec::new(&Block::default(), &Block::default())
        });
        Decryptor {
            inner,
            pending: Vec::new(),
        }
    }

    /// Decrypts every block that can't be the last one, appending the plaintext to `out`.
    pub fn update(&mut self, input: &[u8], out: &mut Vec<u8>) {
        self.pending.extend_from_slice(input);
        if self.pending.len() <= 16 {
            return;
        }
        // Every whole block but the last one stays held back; the bytes are moved once.
        let ready = (self.pending.len() - 1) / 16 * 16;
        out.reserve(ready);
        for chunk in self.pending[..ready].as_chunks::<16>().0 {
            let mut block = Block::default();
            block.copy_from_slice(chunk);
            self.inner.decrypt_block(&mut block);
            out.extend_from_slice(&block);
        }
        self.pending.drain(..ready);
    }

    /// Decrypts the last block and removes its padding. Fails when the stream wasn't a whole
    /// number of blocks, or the padding is wrong (the wrong key gives this).
    pub fn finish(mut self, out: &mut Vec<u8>) -> Result<(), CryptoError> {
        if self.pending.len() != 16 {
            return Err(CryptoError::Length);
        }
        let mut block = Block::default();
        block.copy_from_slice(&self.pending);
        self.inner.decrypt_block(&mut block);
        let pad = block[15] as usize;
        if pad == 0 || pad > 16 || block[16 - pad..].iter().any(|&b| b as usize != pad) {
            return Err(CryptoError::Padding);
        }
        out.extend_from_slice(&block[..16 - pad]);
        Ok(())
    }
}

/// Decrypts a whole segment.
pub fn decrypt_aes128_cbc(
    data: &[u8],
    key: &AesKey,
    iv: &[u8; 16],
) -> Result<Vec<u8>, CryptoError> {
    let mut out = Vec::with_capacity(data.len());
    let mut dec = Decryptor::new(key, iv);
    dec.update(data, &mut out);
    dec.finish(&mut out)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cbc::cipher::BlockModeEncrypt;

    /// Encrypts `plain` with PKCS#7 padding, for round-trip tests.
    fn encrypt(plain: &[u8], key: &[u8; 16], iv: &[u8; 16]) -> Vec<u8> {
        let mut enc = cbc::Encryptor::<Aes128>::new_from_slices(key, iv).expect("lengths");
        let pad = 16 - plain.len() % 16;
        let mut data = plain.to_vec();
        data.extend(std::iter::repeat_n(pad as u8, pad));
        for i in (0..data.len()).step_by(16) {
            let mut b = Block::default();
            b.copy_from_slice(&data[i..i + 16]);
            enc.encrypt_block(&mut b);
            data[i..i + 16].copy_from_slice(&b);
        }
        data
    }

    #[test]
    fn round_trip_at_every_chunking() {
        let key_bytes = *b"0123456789abcdef";
        let key = AesKey::from_bytes(&key_bytes).expect("16 bytes");
        let iv = iv_from_sequence(7);
        let plain: Vec<u8> = (0..1000u32).map(|i| (i * 31 % 251) as u8).collect();
        let cipher = encrypt(&plain, &key_bytes, &iv);
        assert_eq!(
            decrypt_aes128_cbc(&cipher, &key, &iv).expect("decrypts"),
            plain
        );

        // Fed in pieces of every size, the result is the same.
        for step in [1usize, 7, 16, 17, 33] {
            let mut out = Vec::new();
            let mut dec = Decryptor::new(&key, &iv);
            for piece in cipher.chunks(step) {
                dec.update(piece, &mut out);
            }
            dec.finish(&mut out).expect("finishes");
            assert_eq!(out, plain, "step {step}");
        }
    }

    #[test]
    fn a_large_segment_decrypts_in_one_pass() {
        // Fed at once, as segments are: this took minutes when every block moved the rest.
        let key_bytes = [7u8; 16];
        let key = AesKey::from_bytes(&key_bytes).expect("16 bytes");
        let iv = [1u8; 16];
        let plain: Vec<u8> = (0..4 * 1024 * 1024u32).map(|i| (i % 253) as u8).collect();
        let cipher = encrypt(&plain, &key_bytes, &iv);
        let started = std::time::Instant::now();
        assert_eq!(
            decrypt_aes128_cbc(&cipher, &key, &iv).expect("decrypts"),
            plain
        );
        assert!(started.elapsed().as_secs() < 20);
    }

    #[test]
    fn exact_block_multiple_and_empty_plaintext() {
        let key_bytes = [9u8; 16];
        let key = AesKey::from_bytes(&key_bytes).expect("16 bytes");
        let iv = [3u8; 16];
        assert_eq!(
            decrypt_aes128_cbc(&encrypt(&[], &key_bytes, &iv), &key, &iv).expect("ok"),
            Vec::<u8>::new()
        );
        let sixteen = [5u8; 16];
        assert_eq!(
            decrypt_aes128_cbc(&encrypt(&sixteen, &key_bytes, &iv), &key, &iv).expect("ok"),
            sixteen
        );
    }

    #[test]
    fn bad_input_is_a_typed_error() {
        let key = AesKey::from_bytes(&[1u8; 16]).expect("16 bytes");
        let iv = [0u8; 16];
        assert_eq!(
            decrypt_aes128_cbc(&[0u8; 20], &key, &iv),
            Err(CryptoError::Length)
        );
        assert_eq!(decrypt_aes128_cbc(&[], &key, &iv), Err(CryptoError::Length));
        // Random data almost never has valid padding.
        assert_eq!(
            decrypt_aes128_cbc(&[0x42u8; 32], &key, &iv),
            Err(CryptoError::Padding)
        );
    }

    #[test]
    fn key_length_is_exact() {
        assert_eq!(
            AesKey::from_bytes(&[0u8; 15]).err(),
            Some(CryptoError::KeyLength(15))
        );
        assert_eq!(
            AesKey::from_bytes(&[0u8; 17]).err(),
            Some(CryptoError::KeyLength(17))
        );
        assert!(AesKey::from_bytes(&[0u8; 16]).is_ok());
        assert_eq!(
            format!("{:?}", AesKey::from_bytes(&[0u8; 16]).expect("ok")),
            "AesKey(..)"
        );
    }

    #[test]
    fn sequence_iv_is_big_endian() {
        let iv = iv_from_sequence(0x0102);
        assert_eq!(&iv[..8], &[0u8; 8]);
        assert_eq!(&iv[8..], &[0, 0, 0, 0, 0, 0, 1, 2]);
    }
}
