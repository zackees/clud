//! Windows half of `crate::daemon::login_env`: reading the persisted
//! machine and user environment from the registry.

use std::io;

use winreg::enums::{RegType, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
use winreg::types::FromRegValue;
use winreg::RegKey;

/// Which persisted environment block to read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnvironmentKey {
    Machine,
    User,
}

const MACHINE_PATH: &str = r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment";
const USER_PATH: &str = r"Environment";

/// The key's `REG_SZ` / `REG_EXPAND_SZ` values as `(name, raw value)`, in
/// registry enumeration order and unexpanded. Other value types are skipped.
pub fn read_environment_key(which: EnvironmentKey) -> io::Result<Vec<(String, String)>> {
    let key = match which {
        EnvironmentKey::Machine => RegKey::predef(HKEY_LOCAL_MACHINE).open_subkey(MACHINE_PATH)?,
        EnvironmentKey::User => RegKey::predef(HKEY_CURRENT_USER).open_subkey(USER_PATH)?,
    };
    let mut values = Vec::new();
    for item in key.enum_values() {
        let (name, value) = item?;
        if !matches!(value.vtype, RegType::REG_SZ | RegType::REG_EXPAND_SZ) {
            continue;
        }
        let raw = String::from_reg_value(&value)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        values.push((name, raw));
    }
    Ok(values)
}
