//! Human-readable formatting helpers for the CLI.

pub fn bytes(value: u64) -> String {
    const KIB: f64 = 1024.0;
    let value_f = value as f64;
    if value_f >= KIB * KIB * KIB {
        format!("{:.2} GiB", value_f / (KIB * KIB * KIB))
    } else if value_f >= KIB * KIB {
        format!("{:.2} MiB", value_f / (KIB * KIB))
    } else if value_f >= KIB {
        format!("{:.2} KiB", value_f / KIB)
    } else {
        format!("{value} B")
    }
}

pub fn number(value: u64) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, ch) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

pub fn duration(seconds: f64) -> String {
    if !seconds.is_finite() || seconds < 0.0 {
        return "0:00.000".to_string();
    }
    let total_ms = (seconds * 1000.0).round() as u64;
    let ms = total_ms % 1000;
    let total_seconds = total_ms / 1000;
    let s = total_seconds % 60;
    let total_minutes = total_seconds / 60;
    let m = total_minutes % 60;
    let h = total_minutes / 60;
    if h > 0 {
        format!("{h}:{m:02}:{s:02}.{ms:03}")
    } else {
        format!("{m}:{s:02}.{ms:03}")
    }
}

pub fn channel_layout(channels: u16, mask: u32) -> String {
    let base = match channels {
        1 => "mono".to_string(),
        2 => "stereo".to_string(),
        3 => "3 channels".to_string(),
        4 => "4 channels".to_string(),
        5 => "5 channels".to_string(),
        6 => "5.1".to_string(),
        8 => "7.1".to_string(),
        n => format!("{n} channels"),
    };
    if mask != 0 && !matches!(channels, 1 | 2 | 6) {
        format!("{base} (mask 0x{mask:08x})")
    } else {
        base
    }
}

pub fn throughput(bytes: u64, seconds: f64) -> String {
    if seconds <= 0.0 {
        return "-".to_string();
    }
    format!("{:.1} MB/s", bytes as f64 / seconds / 1_000_000.0)
}
