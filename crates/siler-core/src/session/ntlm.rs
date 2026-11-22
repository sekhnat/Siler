//! NTLMv2 (MS-NLMP) for HTTP: the client's two messages, with the MIC and the TLS channel
//! binding. No NTLMv1, no LM hash, no signing or sealing of what follows.

use hmac::{Hmac, KeyInit, Mac};
use md4::Md4;
use md5::{Digest, Md5};
use std::time::{SystemTime, UNIX_EPOCH};

const SIGNATURE: &[u8; 8] = b"NTLMSSP\0";

const UNICODE: u32 = 0x0000_0001;
const REQUEST_TARGET: u32 = 0x0000_0004;
const NTLM: u32 = 0x0000_0200;
const ALWAYS_SIGN: u32 = 0x0000_8000;
const EXTENDED_SESSION: u32 = 0x0008_0000;
const TARGET_INFO: u32 = 0x0080_0000;
const VERSION: u32 = 0x0200_0000;
const STRONG_128: u32 = 0x2000_0000;
const WANTED: u32 =
    UNICODE | REQUEST_TARGET | NTLM | ALWAYS_SIGN | EXTENDED_SESSION | TARGET_INFO | STRONG_128;
/// The version field: Windows 10, NTLM revision 15.
const AS_VERSION: [u8; 8] = [10, 0, 0, 0, 0, 0, 0, 15];

const AV_EOL: u16 = 0;
const AV_FLAGS: u16 = 6;
const AV_TIMESTAMP: u16 = 7;
const AV_TARGET_NAME: u16 = 9;
const AV_CHANNEL_BINDINGS: u16 = 10;
const AV_FLAG_MIC: u32 = 2;

/// Where the MIC sits in the third message, and where that message's payload starts.
const MIC_AT: usize = 72;
const PAYLOAD_AT: usize = 88;
/// Seconds from 1601 (Windows' epoch) to 1970.
const FILETIME_EPOCH: u64 = 11_644_473_600;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Unusable {
    BadChallenge,
    /// The server only speaks NTLMv1.
    TooOld,
}

/// What an answer is bound to: the server's name and, over TLS, its certificate.
pub(crate) struct Target<'a> {
    pub host: &'a str,
    /// The application data of the `tls-server-end-point` binding.
    pub binding: Option<&'a [u8]>,
}

fn utf16(s: &str) -> Vec<u8> {
    s.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

fn hmac(key: &[u8], parts: &[&[u8]]) -> [u8; 16] {
    let mut m = Hmac::<Md5>::new_from_slice(key).expect("HMAC takes a key of any length");
    for p in parts {
        m.update(p);
    }
    m.finalize().into_bytes().into()
}

fn owf_v2(user: &str, domain: &str, password: &str) -> [u8; 16] {
    let nt: [u8; 16] = Md4::digest(utf16(password)).into();
    hmac(&nt, &[&utf16(&format!("{}{domain}", user.to_uppercase()))])
}

/// `DOMAIN\user` gives its domain; `user@realm` and a bare name go as they are.
fn split_user(login: &str) -> (&str, &str) {
    match login.split_once('\\') {
        Some((domain, user)) => (domain, user),
        None => ("", login),
    }
}

/// The first message.
pub(crate) fn negotiate() -> Vec<u8> {
    let mut m = SIGNATURE.to_vec();
    m.extend(1u32.to_le_bytes());
    m.extend((WANTED | VERSION).to_le_bytes());
    m.extend([0; 16]); // no domain and no workstation
    m.extend(AS_VERSION);
    m
}

struct Challenge<'a> {
    flags: u32,
    nonce: &'a [u8],
    info: Vec<(u16, &'a [u8])>,
}

fn parse(m: &[u8]) -> Result<Challenge<'_>, Unusable> {
    use Unusable::{BadChallenge, TooOld};
    let u16_at = |at: usize| m.get(at..at + 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
    let u32_at = |at: usize| {
        m.get(at..at + 4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    };
    if !m.starts_with(SIGNATURE) || u32_at(8) != Some(2) || m.len() < 32 {
        return Err(BadChallenge);
    }
    let flags = u32_at(20).ok_or(BadChallenge)?;
    if flags & TARGET_INFO == 0 || m.len() < 48 {
        return Err(TooOld);
    }
    let len = usize::from(u16_at(40).ok_or(BadChallenge)?);
    let at = u32_at(44).ok_or(BadChallenge)? as usize;
    let mut rest = m.get(at..).and_then(|r| r.get(..len)).ok_or(BadChallenge)?;
    let mut info = Vec::new();
    loop {
        let [a, b, c, d, tail @ ..] = rest else {
            return Err(BadChallenge);
        };
        let id = u16::from_le_bytes([*a, *b]);
        if id == AV_EOL {
            break;
        }
        let n = usize::from(u16::from_le_bytes([*c, *d]));
        info.push((id, tail.get(..n).ok_or(BadChallenge)?));
        rest = &tail[n..];
    }
    Ok(Challenge {
        flags,
        nonce: &m[24..32],
        info,
    })
}

fn pair(out: &mut Vec<u8>, id: u16, value: &[u8]) {
    out.extend(id.to_le_bytes());
    out.extend((value.len() as u16).to_le_bytes());
    out.extend(value);
}

/// The MD5 of a `gss_channel_bindings_struct` with empty addresses.
fn binding_hash(data: &[u8]) -> [u8; 16] {
    let mut md = Md5::new();
    md.update([0u8; 16]);
    md.update((data.len() as u32).to_le_bytes());
    md.update(data);
    md.finalize().into()
}

struct Proof {
    nt: Vec<u8>,
    session: [u8; 16],
}

fn prove(key: &[u8; 16], nonce: &[u8], client: &[u8; 8], time: &[u8; 8], info: &[u8]) -> Proof {
    let mut temp = vec![1, 1, 0, 0, 0, 0, 0, 0];
    temp.extend(time);
    temp.extend(client);
    temp.extend([0; 4]);
    temp.extend(info);
    temp.extend([0; 4]);
    let proof = hmac(key, &[nonce, &temp]);
    Proof {
        session: hmac(key, &[&proof]),
        nt: [&proof[..], &temp].concat(),
    }
}

fn filetime_now() -> [u8; 8] {
    let d = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    ((d.as_secs() + FILETIME_EPOCH) * 10_000_000 + u64::from(d.subsec_nanos() / 100)).to_le_bytes()
}

/// The third message: the answer to `challenge`, which followed `negotiate`.
pub(crate) fn authenticate(
    negotiate: &[u8],
    challenge: &[u8],
    login: &str,
    password: &str,
    client: [u8; 8],
    to: &Target<'_>,
) -> Result<Vec<u8>, Unusable> {
    let ch = parse(challenge)?;
    let (domain, user) = split_user(login);
    let key = owf_v2(user, domain, password);
    let stamp: Option<[u8; 8]> = ch
        .info
        .iter()
        .find(|(id, _)| *id == AV_TIMESTAMP)
        .and_then(|(_, v)| (*v).try_into().ok());

    let mut info = Vec::new();
    let mut av_flags = 0u32;
    for (id, v) in &ch.info {
        match (*id, *v) {
            (AV_TARGET_NAME | AV_CHANNEL_BINDINGS, _) => {}
            (AV_FLAGS, [a, b, c, d]) => av_flags = u32::from_le_bytes([*a, *b, *c, *d]),
            (id, v) => pair(&mut info, id, v),
        }
    }
    if stamp.is_some() {
        av_flags |= AV_FLAG_MIC;
    }
    if av_flags != 0 {
        pair(&mut info, AV_FLAGS, &av_flags.to_le_bytes());
    }
    if let Some(data) = to.binding {
        pair(&mut info, AV_CHANNEL_BINDINGS, &binding_hash(data));
    }
    pair(
        &mut info,
        AV_TARGET_NAME,
        &utf16(&format!("HTTP/{}", to.host)),
    );
    pair(&mut info, AV_EOL, &[]);

    let time = stamp.unwrap_or_else(filetime_now);
    let proof = prove(&key, ch.nonce, &client, &time, &info);
    let lm = match stamp {
        Some(_) => vec![0; 24],
        None => [&hmac(&key, &[ch.nonce, &client])[..], &client].concat(),
    };
    // No key exchange is asked for, so the exported key is the session base key.
    let flags = (ch.flags & WANTED) | UNICODE | VERSION;

    let (domain, user) = (utf16(domain), utf16(user));
    let fields: [&[u8]; 6] = [&lm, &proof.nt, &domain, &user, &[], &[]];
    let mut m = SIGNATURE.to_vec();
    m.extend(3u32.to_le_bytes());
    let mut at = PAYLOAD_AT;
    for f in fields {
        let n = u16::try_from(f.len()).map_err(|_| Unusable::BadChallenge)?;
        m.extend(n.to_le_bytes());
        m.extend(n.to_le_bytes());
        m.extend((at as u32).to_le_bytes());
        at += f.len();
    }
    m.extend(flags.to_le_bytes());
    m.extend(AS_VERSION);
    m.extend([0; 16]);
    for f in fields {
        m.extend(f);
    }
    if stamp.is_some() {
        let mic = hmac(&proof.session, &[negotiate, challenge, &m]);
        m[MIC_AT..PAYLOAD_AT].copy_from_slice(&mic);
    }
    Ok(m)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(b: &[u8]) -> String {
        b.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// MS-NLMP 4.2.4, the NTLMv2 example.
    #[test]
    fn the_specifications_example() {
        let key = owf_v2("User", "Domain", "Password");
        assert_eq!(hex(&key), "0c868a403bfd7a93a3001ef22ef02e3f");
        let nonce = [0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef];
        let client = [0xaa; 8];
        assert_eq!(
            hex(&hmac(&key, &[&nonce, &client])),
            "86c35097ac9cec102554764a57cccc19"
        );
        let mut info = Vec::new();
        pair(&mut info, 2, &utf16("Domain"));
        pair(&mut info, 1, &utf16("Server"));
        pair(&mut info, AV_EOL, &[]);
        let p = prove(&key, &nonce, &client, &[0; 8], &info);
        assert_eq!(hex(&p.nt[..16]), "68cd0ab851e51c96aabc927bebef6a1c");
        assert_eq!(hex(&p.session), "8de40ccadbc14a82f15cb0ad0de95ca3");
    }

    #[test]
    fn junk_is_refused() {
        assert_eq!(parse(b"NTLMSSP\0").err(), Some(Unusable::BadChallenge));
        let mut m = [&SIGNATURE[..], &2u32.to_le_bytes(), &[0; 36]].concat();
        assert_eq!(parse(&m).err(), Some(Unusable::TooOld));
        m[20..24].copy_from_slice(&TARGET_INFO.to_le_bytes());
        assert_eq!(parse(&m).err(), Some(Unusable::BadChallenge));
        m[40] = 0xff;
        m[44] = 0xff;
        assert_eq!(parse(&m).err(), Some(Unusable::BadChallenge));
    }
}
