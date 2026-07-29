//! HTTP Range parsing (RFC 7233 §2.1, §3.1, §4.4).
//!
//! Kept as a **pure function** — no I/O, no HTTP types — because the watch's whole
//! playback path rests on it. A chapter's `.pcm` is ~25 MB against 512 KB of RAM, so
//! the watch never holds a file: it walks the chapter in ranges taken straight from
//! the manifest. An off-by-one here is a click or a skipped syllable on glass, which
//! is exactly the class of bug that is miserable to chase from firmware. Pure input
//! → output means every edge case below is a cheap table-driven test.

/// What the server should do about a request's `Range` header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeOutcome {
    /// Serve the entire representation with `200 OK`.
    ///
    /// Also the answer for a *malformed* `Range`: RFC 7233 §3.1 says a recipient
    /// MUST ignore a Range header it cannot understand, rather than reject it.
    Full,
    /// Serve `206 Partial Content`. Both bounds are inclusive, matching the wire
    /// format of `Content-Range` so the handler does no further arithmetic.
    Partial { start: u64, end: u64 },
    /// Serve `416 Range Not Satisfiable` with `Content-Range: bytes */total`.
    Unsatisfiable,
}

impl RangeOutcome {
    /// Number of bytes the response body will contain.
    pub fn content_length(&self, total: u64) -> u64 {
        match self {
            Self::Full => total,
            Self::Partial { start, end } => end.saturating_sub(*start) + 1,
            Self::Unsatisfiable => 0,
        }
    }
}

/// Parse a `Range` header value against a known total size.
///
/// Deliberate choices, each with a reason rather than a preference:
///
/// * **Malformed input degrades to [`RangeOutcome::Full`]**, never to an error. Per
///   RFC 7233 §3.1 an unparsable Range must be ignored. Serving the whole body is
///   always a correct answer; a 4xx would strand a client over a header typo.
/// * **Multi-range requests degrade to `Full`.** A single `206` answering a
///   two-range request is a protocol violation — the client is entitled to a
///   `multipart/byteranges` body it would then fail to parse. We do not implement
///   multipart, and the watch only ever sends one range, so `200` is the honest
///   answer instead of a subtly wrong `206`.
/// * **`start > end` is treated as malformed, not unsatisfiable.** The RFC grammar
///   requires `first-byte-pos <= last-byte-pos`, so `bytes=500-100` fails to parse
///   as a valid byte-range-spec and is ignored — it is not a satisfiable-range
///   question at all.
pub fn parse_range(header: Option<&str>, total: u64) -> RangeOutcome {
    let Some(raw) = header else {
        return RangeOutcome::Full;
    };
    let raw = raw.trim();

    // Only the `bytes` unit exists in practice; anything else we must ignore.
    let Some(spec) = raw
        .strip_prefix("bytes=")
        .or_else(|| raw.strip_prefix("bytes ="))
    else {
        return RangeOutcome::Full;
    };

    // We answer exactly one range or none at all.
    if spec.contains(',') {
        return RangeOutcome::Full;
    }

    let spec = spec.trim();
    let Some((first, last)) = spec.split_once('-') else {
        return RangeOutcome::Full;
    };
    let (first, last) = (first.trim(), last.trim());

    // A zero-length representation cannot satisfy any range. Checked after parsing
    // so that a malformed header on an empty file still degrades to `Full` (200 with
    // an empty body) rather than 416.
    match (first.is_empty(), last.is_empty()) {
        // `bytes=-N` — the final N bytes (suffix range).
        (true, false) => {
            let Ok(n) = last.parse::<u64>() else {
                return RangeOutcome::Full;
            };
            // `bytes=-0` asks for the last zero bytes: satisfiable by nothing.
            if n == 0 || total == 0 {
                return RangeOutcome::Unsatisfiable;
            }
            // A suffix longer than the file is *not* an error — it clamps to the
            // whole file (RFC 7233 §2.1).
            let start = total.saturating_sub(n);
            RangeOutcome::Partial {
                start,
                end: total - 1,
            }
        }
        // `bytes=N-` — from N to the end (open-ended).
        (false, true) => {
            let Ok(start) = first.parse::<u64>() else {
                return RangeOutcome::Full;
            };
            if total == 0 || start >= total {
                return RangeOutcome::Unsatisfiable;
            }
            RangeOutcome::Partial {
                start,
                end: total - 1,
            }
        }
        // `bytes=A-B` — an explicit closed interval.
        (false, false) => {
            let (Ok(start), Ok(end)) = (first.parse::<u64>(), last.parse::<u64>()) else {
                return RangeOutcome::Full;
            };
            if start > end {
                // Invalid per the grammar, so ignore rather than 416.
                return RangeOutcome::Full;
            }
            if total == 0 || start >= total {
                return RangeOutcome::Unsatisfiable;
            }
            // Clamp an over-long end to the last byte instead of failing: asking for
            // more than exists past a valid start is satisfiable.
            RangeOutcome::Partial {
                start,
                end: end.min(total - 1),
            }
        }
        // `bytes=-` — no bounds at all.
        (true, true) => RangeOutcome::Full,
    }
}

/// The `Content-Range` value for a `206`: `bytes start-end/total`.
pub fn content_range(start: u64, end: u64, total: u64) -> String {
    format!("bytes {start}-{end}/{total}")
}

/// The `Content-Range` value for a `416`: `bytes */total`.
pub fn content_range_unsatisfied(total: u64) -> String {
    format!("bytes */{total}")
}
