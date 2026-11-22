//! Kerberos for `Negotiate`, through MIT's GSS-API loaded when the program runs (it is
//! optional). It makes one token, for one named site: from the user's ticket, or from a
//! password in a cache of its own that lives in memory. Credentials are never delegated.

use libloading::Library;
use std::ffi::c_void;
use std::ptr::{null, null_mut};
use std::sync::{Arc, OnceLock};

const LIBRARY: &str = "libgssapi_krb5.so.2";

type Handle = *mut c_void;

#[repr(C)]
struct Oid {
    length: u32,
    elements: *const u8,
}

#[repr(C)]
struct OidSet {
    count: usize,
    elements: *const Oid,
}

#[repr(C)]
struct Buffer {
    length: usize,
    value: *mut c_void,
}

impl Buffer {
    fn empty() -> Buffer {
        Buffer {
            length: 0,
            value: null_mut(),
        }
    }

    /// Borrows `bytes` for a parameter the library only reads.
    fn of(bytes: &[u8]) -> Buffer {
        Buffer {
            length: bytes.len(),
            value: bytes.as_ptr().cast_mut().cast(),
        }
    }
}

/// `gss_channel_bindings_struct`.
#[repr(C)]
struct Bindings {
    initiator_type: u32,
    initiator: Buffer,
    acceptor_type: u32,
    acceptor: Buffer,
    data: Buffer,
}

/// 1.2.840.113554.1.2.2
const KRB5: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x12, 0x01, 0x02, 0x02];
/// GSS_KRB5_NT_PRINCIPAL_NAME
const PRINCIPAL: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x12, 0x01, 0x02, 0x02, 0x01];
/// GSS_C_NT_HOSTBASED_SERVICE
const SERVICE: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x12, 0x01, 0x02, 0x01, 0x04];
/// 1.3.6.1.5.5.2
const SPNEGO: &[u8] = &[0x2b, 0x06, 0x01, 0x05, 0x05, 0x02];

const INITIATE: i32 = 1;
/// GSS_C_REPLAY_FLAG alone: no delegation, and one message is enough.
const FLAGS: u32 = 4;

/// KRB5 errors that say the password is wrong, expired or locked: not to be tried again.
const PASSWORD_REFUSED: [i32; 4] = [
    -1_765_328_366, // KDC_ERR_CLIENT_REVOKED
    -1_765_328_361, // KDC_ERR_KEY_EXPIRED
    -1_765_328_360, // KDC_ERR_PREAUTH_FAILED
    -1_765_328_353, // KRB_AP_ERR_BAD_INTEGRITY
];

type ImportName = unsafe extern "C" fn(*mut u32, *const Buffer, *const Oid, *mut Handle) -> u32;
type ReleaseName = unsafe extern "C" fn(*mut u32, *mut Handle) -> u32;
type AcquireWithPassword = unsafe extern "C" fn(
    *mut u32,
    Handle,
    *const Buffer,
    u32,
    *const OidSet,
    i32,
    *mut Handle,
    *mut *mut OidSet,
    *mut u32,
) -> u32;
type ReleaseCred = unsafe extern "C" fn(*mut u32, *mut Handle) -> u32;
type InitContext = unsafe extern "C" fn(
    *mut u32,
    Handle,
    *mut Handle,
    Handle,
    *const Oid,
    u32,
    u32,
    *const Bindings,
    *const Buffer,
    *mut *const Oid,
    *mut Buffer,
    *mut u32,
    *mut u32,
) -> u32;
type DeleteContext = unsafe extern "C" fn(*mut u32, *mut Handle, *mut Buffer) -> u32;
type ReleaseBuffer = unsafe extern "C" fn(*mut u32, *mut Buffer) -> u32;

struct Gss {
    import_name: ImportName,
    release_name: ReleaseName,
    acquire_with_password: AcquireWithPassword,
    release_cred: ReleaseCred,
    init_context: InitContext,
    delete_context: DeleteContext,
    release_buffer: ReleaseBuffer,
    _lib: Library,
}

fn gss() -> Option<&'static Gss> {
    static GSS: OnceLock<Option<Gss>> = OnceLock::new();
    GSS.get_or_init(|| {
        // SAFETY: the library's initialisers have no preconditions, and each symbol's type
        // is the one in gssapi.h and gssapi_ext.h. They can't outlive `_lib`, in a static.
        let loaded = unsafe {
            (|| -> Result<Gss, libloading::Error> {
                let lib = Library::new(LIBRARY)?;
                Ok(Gss {
                    import_name: *lib.get(b"gss_import_name\0")?,
                    release_name: *lib.get(b"gss_release_name\0")?,
                    acquire_with_password: *lib.get(b"gss_acquire_cred_with_password\0")?,
                    release_cred: *lib.get(b"gss_release_cred\0")?,
                    init_context: *lib.get(b"gss_init_sec_context\0")?,
                    delete_context: *lib.get(b"gss_delete_sec_context\0")?,
                    release_buffer: *lib.get(b"gss_release_buffer\0")?,
                    _lib: lib,
                })
            })()
        };
        loaded
            .inspect_err(|e| tracing::info!("no Kerberos: {e}"))
            .ok()
    })
    .as_ref()
}

/// Whether this computer can do Kerberos at all.
pub(crate) fn available() -> bool {
    gss().is_some()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Failure {
    /// The password was refused.
    Refused,
    /// No ticket, no KDC, no such site in the realm, or no Kerberos.
    Unavailable,
}

impl Failure {
    fn of(what: &str, major: u32, minor: u32) -> Failure {
        tracing::debug!(major, minor = minor as i32, "Kerberos: {what} failed");
        if PASSWORD_REFUSED.contains(&(minor as i32)) {
            Failure::Refused
        } else {
            Failure::Unavailable
        }
    }
}

struct Name {
    gss: &'static Gss,
    handle: Handle,
}

impl Name {
    fn new(gss: &'static Gss, text: &str, kind: &'static [u8]) -> Result<Name, Failure> {
        let kind = Oid {
            length: kind.len() as u32,
            elements: kind.as_ptr(),
        };
        let text = Buffer::of(text.as_bytes());
        let (mut minor, mut handle) = (0, null_mut());
        // SAFETY: both inputs live through the call, which copies them.
        let major = unsafe { (gss.import_name)(&mut minor, &text, &kind, &mut handle) };
        if major != 0 {
            return Err(Failure::of("reading a name", major, minor));
        }
        Ok(Name { gss, handle })
    }
}

impl Drop for Name {
    fn drop(&mut self) {
        // SAFETY: the handle came from gss_import_name and is released once.
        unsafe { (self.gss.release_name)(&mut 0, &mut self.handle) };
    }
}

/// A ticket made from a password, in memory.
pub(crate) struct Cred {
    gss: &'static Gss,
    handle: Handle,
}

// SAFETY: MIT's credential handles carry their own lock and may be used from any thread.
unsafe impl Send for Cred {}
// SAFETY: as above.
unsafe impl Sync for Cred {}

impl Cred {
    /// Logs `user@REALM` in. Blocks while it talks to the KDC.
    pub fn with_password(user: &str, password: &str) -> Result<Arc<Cred>, Failure> {
        let gss = gss().ok_or(Failure::Unavailable)?;
        let name = Name::new(gss, &principal(user), PRINCIPAL)?;
        let mech = Oid {
            length: KRB5.len() as u32,
            elements: KRB5.as_ptr(),
        };
        let mechs = OidSet {
            count: 1,
            elements: &mech,
        };
        let password = Buffer::of(password.as_bytes());
        let (mut minor, mut handle) = (0, null_mut());
        // SAFETY: every pointer is valid for the call; the unused outputs may be null.
        let major = unsafe {
            (gss.acquire_with_password)(
                &mut minor,
                name.handle,
                &password,
                0,
                &mechs,
                INITIATE,
                &mut handle,
                null_mut(),
                null_mut(),
            )
        };
        if major != 0 {
            return Err(Failure::of("logging in", major, minor));
        }
        Ok(Arc::new(Cred { gss, handle }))
    }
}

impl Drop for Cred {
    fn drop(&mut self) {
        // SAFETY: the handle is this value's own and is released once.
        unsafe { (self.gss.release_cred)(&mut 0, &mut self.handle) };
    }
}

/// Realms are written in capitals; people type them as they come.
fn principal(user: &str) -> String {
    match user.rsplit_once('@') {
        Some((name, realm)) => format!("{name}@{}", realm.to_ascii_uppercase()),
        None => user.to_string(),
    }
}

/// A token for `HTTP@host`: from `cred`, or from the user's own ticket. Blocks while a
/// service ticket is fetched.
pub(crate) fn token(
    host: &str,
    cred: Option<&Cred>,
    binding: Option<&[u8]>,
) -> Result<Vec<u8>, Failure> {
    let gss = gss().ok_or(Failure::Unavailable)?;
    let target = Name::new(gss, &format!("HTTP@{host}"), SERVICE)?;
    let mech = Oid {
        length: KRB5.len() as u32,
        elements: KRB5.as_ptr(),
    };
    let bound = binding.map(|data| Bindings {
        initiator_type: 0,
        initiator: Buffer::empty(),
        acceptor_type: 0,
        acceptor: Buffer::empty(),
        data: Buffer::of(data),
    });
    let mut out = Buffer::empty();
    let (mut minor, mut context) = (0, null_mut());
    // SAFETY: every pointer is valid for the call. A null credential is the default one
    // (the user's ticket), and the unused inputs and outputs may be null. `out` is copied
    // and released, and the context deleted, before returning.
    unsafe {
        let major = (gss.init_context)(
            &mut minor,
            cred.map_or(null_mut(), |c| c.handle),
            &mut context,
            target.handle,
            &mech,
            FLAGS,
            0,
            bound.as_ref().map_or(null(), |b| b as *const Bindings),
            null(),
            null_mut(),
            &mut out,
            null_mut(),
            null_mut(),
        );
        // The upper half holds the errors; "continue" would be in the lower one.
        let made = major >> 16 == 0 && !out.value.is_null();
        let result = if made {
            Ok(spnego(std::slice::from_raw_parts(
                out.value.cast::<u8>(),
                out.length,
            )))
        } else {
            Err(Failure::of("making a token", major, minor))
        };
        (gss.release_buffer)(&mut 0, &mut out);
        if !context.is_null() {
            (gss.delete_context)(&mut 0, &mut context, null_mut());
        }
        result
    }
}

fn der(tag: u8, content: &[u8]) -> Vec<u8> {
    let mut out = vec![tag];
    let n = content.len();
    if n < 0x80 {
        out.push(n as u8);
    } else {
        let bytes: Vec<u8> = n
            .to_be_bytes()
            .into_iter()
            .skip_while(|b| *b == 0)
            .collect();
        out.push(0x80 | bytes.len() as u8);
        out.extend(bytes);
    }
    out.extend(content);
    out
}

/// A Kerberos token as SPNEGO's first message (RFC 4178), which RFC 4559 asks for. Offering
/// one mechanism needs no list MIC, and it keeps an installed NTLM module out of it.
fn spnego(token: &[u8]) -> Vec<u8> {
    let mechs = der(0xa0, &der(0x30, &der(0x06, KRB5)));
    let inner = der(0xa2, &der(0x04, token));
    let init = der(0xa0, &der(0x30, &[mechs, inner].concat()));
    der(0x60, &[der(0x06, SPNEGO), init].concat())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrapping() {
        assert_eq!(der(4, &[1, 2]), [4, 2, 1, 2]);
        assert_eq!(der(4, &[0; 300])[..4], [4, 0x82, 0x01, 0x2c]);
        let w = spnego(b"token");
        assert_eq!(w[..4], [0x60, w.len() as u8 - 2, 0x06, 0x06]);
        assert!(w.ends_with(b"token"));
    }

    #[test]
    fn principals() {
        assert_eq!(principal("jo@corp.example"), "jo@CORP.EXAMPLE");
        assert_eq!(principal("jo"), "jo");
    }
}
