//! Windows half of `crate::provider_auth`: API keys are stored through
//! Credential Manager directly (not the `keyring` crate), under the target
//! name `crate::provider_auth::vault_target(service, account)`.

use crate::provider_auth::vault_target;
use crate::provider_auth::SecretStoreError;

pub fn get(
    service: &'static str,
    account: &'static str,
) -> Result<Option<String>, SecretStoreError> {
    windows_vault::get(&vault_target(service, account))
}

pub fn set(
    service: &'static str,
    account: &'static str,
    secret: &str,
) -> Result<(), SecretStoreError> {
    windows_vault::set(&vault_target(service, account), secret)
}

pub fn delete(service: &'static str, account: &'static str) -> Result<(), SecretStoreError> {
    windows_vault::delete(&vault_target(service, account))
}

mod windows_vault {
    use std::slice;

    use std::ffi::c_void;

    use zeroize::Zeroizing;

    const CRED_TYPE_GENERIC: u32 = 1;
    const CRED_PERSIST_LOCAL_MACHINE: u32 = 2;
    const ERROR_NOT_FOUND: u32 = 1168;

    #[repr(C)]
    struct FileTime {
        low: u32,
        high: u32,
    }

    #[repr(C)]
    struct Credential {
        flags: u32,
        credential_type: u32,
        target_name: *mut u16,
        comment: *mut u16,
        last_written: FileTime,
        blob_size: u32,
        blob: *mut u8,
        persist: u32,
        attribute_count: u32,
        attributes: *mut c_void,
        target_alias: *mut u16,
        user_name: *mut u16,
    }

    // Lowercase: xwin's vendored SDK (used by the cross-compiled CI build)
    // normalizes lib filenames to lowercase, and lld-link on a case-sensitive
    // host filesystem fails to find "Advapi32.lib". Native MSVC builds on
    // Windows work with either case since NTFS is case-insensitive.
    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn CredReadW(
            target: *const u16,
            credential_type: u32,
            flags: u32,
            credential: *mut *mut Credential,
        ) -> i32;
        fn CredWriteW(credential: *const Credential, flags: u32) -> i32;
        fn CredDeleteW(target: *const u16, credential_type: u32, flags: u32) -> i32;
        fn CredFree(buffer: *const c_void);
    }

    unsafe extern "system" {
        fn GetLastError() -> u32;
    }

    use super::SecretStoreError;

    fn wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(Some(0)).collect()
    }

    fn is_missing() -> bool {
        // SAFETY: GetLastError reads the current thread's Win32 failure code
        // immediately after a Credential Manager call has returned false.
        unsafe { GetLastError() == ERROR_NOT_FOUND }
    }

    pub fn get(target: &str) -> Result<Option<String>, SecretStoreError> {
        let target = wide(target);
        let mut credential = std::ptr::null_mut();
        // SAFETY: `target` is NUL-terminated and Windows initializes the output
        // pointer only on success, which we free after copying the blob.
        let result = unsafe { CredReadW(target.as_ptr(), CRED_TYPE_GENERIC, 0, &mut credential) };
        if result == 0 {
            return if is_missing() {
                Ok(None)
            } else {
                Err(SecretStoreError::Unavailable)
            };
        }
        // SAFETY: `credential` was allocated by CredReadW. Its blob is valid
        // for `CredentialBlobSize` bytes until the matching CredFree below.
        let secret = unsafe {
            let bytes = slice::from_raw_parts((*credential).blob, (*credential).blob_size as usize);
            String::from_utf8(bytes.to_vec()).map_err(|_| SecretStoreError::Unavailable)
        };
        // SAFETY: the pointer came from CredReadW and is freed exactly once.
        unsafe { CredFree(credential.cast::<std::ffi::c_void>()) };
        secret.map(Some)
    }

    pub fn set(target: &str, secret: &str) -> Result<(), SecretStoreError> {
        let mut target = wide(target);
        let mut user = wide("clud");
        let mut blob = Zeroizing::new(secret.as_bytes().to_vec());
        // SAFETY: CREDENTIALW is a Win32 C struct containing only integer,
        // pointer, and FILETIME fields, all of which permit zero initialization.
        // The fields Credential Manager requires are set below before use.
        let mut credential: Credential = unsafe { std::mem::zeroed() };
        credential.credential_type = CRED_TYPE_GENERIC;
        credential.target_name = target.as_mut_ptr();
        credential.blob_size = blob
            .len()
            .try_into()
            .map_err(|_| SecretStoreError::Unavailable)?;
        credential.blob = blob.as_mut_ptr();
        credential.persist = CRED_PERSIST_LOCAL_MACHINE;
        credential.user_name = user.as_mut_ptr();
        // SAFETY: every pointer in `credential` remains live for this call.
        if unsafe { CredWriteW(&credential, 0) } == 0 {
            Err(SecretStoreError::Unavailable)
        } else {
            Ok(())
        }
    }

    pub fn delete(target: &str) -> Result<(), SecretStoreError> {
        let target = wide(target);
        // SAFETY: `target` is NUL-terminated and lives for this call.
        if unsafe { CredDeleteW(target.as_ptr(), CRED_TYPE_GENERIC, 0) } != 0 || is_missing() {
            Ok(())
        } else {
            Err(SecretStoreError::Unavailable)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::windows_vault;

    /// Windows stores keys through Credential Manager directly (not the
    /// `keyring` crate). Round-trip a key through the real API so a Windows
    /// regression in the storage path fails CI rather than a user's launch.
    #[test]
    fn windows_credential_manager_round_trips_an_api_key() {
        let target = format!("clud.test-inline-api-key/{}", std::process::id());
        let key = "sk-0123456789abcdef0123456789abcdef";
        if let Err(error) = windows_vault::set(&target, key) {
            eprintln!("SKIP: Credential Manager unavailable on this runner: {error}");
            return;
        }
        let read = windows_vault::get(&target);
        let _ = windows_vault::delete(&target);
        assert_eq!(read, Ok(Some(key.to_string())));
        assert_eq!(windows_vault::get(&target), Ok(None));
    }
}
