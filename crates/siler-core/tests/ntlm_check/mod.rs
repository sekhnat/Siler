//! The server's half of NTLMv2, for tests: a challenge, and whether an answer to it is right.

use hmac::{Hmac, KeyInit, Mac};
use md4::Md4;
use md5::{Digest, Md5};

pub const USER: &str = "jo";
pub const DOMAIN: &str = "CORP";
pub const PASS: &str = "Pässword!";

fn utf16(s: &str) -> Vec<u8> {
    s.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

fn hmac(key: &[u8], parts: &[&[u8]]) -> Vec<u8> {
    let mut m = Hmac::<Md5>::new_from_slice(key).unwrap();
    for p in parts {
        m.update(p);
    }
    m.finalize().into_bytes().to_vec()
}

fn pair(out: &mut Vec<u8>, id: u16, v: &[u8]) {
    out.extend(id.to_le_bytes());
    out.extend((v.len() as u16).to_le_bytes());
    out.extend(v);
}

pub fn challenge(nonce: &[u8; 8]) -> Vec<u8> {
    let mut info = Vec::new();
    pair(&mut info, 2, &utf16(DOMAIN));
    pair(&mut info, 7, &133_000_000_000_000_000u64.to_le_bytes());
    pair(&mut info, 0, &[]);
    let mut m = b"NTLMSSP\0".to_vec();
    m.extend(2u32.to_le_bytes());
    m.extend([0; 8]);
    m.extend(0xa288_8205u32.to_le_bytes());
    m.extend(nonce);
    m.extend([0; 8]);
    m.extend((info.len() as u16).to_le_bytes());
    m.extend((info.len() as u16).to_le_bytes());
    m.extend(56u32.to_le_bytes());
    m.extend([0; 8]);
    m.extend(info);
    m
}

fn field(m: &[u8], at: usize) -> &[u8] {
    let len = u16::from_le_bytes([m[at], m[at + 1]]) as usize;
    let from = u32::from_le_bytes(m[at + 4..at + 8].try_into().unwrap()) as usize;
    &m[from..from + len]
}

pub fn right_answer(t1: &[u8], t2: &[u8], nonce: &[u8; 8], t3: &[u8]) -> bool {
    let (nt, domain, user) = (field(t3, 20), field(t3, 28), field(t3, 36));
    if user != utf16(USER) || domain != utf16(DOMAIN) || nt.len() < 48 {
        return false;
    }
    let hash = Md4::digest(utf16(PASS));
    let name = utf16(&format!("{}{DOMAIN}", USER.to_uppercase()));
    let key = hmac(&hash, &[&name]);
    let proof = hmac(&key, &[nonce, &nt[16..]]);
    if proof != nt[..16] {
        return false;
    }
    let session = hmac(&key, &[&proof]);
    let mut unsigned = t3.to_vec();
    unsigned[72..88].fill(0);
    assert_eq!(hmac(&session, &[t1, t2, &unsigned]), t3[72..88], "the MIC");
    let target = utf16("HTTP/127.0.0.1");
    assert!(
        nt.windows(target.len()).any(|w| w == target),
        "the target's name"
    );
    true
}
