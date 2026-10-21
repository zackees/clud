//! Windows half of `crate::codex_auth`: the owner-only credential file ACL.

use std::fs::OpenOptions;
use std::path::Path;

/// No-op on Windows: there is no Unix mode to set at open time; the file is
/// protected by [`protect_windows_credential_file`] instead.
pub fn restrict_credential_open_options(_options: &mut OpenOptions) {}

/// Windows has no portable equivalent of Unix mode 0600.  Do not merely rely
/// on the profile directory's inherited ACL: network homes and redirected
/// profiles can make that inheritance broader than a credential file permits.
///
/// `OW` is the SDDL owner-rights principal, so the only explicit ACE grants
/// full access to the file's current owner.  The protected DACL disables
/// inherited entries.  Administrators with backup/restore privileges remain
/// an OS trust boundary; ordinary other local users cannot read this file.
#[expect(
    clippy::too_many_lines,
    reason = "complexity ratchet baseline (zackees/ci.yml#229); split this function"
)]
pub fn protect_windows_credential_file(path: &Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;

    use windows::core::{w, PCWSTR};
    use windows::Win32::Foundation::{LocalFree, HLOCAL};
    use windows::Win32::Security::Authorization::{
        ConvertSecurityDescriptorToStringSecurityDescriptorW,
        ConvertStringSecurityDescriptorToSecurityDescriptorW, GetNamedSecurityInfoW,
        SetNamedSecurityInfoW, SDDL_REVISION_1, SE_FILE_OBJECT,
    };
    use windows::Win32::Security::{
        DACL_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
        SE_DACL_PROTECTED,
    };

    const OWNER_ONLY_ACE: &str = "(A;;FA;;;OW)";
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    // SAFETY: the SDDL is a static NUL-terminated Windows string and Windows
    // allocates `descriptor`, which is freed on every path below.
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            w!("D:P(A;;FA;;;OW)"),
            SDDL_REVISION_1,
            &mut descriptor,
            None,
        )
        .map_err(|error| format!("could not construct Windows credential ACL: {error}"))?;
    }
    let result = (|| {
        let mut present = windows::core::BOOL::default();
        let mut dacl = std::ptr::null_mut();
        let mut defaulted = windows::core::BOOL::default();
        // SAFETY: `descriptor` was returned by the conversion API and remains
        // live until the cleanup below.
        unsafe {
            windows::Win32::Security::GetSecurityDescriptorDacl(
                descriptor,
                &mut present,
                &mut dacl,
                &mut defaulted,
            )
            .map_err(|error| format!("could not inspect Windows credential ACL: {error}"))?;
        }
        if !present.as_bool() || dacl.is_null() {
            return Err("Windows credential ACL unexpectedly has no DACL".to_string());
        }
        let mut wide_path: Vec<u16> = path.as_os_str().encode_wide().collect();
        wide_path.push(0);
        // SAFETY: the path is NUL-terminated and `dacl` points into the live
        // descriptor.  No owner/group/SACL changes are requested.
        let status = unsafe {
            SetNamedSecurityInfoW(
                PCWSTR(wide_path.as_ptr()),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                None,
                None,
                Some(dacl.cast_const()),
                None,
            )
        };
        if status.0 != 0 {
            return Err(format!(
                "could not protect Windows credential file (error {})",
                status.0
            ));
        }

        let mut actual = PSECURITY_DESCRIPTOR::default();
        // SAFETY: all output pointers are valid for this call; Windows owns
        // the returned descriptor until the matching LocalFree below.
        let status = unsafe {
            GetNamedSecurityInfoW(
                PCWSTR(wide_path.as_ptr()),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                None,
                None,
                None,
                None,
                &mut actual,
            )
        };
        if status.0 != 0 {
            return Err(format!(
                "could not verify Windows credential ACL (error {})",
                status.0
            ));
        }
        let mut control = 0_u16;
        let mut revision = 0_u32;
        // SAFETY: `actual` is the live descriptor returned by
        // GetNamedSecurityInfoW above, and both output pointers are valid.
        unsafe {
            windows::Win32::Security::GetSecurityDescriptorControl(
                actual,
                &mut control,
                &mut revision,
            )
            .map_err(|error| {
                format!("could not inspect Windows credential ACL control: {error}")
            })?;
        }
        if control & SE_DACL_PROTECTED.0 == 0 {
            return Err("Windows credential ACL verification found inherited entries".to_string());
        }
        let verified = (|| {
            let mut rendered = windows::core::PWSTR::null();
            // SAFETY: `actual` is live until LocalFree and Windows allocates
            // `rendered`, which is freed before returning from this closure.
            unsafe {
                ConvertSecurityDescriptorToStringSecurityDescriptorW(
                    actual,
                    SDDL_REVISION_1,
                    DACL_SECURITY_INFORMATION,
                    &mut rendered,
                    None,
                )
                .map_err(|error| format!("could not render Windows credential ACL: {error}"))?;
            }
            let rendered_acl = unsafe { rendered.to_string() }
                .map_err(|error| format!("could not decode Windows credential ACL: {error}"));
            // SAFETY: `rendered` was allocated by the documented Windows API.
            unsafe { LocalFree(Some(HLOCAL(rendered.0.cast()))) };
            let rendered_acl = rendered_acl?;
            if !rendered_acl.contains(OWNER_ONLY_ACE) {
                return Err(
                    "Windows credential ACL verification did not find the owner-only ACE"
                        .to_string(),
                );
            }
            Ok(())
        })();
        // SAFETY: `actual` was allocated by GetNamedSecurityInfoW.
        unsafe { LocalFree(Some(HLOCAL(actual.0))) };
        verified
    })();
    // SAFETY: `descriptor` was allocated by the SDDL conversion API.
    unsafe { LocalFree(Some(HLOCAL(descriptor.0))) };
    result
}
