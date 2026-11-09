//! Small formatting helpers for the UI.

use wasm_bindgen::JsValue;

/// Human-readable byte count: `6 B`, `1.4 kB`, `12.5 MB`.
pub fn human_size(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    let bytes_f = bytes as f64;
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes_f < KIB * KIB {
        format!("{:.1} kB", bytes_f / KIB)
    } else if bytes_f < KIB * KIB * KIB {
        format!("{:.1} MB", bytes_f / (KIB * KIB))
    } else {
        format!("{:.1} GB", bytes_f / (KIB * KIB * KIB))
    }
}

/// Unix milliseconds as `YYYY-MM-DD HH:MM`, in the browser's local time.
pub fn format_millis(millis: i64) -> String {
    let date = js_sys::Date::new(&JsValue::from_f64(millis as f64));
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}",
        date.get_full_year(),
        date.get_month() + 1,
        date.get_date(),
        date.get_hours(),
        date.get_minutes()
    )
}

/// Percent-encode a value for use in a query string.
///
/// Deliberately encodes `/` as well: paths travel as a single query *value*, and
/// the server decodes them back before applying its own path rules, so encoding
/// more than strictly necessary is the safer default.
pub fn encode_query(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(1023), "1023 B");
        assert_eq!(human_size(2048), "2.0 kB");
        assert_eq!(human_size(5 * 1024 * 1024), "5.0 MB");
    }

    #[test]
    fn encoding_separators_and_spaces() {
        assert_eq!(encode_query("notes/todo.md"), "notes%2Ftodo.md");
        assert_eq!(encode_query("a b&c=d"), "a%20b%26c%3Dd");
        assert_eq!(encode_query("keep-._~"), "keep-._~");
    }
}
