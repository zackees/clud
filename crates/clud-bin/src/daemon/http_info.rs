use super::*;

pub type ApiInfo = (u32, Option<u16>, Option<String>, Option<String>);

pub fn read_dashboard_port(state_dir: &Path) -> io::Result<Option<u16>> {
    let info = read_json_file::<DaemonInfo>(&daemon_info_path(state_dir))?;
    Ok(info.dashboard_port)
}

/// Re-export the typed info read by the `clud ui` CLI. Kept narrow so the
/// CLI layer doesn't depend on the (internal) `DaemonInfo` struct.
pub fn read_dashboard_info(state_dir: &Path) -> io::Result<DashboardInfo> {
    let info = read_json_file::<DaemonInfo>(&daemon_info_path(state_dir))?;
    Ok(DashboardInfo {
        pid: info.pid,
        ipc_port: info.port,
        dashboard_port: info.dashboard_port,
        dashboard_token: info.dashboard_token,
    })
}

pub fn read_api_info(state_dir: &Path) -> io::Result<ApiInfo> {
    let info = read_json_file::<DaemonInfo>(&daemon_info_path(state_dir))?;
    Ok((info.pid, info.dashboard_port, info.api_token, info.version))
}
