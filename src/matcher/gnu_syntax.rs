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
