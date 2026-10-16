//! Windows half of `crate::trampoline::relay_child_and_wait`.
//!
//! Raw `std::process::Command` is deliberate here, exactly as in the neutral
//! `trampoline.rs` (see its `relay_child_and_wait` docs, #333); both files
//! share the `trampoline.rs` exemption in `ci/banned_imports.py`.

/// Run the prepared relay `command` and return its exit code.
pub fn relay_status(command: &mut std::process::Command) -> std::io::Result<i32> {
    // The cached child needs the caller's visible stdio, but its detached
    // descendants must not inherit the caller's pipe handles. Pass owned
    // inheritable duplicates explicitly and make the originals
    // non-inheritable for the CreateProcess call.
    let [stdin, stdout, stderr] = windows_stdio::duplicate_stdio_for_child()?;
    command.stdin(stdin).stdout(stdout).stderr(stderr);
    let _guard = windows_stdio::NonInheritableStdioGuard::install();
    let status = command.status()?;
    Ok(status.code().unwrap_or(1))
}

mod windows_stdio {
    //! RAII guard that strips `HANDLE_FLAG_INHERIT` from our three standard
    //! handles for the lifetime of the guard, restoring the original flags
    //! on drop. Used by `relay_child_and_wait` so the relayed clud's detached
    //! descendants don't inherit parent stdio pipes — see the module doc of
    //! the parent file.

    const HANDLE_FLAG_INHERIT: u32 = 0x0001;
    // Windows STD_*_HANDLE values are `((DWORD)-N)`; in Rust const context
    // the `as u32` cast on a negative i32 produces the matching bit pattern.
    const STD_INPUT_HANDLE: u32 = -10i32 as u32;
    const STD_OUTPUT_HANDLE: u32 = -11i32 as u32;
    const STD_ERROR_HANDLE: u32 = -12i32 as u32;
    const INVALID_HANDLE_VALUE: isize = -1;
    const DUPLICATE_SAME_ACCESS: u32 = 0x0000_0002;

    extern "system" {
        fn GetStdHandle(n_std_handle: u32) -> isize;
        fn GetHandleInformation(handle: isize, flags: *mut u32) -> i32;
        fn SetHandleInformation(handle: isize, mask: u32, flags: u32) -> i32;
        fn GetCurrentProcess() -> isize;
        fn DuplicateHandle(
            source_process_handle: isize,
            source_handle: isize,
            target_process_handle: isize,
            target_handle: *mut isize,
            desired_access: u32,
            inherit_handle: i32,
            options: u32,
        ) -> i32;
    }

    /// Duplicate the three standard handles for the relay's cached child.
    ///
    /// The copies deliberately start non-inheritable: `std::process::Command`
    /// creates the one inheritable copy it needs for each STARTUPINFO standard
    /// slot. Making these copies inheritable here would also leak them as
    /// ordinary handles to every descendant. See [`crate::trampoline::relay_child_and_wait`].
    pub(super) fn duplicate_stdio_for_child() -> std::io::Result<[std::process::Stdio; 3]> {
        use std::os::windows::io::FromRawHandle;

        let ids = [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE];
        let mut duplicated = Vec::with_capacity(ids.len());
        for std_id in ids {
            let source = unsafe { GetStdHandle(std_id) };
            if source == 0 || source == INVALID_HANDLE_VALUE {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "standard handle unavailable for runtime-cache relay",
                ));
            }

            let mut target = 0isize;
            let copied = unsafe {
                DuplicateHandle(
                    GetCurrentProcess(),
                    source,
                    GetCurrentProcess(),
                    &mut target,
                    0,
                    0,
                    DUPLICATE_SAME_ACCESS,
                )
            };
            if copied == 0 {
                return Err(std::io::Error::last_os_error());
            }
            // SAFETY: DuplicateHandle returned a fresh owned handle. Stdio
            // takes ownership through File and closes it after CreateProcess.
            let file = unsafe { std::fs::File::from_raw_handle(target as _) };
            duplicated.push(std::process::Stdio::from(file));
        }

        let mut handles = duplicated.into_iter();
        Ok([
            handles.next().expect("stdin duplicate exists"),
            handles.next().expect("stdout duplicate exists"),
            handles.next().expect("stderr duplicate exists"),
        ])
    }

    pub(super) struct NonInheritableStdioGuard {
        saved: [Option<(isize, u32)>; 3],
    }

    impl NonInheritableStdioGuard {
        pub(super) fn install() -> Self {
            let ids = [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE];
            let mut saved: [Option<(isize, u32)>; 3] = [None, None, None];
            for (i, std_id) in ids.iter().enumerate() {
                unsafe {
                    let h = GetStdHandle(*std_id);
                    if h == 0 || h == INVALID_HANDLE_VALUE {
                        continue;
                    }
                    let mut flags = 0u32;
                    if GetHandleInformation(h, &mut flags) == 0 {
                        continue;
                    }
                    if flags & HANDLE_FLAG_INHERIT == 0 {
                        // Already non-inheritable; nothing to do.
                        continue;
                    }
                    if SetHandleInformation(h, HANDLE_FLAG_INHERIT, 0) != 0 {
                        saved[i] = Some((h, flags));
                    }
                }
            }
            Self { saved }
        }
    }

    impl Drop for NonInheritableStdioGuard {
        fn drop(&mut self) {
            for item in &self.saved {
                if let Some((h, flags)) = *item {
                    unsafe {
                        SetHandleInformation(h, HANDLE_FLAG_INHERIT, flags & HANDLE_FLAG_INHERIT);
                    }
                }
            }
        }
    }
}
