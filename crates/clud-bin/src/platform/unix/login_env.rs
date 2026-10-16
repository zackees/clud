//! Unix half of `crate::daemon::login_env`: there is no registry; the login
//! environment comes from the account's login shell instead.

use std::io;

/// Which persisted environment block to read (a Windows registry concept).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnvironmentKey {
    Machine,
    User,
}

/// Never called off Windows: `daemon::login_env` only reads the registry
/// when `platform::host::WINDOWS` holds.
pub fn read_environment_key(_which: EnvironmentKey) -> io::Result<Vec<(String, String)>> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "the registry environment is Windows-only",
    ))
}
