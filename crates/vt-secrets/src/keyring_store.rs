//! [`SecretStore`] backed by the OS credential store.
//!
//! On macOS this is a generic password item in the user's login Keychain,
//! which is the item kind Claude Code reads and writes (service, account).
//! Built directly on `keyring-core` with the `apple-native-keyring-store`
//! keychain backend, so no process-wide default store is installed and no
//! other platform backend is linked. Windows support arrives in a later phase.
//!
//! Ruling R7: Claude Code reads its credential through `/usr/bin/security`.
//! An item created with the default access list trusts only its creator,
//! so Claude would get a prompt (or fail headless). A *new* Claude item is
//! therefore created with an access list trusting `/usr/bin/security` and
//! this binary; an existing item is updated in place and keeps its own
//! access list. The value goes through Security.framework only, never argv.

use anyhow::Result;
use vt_core::secret_types::SecretBytes;

use crate::SecretStore;

/// Secret store using the platform credential store.
#[derive(Debug, Default, Clone, Copy)]
pub struct KeyringStore;

impl KeyringStore {
    pub fn new() -> Self {
        Self
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use anyhow::{Result, anyhow};
    use apple_native_keyring_store::keychain::{Cred, MacKeychainDomain};
    use keyring_core::{Entry, Error};
    use security_framework::os::macos::keychain::{SecKeychain, SecPreferencesDomain};

    fn entry(service: &str, account: &str) -> Result<Entry> {
        Cred::build(MacKeychainDomain::User, service, account)
            .map_err(|e| anyhow!("Keychain kaydı hazırlanamadı ({service}): {}", describe(&e)))
    }

    pub fn get(service: &str, account: &str) -> Result<Option<Vec<u8>>> {
        match entry(service, account)?.get_secret() {
            Ok(bytes) => Ok(Some(bytes)),
            Err(Error::NoEntry) => Ok(None),
            Err(e) => Err(anyhow!(
                "Keychain kaydı okunamadı ({service}): {}",
                describe(&e)
            )),
        }
    }

    pub fn set(service: &str, account: &str, value: &[u8]) -> Result<()> {
        if service.starts_with(crate::CLAUDE_KEYCHAIN_SERVICE) {
            let keychain = SecKeychain::default_for_domain(SecPreferencesDomain::User)
                .map_err(|e| anyhow!("Keychain açılamadı: {}", e.message().unwrap_or_default()))?;
            if super::acl::create_trusting_security(&keychain, service, account, value)? {
                return Ok(());
            }
        }
        entry(service, account)?
            .set_secret(value)
            .map_err(|e| anyhow!("Keychain kaydı yazılamadı ({service}): {}", describe(&e)))
    }

    pub fn delete(service: &str, account: &str) -> Result<()> {
        match entry(service, account)?.delete_credential() {
            Ok(()) | Err(Error::NoEntry) => Ok(()),
            Err(e) => Err(anyhow!(
                "Keychain kaydı silinemedi ({service}): {}",
                describe(&e)
            )),
        }
    }

    /// Describes an error without any payload bytes it may carry.
    fn describe(e: &Error) -> String {
        match e {
            Error::PlatformFailure(p) | Error::NoStorageAccess(p) => p.to_string(),
            Error::BadEncoding(_) | Error::BadDataFormat(..) => "veri biçimi geçersiz".to_owned(),
            Error::Invalid(attr, reason) => format!("{attr}: {reason}"),
            _ => "beklenmeyen Keychain hatası".to_owned(),
        }
    }
}

#[cfg(target_os = "macos")]
pub(crate) mod acl {
    //! Creating a generic password item with an explicit access list.

    use std::ffi::{CString, c_char};
    use std::ptr;

    use anyhow::{Result, anyhow, bail};
    use core_foundation::array::{CFArray, CFArrayRef};
    use core_foundation::base::{CFType, CFTypeRef, OSStatus, TCFType};
    use core_foundation::data::CFData;
    use core_foundation::dictionary::CFDictionary;
    use core_foundation::string::{CFString, CFStringRef};
    use security_framework::os::macos::access::SecAccess;
    use security_framework::os::macos::keychain::SecKeychain;
    use security_framework_sys::base::{SecAccessRef, errSecDuplicateItem};
    use security_framework_sys::item::{
        kSecAttrAccount, kSecAttrLabel, kSecAttrService, kSecClass, kSecClassGenericPassword,
        kSecUseKeychain, kSecValueData,
    };
    use security_framework_sys::keychain_item::SecItemAdd;

    /// The tool Claude Code reads its credential with.
    pub const SECURITY_TOOL: &str = "/usr/bin/security";

    #[link(name = "Security", kind = "framework")]
    unsafe extern "C" {
        /// macOS-only `SecItem` key for a legacy access object.
        static kSecAttrAccess: CFStringRef;
        fn SecTrustedApplicationCreateFromPath(
            path: *const c_char,
            app: *mut CFTypeRef,
        ) -> OSStatus;
        fn SecAccessCreate(
            descriptor: CFStringRef,
            trusted_list: CFArrayRef,
            access: *mut SecAccessRef,
        ) -> OSStatus;
    }

    /// A trusted-application entry; `None` means the calling binary.
    fn trusted_app(path: Option<&str>) -> Result<CFType> {
        let c_path = path.map(CString::new).transpose()?;
        let mut app: CFTypeRef = ptr::null();
        // SAFETY: valid NUL-terminated path or null; `app` is an out pointer
        // that receives a +1 reference on success.
        let status = unsafe {
            SecTrustedApplicationCreateFromPath(
                c_path.as_ref().map_or(ptr::null(), |p| p.as_ptr()),
                &mut app,
            )
        };
        if status != 0 || app.is_null() {
            bail!(
                "güvenilir uygulama oluşturulamadı ({}): {status}",
                path.unwrap_or("vibe-transfer")
            );
        }
        // SAFETY: created above with a +1 reference.
        Ok(unsafe { CFType::wrap_under_create_rule(app) })
    }

    /// Access list trusting this binary and `/usr/bin/security`.
    fn access_for(descriptor: &str) -> Result<SecAccess> {
        let apps = CFArray::from_CFTypes(&[trusted_app(None)?, trusted_app(Some(SECURITY_TOOL))?]);
        let descriptor = CFString::new(descriptor);
        let mut access: SecAccessRef = ptr::null_mut();
        // SAFETY: valid CF objects; `access` receives a +1 reference.
        let status = unsafe {
            SecAccessCreate(
                descriptor.as_concrete_TypeRef(),
                apps.as_concrete_TypeRef(),
                &mut access,
            )
        };
        if status != 0 || access.is_null() {
            bail!("Keychain erişim listesi oluşturulamadı: {status}");
        }
        // SAFETY: created above with a +1 reference.
        Ok(unsafe { SecAccess::wrap_under_create_rule(access) })
    }

    /// Creates the generic password `(service, account)` in `keychain`
    /// with the access list above. Returns `false` (nothing written) when
    /// the item already exists, so the caller updates it in place.
    pub fn create_trusting_security(
        keychain: &SecKeychain,
        service: &str,
        account: &str,
        value: &[u8],
    ) -> Result<bool> {
        let access = access_for(service)?;
        // SAFETY: the `kSec*` keys are immutable framework constants.
        let pairs: Vec<(CFString, CFType)> = unsafe {
            vec![
                (
                    CFString::wrap_under_get_rule(kSecClass),
                    CFString::wrap_under_get_rule(kSecClassGenericPassword).as_CFType(),
                ),
                (
                    CFString::wrap_under_get_rule(kSecAttrService),
                    CFString::new(service).as_CFType(),
                ),
                (
                    CFString::wrap_under_get_rule(kSecAttrAccount),
                    CFString::new(account).as_CFType(),
                ),
                (
                    CFString::wrap_under_get_rule(kSecAttrLabel),
                    CFString::new(service).as_CFType(),
                ),
                (
                    CFString::wrap_under_get_rule(kSecValueData),
                    CFData::from_buffer(value).as_CFType(),
                ),
                (
                    CFString::wrap_under_get_rule(kSecAttrAccess),
                    access.as_CFType(),
                ),
                (
                    CFString::wrap_under_get_rule(kSecUseKeychain),
                    keychain.as_CFType(),
                ),
            ]
        };
        let query = CFDictionary::from_CFType_pairs(&pairs);
        // SAFETY: valid dictionary; no result requested.
        let status = unsafe { SecItemAdd(query.as_concrete_TypeRef(), ptr::null_mut()) };
        match status {
            0 => Ok(true),
            s if s == errSecDuplicateItem => Ok(false),
            s => Err(anyhow!("Keychain kaydı oluşturulamadı ({service}): {s}")),
        }
    }

    /// Never touches the login Keychain: everything happens in a throwaway
    /// keychain file in a temp dir, deleted at the end.
    #[cfg(test)]
    mod tests {
        use std::process::{Command, Stdio};
        use std::time::{Duration, Instant};

        use security_framework::os::macos::keychain::CreateOptions;
        use security_framework_sys::base::SecKeychainRef;

        use super::*;

        #[link(name = "Security", kind = "framework")]
        unsafe extern "C" {
            fn SecKeychainDelete(keychain: SecKeychainRef) -> OSStatus;
        }

        /// Runs `/usr/bin/security` with a deadline; a prompt would hang it.
        fn security(args: &[&str]) -> (bool, String) {
            let mut child = Command::new(SECURITY_TOOL)
                .args(args)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            let start = Instant::now();
            loop {
                if child.try_wait().unwrap().is_some() {
                    break;
                }
                if start.elapsed() > Duration::from_secs(15) {
                    let _ = child.kill();
                    panic!("security {args:?} did not finish (access prompt?)");
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            let out = child.wait_with_output().unwrap();
            (
                out.status.success(),
                String::from_utf8_lossy(&out.stdout).into_owned(),
            )
        }

        #[test]
        #[ignore = "creates a throwaway keychain file; run manually on macOS"]
        fn new_item_is_readable_by_security_tool() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("vt-acl-test.keychain-db");
            let path_str = path.to_str().unwrap();
            let keychain = CreateOptions::new()
                .password("vt-acl-test")
                .create(&path)
                .unwrap();
            let service = format!("{}-acl-test", crate::CLAUDE_KEYCHAIN_SERVICE);
            let value = format!("{}{}", "synthetic-", "acl-value");

            let result = std::panic::catch_unwind(|| {
                assert!(
                    create_trusting_security(&keychain, &service, "bob", value.as_bytes()).unwrap()
                );
                // A second create reports the existing item.
                assert!(!create_trusting_security(&keychain, &service, "bob", b"other").unwrap());
                let (ok, out) = security(&[
                    "find-generic-password",
                    "-s",
                    &service,
                    "-a",
                    "bob",
                    "-w",
                    path_str,
                ]);
                assert!(ok);
                assert_eq!(out.trim_end(), value);
                let (ok, acl) = security(&["dump-keychain", "-a", path_str]);
                assert!(ok);
                assert!(acl.contains(SECURITY_TOOL), "{acl}");
            });
            // SAFETY: valid keychain reference; removes the temp keychain
            // file and any search-list entry it got.
            unsafe { SecKeychainDelete(keychain.as_concrete_TypeRef()) };
            result.unwrap();
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    use anyhow::{Result, bail};

    pub fn get(_service: &str, _account: &str) -> Result<Option<Vec<u8>>> {
        bail!("bu platformda sır deposu henüz desteklenmiyor")
    }

    pub fn set(_service: &str, _account: &str, _value: &[u8]) -> Result<()> {
        bail!("bu platformda sır deposu henüz desteklenmiyor")
    }

    pub fn delete(_service: &str, _account: &str) -> Result<()> {
        bail!("bu platformda sır deposu henüz desteklenmiyor")
    }
}

impl SecretStore for KeyringStore {
    fn get(&self, service: &str, account: &str) -> Result<Option<SecretBytes>> {
        Ok(platform::get(service, account)?.map(SecretBytes::new))
    }

    fn set(&self, service: &str, account: &str, value: &SecretBytes) -> Result<()> {
        platform::set(service, account, value.expose())
    }

    fn delete(&self, service: &str, account: &str) -> Result<()> {
        platform::delete(service, account)
    }
}
