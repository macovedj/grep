// This file is part of the uutils grep package.
//
// For the full copyright and license information, please view the LICENSE
// file that was distributed with this source code.

//! Translate GNU BRE/ERE syntax without letting Rust regex extensions leak in.
//!
//! This is a syntax adapter for the experimental backend, not a locale
//! implementation. POSIX bracket classes currently have Rust's ASCII meaning;
//! Unicode word classes have Rust's Unicode meaning. Collating symbols and
//! equivalence classes are rejected rather than silently treated as literals.

use uucore::error::{UResult, USimpleError};

fn invalid(message: &str) -> Box<dyn uucore::error::UError> {
    USimpleError::new(
        2,
        format!("invalid pattern for experimental Rust backend: {message}"),
    )
}

fn literal(output: &mut String, ch: char) {
    if matches!(
        ch,
        '\\' | '.' | '+' | '*' | '?' | '(' | ')' | '|' | '[' | ']' | '{' | '}' | '^' | '$'
    ) {
        output.push('\\');
    }
    output.push(ch);
}

fn repeat(
    output: &mut String,
    atom_start: Option<usize>,
    repeated: &mut bool,
    quantifier: &str,
) -> UResult<()> {
    let start = atom_start.ok_or_else(|| invalid("repetition operator has no preceding atom"))?;
    if *repeated {
        output.insert_str(start, "(?:");
        output.push(')');
    }
    output.push_str(quantifier);
    *repeated = true;
    Ok(())
}

/// Return a normalized interval and the index of its final closing character.
/// ERE leaves an unmatched opening brace literal, while BRE requires its
/// escaped closing brace. Neither mode passes Rust-only interval syntax on.
fn interval(chars: &[char], start: usize, basic: bool) -> UResult<Option<(String, usize)>> {
    let mut end = start + 1;
    while end < chars.len() {
        if (!basic && chars[end] == '}')
            || (basic && chars[end] == '\\' && chars.get(end + 1) == Some(&'}'))
        {
            let content: String = chars[start + 1..end].iter().collect();
            if !basic && content.chars().any(|ch| !ch.is_ascii_digit() && ch != ',') {
                return Ok(None);
            }
            let mut parts = content.split(',');
            let lower = parts.next().unwrap_or("");
            let upper = parts.next();
            let valid = parts.next().is_none()
                && lower.chars().all(|c| c.is_ascii_digit())
                && upper.is_none_or(|value| value.chars().all(|c| c.is_ascii_digit()))
                && (!lower.is_empty() || upper.is_some());
            if !valid {
                return Err(invalid("invalid interval contents"));
            }
            let lower = decimal(lower);
            let upper = upper.map(|value| {
                if value.is_empty() {
                    value
                } else {
                    decimal(value)
                }
            });
            if upper.is_some_and(|upper| !upper.is_empty() && decimal_greater(lower, upper)) {
                return Err(invalid("Invalid content of \\{\\}"));
            }
            // Compare decimal strings before converting them: large user
            // bounds and long runs of leading zeroes must not overflow an
            // integer or cause a valid small bound to be rejected.
            if decimal_greater(lower, "32767")
                || upper.is_some_and(|upper| !upper.is_empty() && decimal_greater(upper, "32767"))
            {
                return Err(invalid("Regular expression too big"));
            }
            let mut normalized = format!("{{{lower}");
            if let Some(upper) = upper {
                normalized.push(',');
                normalized.push_str(upper);
            }
            normalized.push('}');
            return Ok(Some((normalized, if basic { end + 1 } else { end })));
        }
        end += 1;
    }
    if basic {
        Err(invalid("unmatched interval opening brace"))
    } else {
        Ok(None)
    }
}

fn decimal(value: &str) -> &str {
    let value = value.trim_start_matches('0');
    if value.is_empty() { "0" } else { value }
}

fn decimal_greater(left: &str, right: &str) -> bool {
    left.len() > right.len() || (left.len() == right.len() && left > right)
}

fn bracket(chars: &[char], start: usize, output: &mut String) -> UResult<usize> {
    output.push('[');
    let mut ix = start + 1;
    if chars.get(ix) == Some(&'^') {
        output.push('^');
        ix += 1;
    }
    let first = ix;
    let mut previous_class = false;
    let mut previous_range_end = false;
    let mut range_end_pending = false;
    let mut previous_literal = None;
    while ix < chars.len() {
        let ch = chars[ix];
        if ch == ']' && ix != first {
            let body = &chars[first..ix];
            if body.len() > 2
                && body.first() == Some(&':')
                && body.last() == Some(&':')
                && !body.iter().any(|ch| matches!(ch, '-' | '['))
            {
                return Err(invalid(
                    "character class syntax is [[:space:]], not [:space:]",
                ));
            }
            output.push(']');
            return Ok(ix);
        }
        if ch == '[' {
            match chars.get(ix + 1) {
                Some('.' | '=') => {
                    return Err(invalid(
                        "collating symbols and equivalence classes are not supported",
                    ));
                }
                Some(':') => {
                    if range_end_pending {
                        return Err(invalid("Invalid range end"));
                    }
                    let mut end = ix + 2;
                    while end + 1 < chars.len() && !(chars[end] == ':' && chars[end + 1] == ']') {
                        end += 1;
                    }
                    if end + 1 >= chars.len() {
                        return Err(invalid("unmatched POSIX character class"));
                    }
                    let name: String = chars[ix + 2..end].iter().collect();
                    if !matches!(
                        name.as_str(),
                        "alnum"
                            | "alpha"
                            | "blank"
                            | "cntrl"
                            | "digit"
                            | "graph"
                            | "lower"
                            | "print"
                            | "punct"
                            | "space"
                            | "upper"
                            | "xdigit"
                    ) {
                        return Err(invalid("unknown POSIX character class"));
                    }
                    output.extend(chars[ix..end + 2].iter());
                    ix = end + 2;
                    previous_class = true;
                    previous_range_end = false;
                    previous_literal = None;
                    continue;
                }
                _ => output.push_str("\\["),
            }
        } else if ch == '\\' {
            // Backslash has no escape function inside GNU bracket expressions.
            output.push_str("\\\\");
        } else if matches!(ch, '&' | '~' | ']' | '^') {
            output.push('\\');
            output.push(ch);
        } else if ch == '-' {
            if ix == first || chars.get(ix + 1) == Some(&']') {
                output.push_str("\\x2d");
            } else {
                if previous_class || previous_range_end || chars.get(ix + 1) == Some(&'-') {
                    return Err(invalid("Invalid range end"));
                }
                if previous_literal.is_some_and(|left| left > chars[ix + 1]) {
                    return Err(invalid("Invalid range end"));
                }
                output.push('-');
                range_end_pending = true;
                ix += 1;
                continue;
            }
        } else {
            output.push(ch);
        }
        previous_range_end = range_end_pending;
        range_end_pending = false;
        previous_class = false;
        previous_literal = Some(ch);
        ix += 1;
    }
    Err(invalid("unmatched opening bracket"))
}

#[cfg(test)]
mod bracket_helper_tests {
    use super::bracket;

    #[test]
    fn quotes_gnu_bracket_literals_for_the_rust_engine() {
        for (pattern, expected) in [
            (r"[\d]", r"[\\d]"),
            (r"[a&&b~~c]", r"[a\&\&b\~\~c]"),
            (r"[[]", r"[\[]"),
            (r"[]a]", r"[\]a]"),
            (r"[^]a]", r"[^\]a]"),
            (r"[[:alpha:]_-]", r"[[:alpha:]_\x2d]"),
            (r"[--a]", r"[\x2d-a]"),
        ] {
            let chars: Vec<char> = pattern.chars().collect();
            let mut output = String::new();
            let end = bracket(&chars, 0, &mut output).expect(pattern);
            assert_eq!(end, chars.len() - 1, "{pattern}");
            assert_eq!(output, expected, "{pattern}");
        }
    }

    #[test]
    fn stops_at_the_closing_bracket_and_preserves_existing_output() {
        let chars: Vec<char> = "[a-z]tail".chars().collect();
        let mut output = String::from("prefix");
        assert_eq!(bracket(&chars, 0, &mut output).expect("range"), 4);
        assert_eq!(output, "prefix[a-z]");
    }

    #[test]
    fn reports_unsupported_classes_and_invalid_ranges() {
        for pattern in [
            "[abc",
            "[[:bad:]]",
            "[[:alpha]",
            "[[.a.]]",
            "[[=a=]]",
            "[a--b]",
            "[a-b-c]",
            "[[:alpha:]-z]",
            "[a-[:alpha:]]",
        ] {
            let chars: Vec<char> = pattern.chars().collect();
            let error = bracket(&chars, 0, &mut String::new()).expect_err(pattern);
            assert_eq!(error.code(), 2, "{pattern}");
        }
    }
}

#[cfg(test)]
mod repetition_helper_tests {
    use super::{interval, literal, repeat};

    #[test]
    fn interval_bounds_are_normalized_in_both_dialects() {
        for (pattern, basic, expected) in [
            ("{,2}", false, "{0,2}"),
            (r"\{,2\}", true, "{0,2}"),
            ("{,}", false, "{0,}"),
            ("{32767,}", false, "{32767,}"),
            (
                "{0000000000000000000000002,0000000000000000000000003}",
                false,
                "{2,3}",
            ),
        ] {
            let chars: Vec<char> = pattern.chars().collect();
            assert_eq!(
                interval(&chars, usize::from(basic), basic).expect(pattern),
                Some((expected.to_owned(), chars.len() - 1)),
                "{pattern}"
            );
        }
        let chars: Vec<char> = "{2}tail".chars().collect();
        assert_eq!(
            interval(&chars, 0, false).expect("interval"),
            Some(("{2}".to_owned(), 2))
        );
    }

    #[test]
    fn ere_noninterval_braces_remain_literal_but_bre_requires_a_close() {
        for pattern in ["{b}", "{2"] {
            let chars: Vec<char> = pattern.chars().collect();
            assert_eq!(interval(&chars, 0, false).expect(pattern), None);
        }
        let chars: Vec<char> = r"\{2".chars().collect();
        let error = interval(&chars, 1, true).expect_err("unmatched BRE interval");
        assert_eq!(error.code(), 2);
        assert!(
            error
                .to_string()
                .contains("unmatched interval opening brace")
        );
    }

    #[test]
    fn interval_bounds_fail_before_any_integer_conversion() {
        for (pattern, diagnostic) in [
            ("{2,1}", "Invalid content of \\{\\}"),
            (
                "{99999999999999999999999999999999,1}",
                "Invalid content of \\{\\}",
            ),
            ("{32768}", "Regular expression too big"),
            (
                "{1,99999999999999999999999999999999}",
                "Regular expression too big",
            ),
            ("{}", "invalid interval contents"),
        ] {
            let chars: Vec<char> = pattern.chars().collect();
            let error = interval(&chars, 0, false).expect_err(pattern);
            assert_eq!(error.code(), 2);
            assert!(error.to_string().contains(diagnostic), "{pattern}: {error}");
        }
    }

    #[test]
    fn repeated_quantifiers_are_nested_at_the_atom_byte_offset() {
        let mut output = String::from("éa");
        let mut repeated = false;
        repeat(&mut output, Some("é".len()), &mut repeated, "+").expect("first repeat");
        repeat(&mut output, Some("é".len()), &mut repeated, "?").expect("second repeat");
        assert_eq!(output, "é(?:a+)?");
        assert!(repeated);
        let error = repeat(&mut output, None, &mut repeated, "*").expect_err("missing atom");
        assert_eq!(error.code(), 2);
    }

    #[test]
    fn fixed_literals_do_not_acquire_rust_metacharacters() {
        let mut output = String::new();
        for ch in r"a[\d]+?(b){2}^$".chars() {
            literal(&mut output, ch);
        }
        assert_eq!(output, r"a\[\\d\]\+\?\(b\)\{2\}\^\$");
    }
}
