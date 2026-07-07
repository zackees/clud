use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::args::TopSort;

use super::types::{ProcRow, ProcTier, ProcTreeSnapshot};

pub(super) fn parse_since_ms(input: Option<&str>) -> Result<u64, String> {
    let Some(raw) = input else {
        return Ok(0);
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("--since cannot be empty".to_string());
    }
    let split_at = trimmed
        .find(|ch: char| !ch.is_ascii_digit())
        .unwrap_or(trimmed.len());
    let (number, suffix) = trimmed.split_at(split_at);
    if number.is_empty() {
        return Err(format!("invalid --since duration: {raw}"));
    }
    let value = number
        .parse::<u64>()
        .map_err(|_| format!("invalid --since duration: {raw}"))?;
    let multiplier = match suffix.trim().to_ascii_lowercase().as_str() {
        "" | "ms" => 1,
        "s" | "sec" | "secs" => 1_000,
        "m" | "min" | "mins" => 60_000,
        "h" | "hr" | "hrs" => 60 * 60_000,
        "d" | "day" | "days" => 24 * 60 * 60_000,
        _ => return Err(format!("invalid --since duration suffix: {raw}")),
    };
    value
        .checked_mul(multiplier)
        .ok_or_else(|| format!("--since duration is too large: {raw}"))
}

pub(super) fn normalize_originator(input: Option<&str>) -> Option<String> {
    let raw = input?.trim();
    if raw.is_empty() {
        return None;
    }
    if raw.contains(':') {
        Some(raw.to_string())
    } else {
        Some(format!("CLUD:{raw}"))
    }
}

pub(super) fn prepare_snapshot(
    mut snapshot: ProcTreeSnapshot,
    sort: TopSort,
    flat: bool,
    limit: usize,
    originator: Option<&str>,
) -> ProcTreeSnapshot {
    if let Some(originator) = normalize_originator(originator) {
        snapshot.rows.retain(|row| row.originator == originator);
    }

    if flat {
        sort_rows(&mut snapshot.rows, sort);
        truncate_if_limited(&mut snapshot.rows, limit);
    } else {
        let mut ordered = Vec::new();
        for (_originator, rows) in grouped_rows(&snapshot.rows) {
            ordered.extend(order_tree_rows(&rows, sort, limit));
        }
        snapshot.rows = ordered;
    }
    snapshot.recompute_summary();
    snapshot
}

pub(super) fn render_snapshot(
    snapshot: &ProcTreeSnapshot,
    sort: TopSort,
    flat: bool,
    limit: usize,
    originator: Option<&str>,
) -> String {
    let snapshot = prepare_snapshot(snapshot.clone(), sort, flat, limit, originator);
    let mut out = String::new();
    out.push_str(&format!(
        "clud top - sample {} ms old - {} procs - {} cohorts - cpu {:.1}% - rss {}\n",
        snapshot.sample_age_ms,
        snapshot.summary.process_count,
        snapshot.summary.originator_count,
        snapshot.summary.total_cpu_pct,
        format_bytes(snapshot.summary.total_rss_bytes),
    ));
    if snapshot.rows.is_empty() {
        out.push_str("No clud-rooted processes currently sampled.\n");
        return out;
    }

    if flat {
        out.push_str(
            "     PID     PPID ORIGIN          CPU%   EWMA       RSS     AGE TIER   COMMAND\n",
        );
        for row in &snapshot.rows {
            out.push_str(&format_flat_row(row));
        }
        return out;
    }

    for (originator, rows) in grouped_rows(&snapshot.rows) {
        let total_cpu = rows.iter().map(|row| row.cpu_pct).sum::<f32>();
        let total_rss = rows
            .iter()
            .fold(0_u64, |sum, row| sum.saturating_add(row.rss_bytes));
        let session_label = rows
            .iter()
            .find_map(session_label)
            .map(|label| format!(" ({label})"))
            .unwrap_or_default();
        out.push_str(&format!(
            "\n{originator}{session_label} - {} procs - cpu {:.1}% - rss {}\n",
            rows.len(),
            total_cpu,
            format_bytes(total_rss),
        ));
        out.push_str("     PID     PPID    CPU%   EWMA       RSS     AGE TIER   COMMAND\n");
        for row in rows {
            out.push_str(&format_tree_row(&row));
        }
    }
    out
}

fn grouped_rows(rows: &[ProcRow]) -> BTreeMap<String, Vec<ProcRow>> {
    let mut groups: BTreeMap<String, Vec<ProcRow>> = BTreeMap::new();
    for row in rows {
        groups
            .entry(row.originator.clone())
            .or_default()
            .push(row.clone());
    }
    groups
}

fn sort_rows(rows: &mut [ProcRow], sort: TopSort) {
    rows.sort_by(|left, right| {
        metric(right, sort)
            .partial_cmp(&metric(left, sort))
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(left.pid.cmp(&right.pid))
    });
}

fn order_tree_rows(rows: &[ProcRow], sort: TopSort, limit: usize) -> Vec<ProcRow> {
    let by_pid: HashMap<u32, ProcRow> = rows.iter().map(|row| (row.pid, row.clone())).collect();
    let pids: BTreeSet<u32> = by_pid.keys().copied().collect();
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    let mut roots = Vec::new();
    for row in rows {
        match row.ppid {
            Some(ppid) if pids.contains(&ppid) && ppid != row.pid => {
                children.entry(ppid).or_default().push(row.pid);
            }
            _ => roots.push(row.pid),
        }
    }
    sort_pid_list(&mut roots, &by_pid, sort);
    for child_pids in children.values_mut() {
        sort_pid_list(child_pids, &by_pid, sort);
    }

    let mut ordered = Vec::new();
    for root in roots {
        push_tree(root, &by_pid, &children, sort, limit, &mut ordered);
        if limit > 0 && ordered.len() >= limit {
            break;
        }
    }
    ordered
}

fn sort_pid_list(pids: &mut [u32], by_pid: &HashMap<u32, ProcRow>, sort: TopSort) {
    pids.sort_by(|left, right| {
        let left_row = by_pid.get(left);
        let right_row = by_pid.get(right);
        match (left_row, right_row) {
            (Some(left_row), Some(right_row)) => metric(right_row, sort)
                .partial_cmp(&metric(left_row, sort))
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(left.cmp(right)),
            _ => left.cmp(right),
        }
    });
}

fn push_tree(
    pid: u32,
    by_pid: &HashMap<u32, ProcRow>,
    children: &HashMap<u32, Vec<u32>>,
    sort: TopSort,
    limit: usize,
    out: &mut Vec<ProcRow>,
) {
    if limit > 0 && out.len() >= limit {
        return;
    }
    let Some(row) = by_pid.get(&pid) else {
        return;
    };
    out.push(row.clone());
    if let Some(child_pids) = children.get(&pid) {
        for child in child_pids {
            let _ = sort;
            push_tree(*child, by_pid, children, sort, limit, out);
            if limit > 0 && out.len() >= limit {
                return;
            }
        }
    }
}

fn metric(row: &ProcRow, sort: TopSort) -> f64 {
    match sort {
        TopSort::Cpu => row.cpu_ewma_pct.max(row.cpu_pct) as f64,
        TopSort::Mem | TopSort::Rss => row.rss_bytes as f64,
        TopSort::Age => row.age_secs as f64,
    }
}

fn truncate_if_limited(rows: &mut Vec<ProcRow>, limit: usize) {
    if limit > 0 && rows.len() > limit {
        rows.truncate(limit);
    }
}

fn format_flat_row(row: &ProcRow) -> String {
    format!(
        "{pid:>8} {ppid:>8} {origin:<14} {cpu:>6.1} {ewma:>6.1} {rss:>9} {age:>7} {tier:<6} {cmd}\n",
        pid = row.pid,
        ppid = row.ppid.map(|pid| pid.to_string()).unwrap_or_else(|| "-".to_string()),
        origin = truncate(&row.originator, 14),
        cpu = row.cpu_pct,
        ewma = row.cpu_ewma_pct,
        rss = format_bytes(row.rss_bytes),
        age = format_age(row.age_secs),
        tier = tier_label(row.tier),
        cmd = truncate(&row.command, 88),
    )
}

fn format_tree_row(row: &ProcRow) -> String {
    let indent = "  ".repeat((row.depth as usize).min(16));
    format!(
        "{pid:>8} {ppid:>8} {cpu:>6.1} {ewma:>6.1} {rss:>9} {age:>7} {tier:<6} {indent}{cmd}\n",
        pid = row.pid,
        ppid = row
            .ppid
            .map(|pid| pid.to_string())
            .unwrap_or_else(|| "-".to_string()),
        cpu = row.cpu_pct,
        ewma = row.cpu_ewma_pct,
        rss = format_bytes(row.rss_bytes),
        age = format_age(row.age_secs),
        tier = tier_label(row.tier),
        indent = indent,
        cmd = truncate(&row.command, 88),
    )
}

fn session_label(row: &ProcRow) -> Option<String> {
    match (row.session_name.as_deref(), row.session_id.as_deref()) {
        (Some(name), Some(id)) => Some(format!("{name}/{id}")),
        (Some(name), None) => Some(name.to_string()),
        (None, Some(id)) => Some(id.to_string()),
        (None, None) => None,
    }
}

fn tier_label(tier: ProcTier) -> &'static str {
    match tier {
        ProcTier::Hot => "hot",
        ProcTier::Warm => "warm",
        ProcTier::Cold => "cold",
        ProcTier::Frozen => "dead",
    }
}

fn format_bytes(bytes: u64) -> String {
    let mib = bytes as f64 / (1024.0 * 1024.0);
    let gib = mib / 1024.0;
    if gib >= 1.0 {
        format!("{gib:.2}GiB")
    } else if mib >= 1.0 {
        format!("{mib:.0}MiB")
    } else {
        format!("{}KiB", bytes / 1024)
    }
}

fn format_age(secs: u64) -> String {
    if secs >= 24 * 3600 {
        format!("{}d", secs / (24 * 3600))
    } else if secs >= 3600 {
        format!("{}h", secs / 3600)
    } else if secs >= 60 {
        format!("{}m", secs / 60)
    } else {
        format!("{secs}s")
    }
}

fn truncate(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    let keep = max_chars.saturating_sub(3);
    let mut out: String = value.chars().take(keep).collect();
    out.push_str("...");
    out
}

#[cfg(test)]
mod tests {
    use super::super::types::ProcTreeSummary;
    use super::*;

    fn row(pid: u32, ppid: Option<u32>, cpu: f32, rss: u64, depth: u32) -> ProcRow {
        ProcRow {
            pid,
            ppid,
            originator: "CLUD:1".to_string(),
            originator_pid: Some(1),
            session_id: None,
            session_name: None,
            cpu_pct: cpu,
            cpu_ewma_pct: cpu,
            rss_bytes: rss,
            age_secs: pid as u64,
            command: format!("cmd-{pid}"),
            depth,
            tier: ProcTier::Cold,
            live: true,
            exited_at_ms: None,
        }
    }

    #[test]
    fn parses_since_duration_units() {
        assert_eq!(parse_since_ms(None).unwrap(), 0);
        assert_eq!(parse_since_ms(Some("500ms")).unwrap(), 500);
        assert_eq!(parse_since_ms(Some("2s")).unwrap(), 2_000);
        assert_eq!(parse_since_ms(Some("3m")).unwrap(), 180_000);
        assert!(parse_since_ms(Some("abc")).is_err());
    }

    #[test]
    fn normalizes_bare_originator_pid() {
        assert_eq!(
            normalize_originator(Some("123")).as_deref(),
            Some("CLUD:123")
        );
        assert_eq!(
            normalize_originator(Some("CLUD:123")).as_deref(),
            Some("CLUD:123")
        );
    }

    #[test]
    fn flat_prepare_sorts_and_limits_rows() {
        let snapshot = ProcTreeSnapshot {
            schema_version: 1,
            sampled_at_ms: 1,
            sample_age_ms: 0,
            sampler_pid: 1,
            interval_ms: 2_000,
            rows: vec![row(2, None, 1.0, 10, 0), row(3, None, 9.0, 1, 0)],
            summary: ProcTreeSummary::default(),
        };
        let prepared = prepare_snapshot(snapshot, TopSort::Cpu, true, 1, None);
        assert_eq!(prepared.rows.len(), 1);
        assert_eq!(prepared.rows[0].pid, 3);
    }

    #[test]
    fn tree_prepare_keeps_parent_before_hot_child() {
        let snapshot = ProcTreeSnapshot {
            schema_version: 1,
            sampled_at_ms: 1,
            sample_age_ms: 0,
            sampler_pid: 1,
            interval_ms: 2_000,
            rows: vec![row(10, None, 1.0, 1, 0), row(11, Some(10), 99.0, 1, 1)],
            summary: ProcTreeSummary::default(),
        };
        let prepared = prepare_snapshot(snapshot, TopSort::Cpu, false, 20, None);
        assert_eq!(
            prepared.rows.iter().map(|row| row.pid).collect::<Vec<_>>(),
            vec![10, 11]
        );
    }
}
