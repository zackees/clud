use std::time::Duration;

/// Parse a `--stale-after` value like `1d`, `2h`, `30m`, `45s`.
///
/// Accepted units: `s` (seconds), `m` (minutes), `h` (hours), `d` (days).
/// The value must be a positive integer. Whitespace around the input is
/// trimmed. Unit characters are case-insensitive (`30M` == `30m`).
pub fn parse_duration(raw: &str) -> Result<Duration, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("duration cannot be empty".to_string());
    }
    let split_at = trimmed
        .find(|c: char| !c.is_ascii_digit())
        .ok_or_else(|| "duration must include a unit like s, m, h, or d".to_string())?;
    if split_at == 0 {
        return Err("duration must start with a positive integer".to_string());
    }
    let (num_part, unit_part) = trimmed.split_at(split_at);
    let n: u64 = num_part
        .parse()
        .map_err(|_| format!("invalid duration value: {num_part}"))?;
    if n == 0 {
        return Err("duration must be greater than zero".to_string());
    }
    let unit = unit_part.trim().to_ascii_lowercase();
    let secs_multiplier: u64 = match unit.as_str() {
        "s" => 1,
        "m" => 60,
        "h" => 60 * 60,
        "d" => 60 * 60 * 24,
        _ => return Err(format!("unsupported duration unit: {unit_part}")),
    };
    let total_secs = n
        .checked_mul(secs_multiplier)
        .ok_or_else(|| "duration is too large".to_string())?;
    Ok(Duration::from_secs(total_secs))
}
