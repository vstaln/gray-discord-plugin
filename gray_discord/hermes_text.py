# Copyright (c) 2025 Nous Research. MIT; see LICENSE and THIRD_PARTY_NOTICES.md.
# The two UTF-16 helpers below are copied from Hermes gateway/platforms/base.py.
def utf16_len(s: str) -> int:
    """Count UTF-16 code units in *s*.

    Telegram's message-length limit (4 096) is measured in UTF-16 code units,
    **not** Unicode code-points.  Characters outside the Basic Multilingual
    Plane (emoji like 😀, CJK Extension B, musical symbols, …) are encoded as
    surrogate pairs and therefore consume **two** UTF-16 code units each, even
    though Python's ``len()`` counts them as one.

    Ported from nearai/ironclaw#2304 which discovered the same discrepancy in
    Rust's ``chars().count()``.
    """
    return len(s.encode("utf-16-le")) // 2


def _prefix_within_utf16_limit(s: str, limit: int) -> str:
    """Return the longest prefix of *s* whose UTF-16 length ≤ *limit*.

    Unlike a plain ``s[:limit]``, this respects surrogate-pair boundaries so
    we never slice a multi-code-unit character in half.
    """
    if utf16_len(s) <= limit:
        return s
    # Binary search for the longest safe prefix
    lo, hi = 0, len(s)
    while lo < hi:
        mid = (lo + hi + 1) // 2
        if utf16_len(s[:mid]) <= limit:
            lo = mid
        else:
            hi = mid - 1
    return s[:lo]



def split_message(text, limit=2000):
    """Conservative UTF-16 chunks, preserving every character."""
    if limit < 2:
        raise ValueError('Message limit must be at least two UTF-16 units')
    chunks = []
    while text:
        prefix = _prefix_within_utf16_limit(text, limit)
        chunks.append(prefix)
        text = text[len(prefix):]
    return chunks
