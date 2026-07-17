//! Quoted-printable decoder that reproduces real-world browser leniency
//! (bug-compatibility over RFC 2045 strictness): an `=` with fewer than two
//! trailing bytes, or followed by non-hex, is emitted verbatim rather than
//! rejected.
//!
/// Decode quoted-printable `data`. Infallible: malformed escapes pass through
/// literally.
pub(crate) fn decode(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < data.len() {
        let c = data[i];
        if c != b'=' {
            out.push(c);
            i += 1;
            continue;
        }
        // A '=xx' sequence requires at least three bytes *total* from
        // the '='; otherwise the '=' is literal and the trailing 1-2 bytes are
        // reconsidered on the next iterations (they are NOT consumed here).
        if data.len() - i < 3 {
            out.push(c);
            i += 1;
            continue;
        }
        let upper = data[i + 1];
        let lower = data[i + 2];
        i += 3;
        if upper == b'\r' && lower == b'\n' {
            // Soft line break — emit nothing.
            continue;
        }
        match (hex_value(upper), hex_value(lower)) {
            (Some(u), Some(l)) => out.push((u << 4) | l),
            // Invalid: '=' followed by non-hex — emit all three bytes verbatim.
            _ => {
                out.push(b'=');
                out.push(upper);
                out.push(lower);
            }
        }
    }
    out
}

fn hex_value(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_hex_escape() {
        assert_eq!(decode(b"=3D"), b"=");
    }

    #[test]
    fn soft_line_break_emits_nothing_and_joins_lines() {
        assert_eq!(decode(b"foo=\r\nbar"), b"foobar");
    }

    #[test]
    fn non_hex_after_equals_passes_through_verbatim() {
        assert_eq!(decode(b"=Z9"), b"=Z9");
    }

    #[test]
    fn trailing_equals_alone_is_literal() {
        assert_eq!(decode(b"abc="), b"abc=");
    }

    #[test]
    fn trailing_equals_with_one_byte_is_literal_then_byte() {
        // "=A": only two bytes total from '=', so '=' is literal and 'A' is
        // reconsidered normally (copied verbatim).
        assert_eq!(decode(b"=A"), b"=A");
    }

    #[test]
    fn lowercase_hex_is_decoded() {
        assert_eq!(decode(b"=3d"), b"=");
    }

    #[test]
    fn empty_input_yields_empty_output() {
        assert_eq!(decode(b""), b"");
    }

    #[test]
    fn golden_mixed_content_matches_expectation() {
        assert_eq!(
            decode(b"long line=3Dbar=3D=\r\nmore\r\n"),
            b"long line=bar=more\r\n"
        );
    }
}
