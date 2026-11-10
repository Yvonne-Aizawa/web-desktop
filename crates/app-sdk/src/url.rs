//! Percent-encoding for query strings, shared by the SDK's request helpers.
//!
//! Kept out of any one feature-gated module so both [`crate::fetch`] and [`crate::fs`]
//! can reach it, and so the one piece of hand-written string handling in the SDK has a
//! test of its own. Building the URL itself stays with the module that knows the shape
//! of its own endpoint's query.

/// Percent-encode everything outside the unreserved set.
///
/// Encodes *every* reserved character rather than taking a shortcut with the few that
/// matter for a query string, so a value that already contains a `?`, a `&` or a `%`
/// arrives intact instead of being read as part of the request.
pub(crate) fn encode(value: &str) -> String {
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
    use super::encode;

    #[test]
    fn encoding_leaves_the_unreserved_set_alone() {
        assert_eq!(encode("aZ0-_.~"), "aZ0-_.~");
        assert_eq!(encode(" "), "%20");
        assert_eq!(encode("/"), "%2F");
    }

    #[test]
    fn encoding_a_path_keeps_its_separators_out_of_the_query() {
        // The fence is the server's job; this only has to prevent a name from being
        // read as a query separator.
        assert_eq!(encode("a b/c?d"), "a%20b%2Fc%3Fd");
    }
}
