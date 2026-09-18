//! Port of gray_discord/hermes_text.py: UTF-16-safe message splitting.
//! Discord counts message length in UTF-16 code units, not code points.
/// Count UTF-16 code units in `s` (surrogate pairs count as two).
pub fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

/// Split `s` into chunks of at most `limit` UTF-16 units, never slicing a
/// character in half. Returns every character exactly once, in order.
pub fn split_message(s: &str, limit: usize) -> Result<Vec<String>, String> {
    if limit < 2 {
        return Err("Message limit must be at least two UTF-16 units".to_string());
    }
    let mut chunks = Vec::new();
    let mut rest = s;
    while !rest.is_empty() {
        let prefix = prefix_within_limit(rest, limit);
        debug_assert!(!prefix.is_empty(), "limit >= 2 always fits one char");
        chunks.push(prefix.to_string());
        rest = &rest[prefix.len()..];
    }
    Ok(chunks)
}

/// Longest prefix of `s` (on a char boundary) within `limit` UTF-16 units.
fn prefix_within_limit(s: &str, limit: usize) -> &str {
    if utf16_len(s) <= limit {
        return s;
    }
    let mut used = 0usize;
    let mut end = 0usize;
    for (i, c) in s.char_indices() {
        let w = c.len_utf16();
        if used + w > limit {
            break;
        }
        used += w;
        end = i + c.len_utf8();
    }
    // Elide the allocation the Python version needed for its binary search;
    // char indices give the same boundary directly. Guard against a
    // zero-width result only to keep the caller's loop total.
    &s[..end]
}
