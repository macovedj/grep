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

use crate::RegexMode;
use uucore::error::{UResult, USimpleError};
use uucore::show_warning;

struct Group {
    output_start: usize,
    capture: usize,
    available_on_entry: [bool; 10],
}

pub(super) fn translate(pattern: &str, mode: RegexMode) -> UResult<String> {
    if mode == RegexMode::Fixed {
        let mut output = String::with_capacity(pattern.len());
        for ch in pattern.chars() {
            literal(&mut output, ch);
        }
        return Ok(output);
    }
    if mode == RegexMode::Perl {
        return Err(invalid(
            "Perl syntax is not supported by the experimental GNU syntax adapter",
        ));
    }

    let basic = mode == RegexMode::Basic;
    let chars: Vec<char> = pattern.chars().collect();
    let mut output = String::with_capacity(pattern.len());
    let mut groups: Vec<Group> = Vec::new();
    let mut capture_count = 0;
    let mut available_captures = [false; 10];
    let mut branch_start = true;
    // The start of the last consuming atom lets repeated GNU quantifiers be
    // nested explicitly, so a+? cannot become a Rust non-greedy quantifier.
    let mut atom_start = None;
    let mut repeated = false;
    let mut ix = 0;

    while ix < chars.len() {
        let start = output.len();
        let ch = chars[ix];
        let escaped = ch == '\\';
        let token = if escaped {
            ix += 1;
            *chars.get(ix).ok_or_else(|| invalid("trailing backslash"))?
        } else {
            ch
        };

        let special = if basic { escaped } else { !escaped };
        match token {
            '(' if special => {
                // (?...) is not GNU ERE syntax, even when the downstream
                // engine recognizes inline flags, lookarounds or group names.
                if !basic && chars.get(ix + 1) == Some(&'?') {
                    return Err(invalid(
                        "(?...) groups are unsupported in GNU extended mode",
                    ));
                }
                output.push('(');
                capture_count += 1;
                groups.push(Group {
                    output_start: start,
                    capture: capture_count,
                    available_on_entry: available_captures,
                });
                branch_start = true;
                atom_start = None;
                repeated = false;
            }
            ')' if special => {
                let group_start = if let Some(group) = groups.pop() {
                    output.push(')');
                    // Once a group closes, GNU permits references to its
                    // nested captures even when some occurred in another
                    // alternative. While it is open, alternatives retain
                    // only the captures available when the group began.
                    if group.capture <= 9 {
                        available_captures[group.capture..=capture_count.min(9)].fill(true);
                    }
                    group.output_start
                } else if basic {
                    return Err(invalid("unmatched closing parenthesis"));
                } else {
                    literal(&mut output, ')');
                    start
                };
                branch_start = false;
                atom_start = Some(group_start);
                repeated = false;
            }
            '|' if special => {
                output.push('|');
                available_captures = groups
                    .last()
                    .map_or([false; 10], |group| group.available_on_entry);
                branch_start = true;
                atom_start = None;
                repeated = false;
            }
            '+' | '?' | '{' if basic && escaped && atom_start.is_none() => {
                // With no preceding atom, GNU BRE quotes these operators.
                // A leading \{ is literal even when no valid interval follows.
                show_warning!("stray \\ before {token}");
                literal(&mut output, token);
                atom_start = Some(start);
                repeated = false;
                branch_start = false;
            }
            '+' | '?' if special => {
                if !basic && atom_start.is_none() {
                    show_warning!("{token} at start of expression");
                } else {
                    repeat(&mut output, atom_start, &mut repeated, &token.to_string())?;
                }
                branch_start = false;
            }
            '{' if special => {
                if let Some((interval, end)) = interval(&chars, ix, basic)? {
                    if !basic && atom_start.is_none() {
                        show_warning!("{{...}} at start of expression");
                    } else {
                        repeat(&mut output, atom_start, &mut repeated, &interval)?;
                    }
                    ix = end;
                    branch_start = false;
                } else {
                    literal(&mut output, '{');
                    atom_start = Some(start);
                    repeated = false;
                    branch_start = false;
                }
            }
            '[' if !escaped => {
                ix = bracket(&chars, ix, &mut output)?;
                atom_start = Some(start);
                repeated = false;
                branch_start = false;
            }
            '*' if !escaped => {
                if basic && atom_start.is_none() {
                    literal(&mut output, '*');
                    atom_start = Some(start);
                    repeated = false;
                } else if !basic && atom_start.is_none() {
                    show_warning!("* at start of expression");
                } else {
                    repeat(&mut output, atom_start, &mut repeated, "*")?;
                }
                branch_start = false;
            }
            '^' if !escaped && (!basic || branch_start) => {
                output.push('^');
                atom_start = None;
                repeated = false;
                branch_start = false;
            }
            '$' if !escaped && (!basic || basic_branch_end(&chars, ix + 1)) => {
                output.push('$');
                atom_start = None;
                repeated = false;
                branch_start = false;
            }
            '.' if !escaped => {
                output.push('.');
                atom_start = Some(start);
                repeated = false;
                branch_start = false;
            }
            '1'..='9' if escaped => {
                let capture = token as usize - '0' as usize;
                if !available_captures[capture] {
                    return Err(invalid("Invalid back reference"));
                }
                // GNU backreferences consume exactly one digit: \10 means
                // backreference 1 followed by 0, not backreference 10.
                output.push_str("(?:\\");
                output.push(token);
                output.push(')');
                atom_start = Some(start);
                repeated = false;
                branch_start = false;
            }
            'w' | 'W' | 'b' | 'B' if escaped => {
                output.push('\\');
                output.push(token);
                atom_start = if token == 'w' || token == 'W' {
                    Some(start)
                } else {
                    None
                };
                repeated = false;
                branch_start = false;
            }
            's' | 'S' if escaped => {
                output.push_str(if token == 's' {
                    "[[:space:]]"
                } else {
                    "[^[:space:]]"
                });
                atom_start = Some(start);
                repeated = false;
                branch_start = false;
            }
            '<' | '>' | '`' | '\'' if escaped => {
                output.push_str(match token {
                    '<' => "(?<!\\w)(?=\\w)",
                    '>' => "(?<=\\w)(?!\\w)",
                    '`' => "\\A",
                    _ => "\\z",
                });
                atom_start = None;
                repeated = false;
                branch_start = false;
            }
            _ => {
                // GNU's unknown escapes quote the following character; in
                // particular \d, \n, \x and \p must not gain Rust meanings.
                literal(&mut output, token);
                atom_start = Some(start);
                repeated = false;
                branch_start = false;
            }
        }
        ix += 1;
    }
    if !groups.is_empty() {
        return Err(invalid("unmatched opening parenthesis"));
    }
    Ok(output)
}

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

fn basic_branch_end(chars: &[char], ix: usize) -> bool {
    ix == chars.len()
        || (chars.get(ix) == Some(&'\\') && matches!(chars.get(ix + 1), Some(')' | '|')))
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
mod tests {
    use super::translate;
    use crate::RegexMode::{Basic, Extended, Fixed, Perl};

    #[test]
    fn bre_operators_and_contextual_anchors() {
        for (pattern, expected) in [
            (r"a+(b)?|{2}", r"a\+\(b\)\?\|\{2\}"),
            (r"a\+\(b\)\?\|c\{,2\}", r"a+(b)?|c{0,2}"),
            (r"a^b$c", r"a\^b\$c"),
            (r"^^a", r"^\^a"),
            (r"^a$\|\(^b$\)", r"^a$|(^b$)"),
            (r"^*a", r"^\*a"),
            (r"\(a\)\10", r"(a)(?:\1)0"),
        ] {
            assert_eq!(translate(pattern, Basic).expect(pattern), expected);
        }
    }

    #[test]
    fn bre_repetition_operators_without_an_atom_are_literal() {
        for (pattern, expected) in [
            (r"\+a", r"\+a"),
            (r"\?a", r"\?a"),
            (r"\{1\}a", r"\{1\}a"),
            (r"\{bogus\}a", r"\{bogus\}a"),
            (r"\{a", r"\{a"),
            (r"^\+a", r"^\+a"),
            (r"\(\?a\)", r"(\?a)"),
            (r"z\|\{1\}a", r"z|\{1\}a"),
            (r"\(^\+a\)", r"(^\+a)"),
            (r"\B\?a", r"\B\?a"),
            (r"\`\{a", r"\A\{a"),
        ] {
            assert_eq!(translate(pattern, Basic).expect(pattern), expected);
        }
    }

    #[test]
    fn ere_quotes_rust_extensions_and_unknown_escapes() {
        for (pattern, expected) in [
            (r"\d\n\t\p\x\0", "dntpx0"),
            (r"\(a\)\+\?\|\{2\}", r"\(a\)\+\?\|\{2\}"),
            (r"a+?", r"(?:a+)?"),
            (r"a*+", r"(?:a*)+"),
            (r"(a|b){,2}", r"(a|b){0,2}"),
            (r"a{1", r"a\{1"),
            (r"a{b}", r"a\{b\}"),
            (r"a{,}", r"a{0,}"),
            (r"a)", r"a\)"),
            (r"(a))", r"(a)\)"),
        ] {
            assert_eq!(translate(pattern, Extended).expect(pattern), expected);
        }
    }

    #[test]
    fn gnu_backslash_assertions() {
        assert_eq!(
            translate(r"\`\<\w+\>\b\B\'\s\S\W", Extended).expect("GNU escapes"),
            r"\A(?<!\w)(?=\w)\w+(?<=\w)(?!\w)\b\B\z[[:space:]][^[:space:]]\W"
        );
    }

    #[test]
    fn bracket_expressions_do_not_acquire_rust_set_or_escape_syntax() {
        for (pattern, expected) in [
            (r"[\d]", r"[\\d]"),
            (r"[a&&b~~c]", r"[a\&\&b\~\~c]"),
            (r"[[]", r"[\[]"),
            (r"[]a]", r"[\]a]"),
            (r"[^]a]", r"[^\]a]"),
            (r"[[:alpha:]_-]", r"[[:alpha:]_\x2d]"),
            (r"[--a]", r"[\x2d-a]"),
        ] {
            assert_eq!(translate(pattern, Extended).expect(pattern), expected);
        }
    }

    #[test]
    fn errors_are_explicit() {
        for (pattern, mode) in [
            (r"a\", Basic),
            (r"\(a", Basic),
            (r"a\)", Basic),
            (r"a\{2", Basic),
            (r"a{}", Extended),
            (r"[abc", Extended),
            (r"[[:bad:]]", Extended),
            (r"[[:alpha]", Extended),
            (r"[[.a.]]", Extended),
            (r"[[=a=]]", Extended),
            (r"[a--b]", Extended),
            (r"[a-b-c]", Extended),
            (r"[[:alpha:]-z]", Extended),
            (r"[a-[:alpha:]]", Extended),
            (r"(?i)a", Extended),
            (r"a", Perl),
        ] {
            let error = translate(pattern, mode).expect_err(pattern);
            assert_eq!(error.code(), 2);
            assert!(error.to_string().contains("experimental"));
        }
    }

    #[test]
    fn fixed_patterns_are_literal() {
        assert_eq!(
            translate(r"a[\d]+?(b){2}^$", Fixed).expect("fixed"),
            r"a\[\\d\]\+\?\(b\)\{2\}\^\$"
        );
    }

    #[test]
    fn leading_ere_quantifiers_are_ignored_with_a_warning() {
        for pattern in ["?", "*", "+", "{2}", "{,2}"] {
            assert_eq!(translate(pattern, Extended).expect(pattern), "");
        }
    }

    #[test]
    fn familiar_diagnostics_are_preserved() {
        for pattern in ["[:digit:]", "q[^:digit:]w", "[:nosuchclass:]"] {
            assert!(
                translate(pattern, Extended)
                    .expect_err(pattern)
                    .to_string()
                    .contains("character class syntax is [[:space:]], not [:space:]")
            );
        }
        for pattern in ["[b-a]", "[a--b]", "[a-b-c]", "[[:alpha:]-z]"] {
            assert!(
                translate(pattern, Extended)
                    .expect_err(pattern)
                    .to_string()
                    .contains("Invalid range end")
            );
        }
    }

    #[test]
    fn interval_bounds_are_checked_without_integer_overflow() {
        for (pattern, mode) in [
            (r"a{2,1}", Extended),
            (r"a\{2,1\}", Basic),
            (r"a{0002,0001}", Extended),
            (r"a{99999999999999999999999999999999,1}", Extended),
        ] {
            let error = translate(pattern, mode).expect_err(pattern);
            assert_eq!(error.code(), 2);
            assert!(error.to_string().contains("Invalid content of \\{\\}"));
        }
        for pattern in ["a{32768}", "a{1,99999999999999999999999999999999}"] {
            assert!(
                translate(pattern, Extended)
                    .expect_err(pattern)
                    .to_string()
                    .contains("Regular expression too big")
            );
        }
        for (pattern, expected) in [
            (
                "a{0000000000000000000000002,0000000000000000000000003}",
                "a{2,3}",
            ),
            ("a{0000000000000000000000000}", "a{0}"),
            ("a{,}", "a{0,}"),
            ("a{32767,}", "a{32767,}"),
        ] {
            assert_eq!(translate(pattern, Extended).expect(pattern), expected);
        }
    }

    #[test]
    fn backreferences_require_a_closed_capture_available_in_this_branch() {
        for pattern in [
            r"(a)|\1",
            r"(a)|(b)\1",
            r"(a|b\1)",
            r"(a\1)",
            r"\1(a)",
            r"((a)|b\2)",
            r"(a)|((b)|\1)",
            r"(a)(b)|\2",
            r"((a)|((b)|\2))",
        ] {
            let basic = pattern
                .replace('(', r"\(")
                .replace(')', r"\)")
                .replace('|', r"\|");
            for (pattern, mode) in [(pattern, Extended), (basic.as_str(), Basic)] {
                let error = translate(pattern, mode).expect_err(pattern);
                assert_eq!(error.code(), 2);
                assert!(error.to_string().contains("Invalid back reference"));
            }
        }
        for pattern in [
            r"((a)|(b))\2",
            r"((a)|(b))\3",
            r"(a)((b)|\1)",
            r"(a)?\1",
            r"((a)|b)\2",
            r"((a)\2|b)",
        ] {
            let basic = pattern
                .replace('(', r"\(")
                .replace(')', r"\)")
                .replace('|', r"\|")
                .replace('?', r"\?");
            assert!(translate(pattern, Extended).is_ok(), "{pattern}");
            assert!(translate(&basic, Basic).is_ok(), "{basic}");
        }
    }
}
