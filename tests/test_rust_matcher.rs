// This file is part of the uutils grep package.
//
// For the full copyright and license information, please view the LICENSE
// file that was distributed with this source code.

#![cfg(feature = "rust-matcher")]

use std::io::Write;
use std::process::{Command, Output, Stdio};

/// Set the locale on the child rather than mutating this test process's
/// environment, so the C and UTF-8 cases can run concurrently.
fn grep(locale: &str, args: &[&str], input: &[u8]) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_grep"))
        .args(args)
        .env("LC_ALL", locale)
        .env_remove("GREP_OPTIONS")
        .env_remove("GREP_COLORS")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start grep");
    let written = child.stdin.take().expect("grep stdin").write_all(input);
    if let Err(error) = written {
        // A rejected pattern or unsupported locale may close stdin before the
        // parent writes; the command's status and diagnostic are the result.
        assert_eq!(error.kind(), std::io::ErrorKind::BrokenPipe);
    }
    child.wait_with_output().expect("wait for grep")
}

fn assert_output(locale: &str, args: &[&str], input: &[u8], expected: &[u8], status: i32) {
    let output = grep(locale, args, input);
    assert_eq!(
        output.status.code(),
        Some(status),
        "arguments {args:?}, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, expected, "arguments {args:?}");
}

#[test]
fn regular_alternation_selects_longest_at_the_first_start() {
    assert_output("C", &["-Eo", "a|aa"], b"aa\n", b"aa\n", 0);
    assert_output("C", &["-Eo", "b+|a"], b"abbbb\n", b"a\nbbbb\n", 0);
}

#[test]
fn multiple_patterns_share_leftmost_longest_ordering() {
    assert_output("C", &["-Eo", "-e", "a", "-e", "aa"], b"aa\n", b"aa\n", 0);
}

#[test]
fn bre_translates_escaped_operators_and_preserves_literal_operators() {
    assert_output("C", &["-o", r"a\|aa"], b"aa\n", b"aa\n", 0);
    assert_output("C", &["-o", "a+b"], b"a+b\naaab\n", b"a+b\n", 0);
    assert_output("C", &["-o", r"a\{2,3\}"], b"aaaa\n", b"aaa\n", 0);
}

#[test]
fn bre_whitespace_extension_and_unknown_escape_have_grep_meaning() {
    assert_output("C", &["-o", r"\s"], b"a b\na\tb\n", b" \n\t\n", 0);
    // In GNU BRE/ERE, \d means a literal d rather than a Rust digit class.
    assert_output("C", &["-o", r"\d"], b"d1d\n", b"d\nd\n", 0);
    assert_output("C", &["-Eo", r"\d"], b"d1d\n", b"d\nd\n", 0);
}

#[test]
fn bre_anchors_are_literal_away_from_branch_edges() {
    assert_output("C", &["-o", "a^b"], b"a^b\nab\n", b"a^b\n", 0);
    assert_output("C", &["-o", "a$b"], b"a$b\nab\n", b"a$b\n", 0);
}

#[test]
fn unmatched_ere_closing_parenthesis_is_literal() {
    assert_output("C", &["-Eo", "a)"], b"a)\na\n", b"a)\n", 0);
}

#[test]
fn reversed_intervals_are_rejected_before_entering_either_engine() {
    for args in [&["-Eo", "a{2,1}"][..], &["-o", r"a\{2,1\}"][..]] {
        let output = grep("C", args, b"aaa\n");
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        assert!(String::from_utf8_lossy(&output.stderr).contains("Invalid content"));
    }
}

#[test]
fn backreferences_keep_original_byte_offsets() {
    assert_output(
        "C",
        &["-bo", r"\(ab\)\1"],
        b"zabab\nxxabab\n",
        b"1:abab\n8:abab\n",
        0,
    );
    assert_output("C", &["-Ebo", r"(a|ab)\1"], b"zabab\n", b"1:abab\n", 0);
}

#[test]
fn backreferences_require_a_closed_group_in_the_current_branch() {
    for args in [
        &["-Eo", r"(a)|\1"][..],
        &["-Eo", r"(a)|(b)\1"][..],
        &["-Eo", r"(a\1)"][..],
        &["-Eo", r"\1(a)"][..],
        &["-o", r"\(a\)\|\1"][..],
    ] {
        let output = grep("C", args, b"aa bb\n");
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        assert!(String::from_utf8_lossy(&output.stderr).contains("Invalid back reference"));
    }
    assert_output("C", &["-Eo", r"((a)|(b))\2"], b"aa bb\n", b"aa\n", 0);
}

#[test]
fn regular_utf8_matching_recovers_after_malformed_bytes() {
    assert_output(
        "C.UTF-8",
        &["-aEbo", "[[:alpha:]]"],
        b"\xe2\x82b\n",
        b"2:b\n",
        0,
    );
    assert_output("C.UTF-8", &["-aEbo", "."], b"\xe2(\xa1\n", b"1:(\n", 0);
}

#[test]
fn c_locale_dot_matches_individual_bytes() {
    assert_output("C", &["-aEbo", "."], b"\xffa\n", b"0:\xff\n1:a\n", 0);
    assert_output("POSIX", &["-aEbo", "."], b"\xffa\n", b"0:\xff\n1:a\n", 0);
}

#[test]
fn c_locale_non_ascii_patterns_have_byte_atom_semantics() {
    assert_output(
        "C",
        &["-aEo", "é+"],
        b"\xc3\xa9\xa9\n",
        b"\xc3\xa9\xa9\n",
        0,
    );
    assert_output("C", &["-aEo", "[é]"], b"\xc3\xa9\n", b"\xc3\n\xa9\n", 0);
}

#[test]
fn fixed_matching_preserves_bytes_and_offsets() {
    assert_output("C", &["-aFbo", "a"], b"\xffa\n", b"1:a\n", 0);
}

#[test]
fn zero_length_matches_make_progress_without_being_printed() {
    assert_output("C", &["-Eo", "a*"], b"baa\n", b"aa\n", 0);
    assert_output("C", &["-Eo", "^|a"], b"aa\n", b"a\na\n", 0);
    assert_output("C", &["-Eo", "$"], b"aa\n", b"", 0);
}

#[test]
fn null_delimited_records_use_the_same_selection_policy() {
    assert_output("C", &["-zEo", "a|aa"], b"aa\0a\0", b"aa\0a\0", 0);
}

#[test]
fn ambiguous_regular_pattern_does_not_use_the_backtracking_budget() {
    let mut input = vec![b'a'; 200];
    input.push(b'\n');
    assert_output("C", &["-Eo", "(a|aa)+"], &input, &input, 0);
}

#[test]
fn backtracking_limit_is_an_error_instead_of_a_nonmatch() {
    // A short ambiguous backreference is enough to exceed the fallback's
    // bounded search. It must not silently change the command's answer to 1.
    let mut input = vec![b'a'; 28];
    input.push(b'\n');
    let output = grep("C", &["-Eo", r"(a|aa)+\1"], &input);
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr).to_ascii_lowercase();
    assert!(
        stderr.contains("backtrack") || stderr.contains("limit"),
        "{stderr}"
    );
}

#[test]
fn malformed_utf8_on_the_backreference_fallback_is_an_explicit_error() {
    // The experimental fallback deliberately refuses malformed UTF-8 until
    // its byte-search fixes can be supplied by an upstream engine release.
    let output = grep("C.UTF-8", &["-aEo", r"(a)\1"], b"\xffaa\n");
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr).to_ascii_lowercase();
    assert!(
        stderr.contains("utf-8") || stderr.contains("utf8"),
        "{stderr}"
    );
}

#[test]
fn non_ascii_bytes_on_the_c_locale_backreference_fallback_are_an_error() {
    // The stock fallback also has byte-mode backreference gaps. Refusing this
    // input preserves an explicit limitation instead of reporting no match.
    let output = grep("C", &["-ao", r"^\(.\)\1$"], b"\x80\x80\n");
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr).to_ascii_lowercase();
    assert!(
        stderr.contains("ascii") || stderr.contains("byte"),
        "{stderr}"
    );
}

#[test]
fn unsupported_locale_is_reported_instead_of_assuming_utf8() {
    let output = grep("en_US.ISO8859-1", &["-E", "."], b"a\n");
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("locale"));
}

#[test]
#[cfg(feature = "onig")]
fn perl_mode_stays_on_the_existing_engine_when_available() {
    assert_output("C", &["-Po", r"(ab)\1"], b"zabab\n", b"abab\n", 0);
}

#[test]
#[cfg(feature = "onig")]
fn invalid_perl_engine_spans_are_reported_before_reaching_output() {
    // A seeded raw-byte probe exposed an out-of-bounds span from the existing
    // engine. Keep it an explicit error rather than slicing beyond the record.
    let output = grep(
        "C",
        &["-P", "-a", "-o", "-b", r"\w"],
        b"(\xa1\xaf(\x7f\xafa\xff\xe2(\xc0\xc3 \xffb\n\xed\n",
    );
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr).to_ascii_lowercase();
    assert!(
        stderr.contains("outside") || stderr.contains("invalid match"),
        "{stderr}"
    );
}

#[test]
#[cfg(not(feature = "onig"))]
fn perl_mode_requires_the_optional_existing_engine() {
    let output = grep("C", &["-P", "a"], b"a\n");
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr).to_ascii_lowercase();
    assert!(
        stderr.contains("perl") || stderr.contains("onig") || stderr.contains("-p"),
        "{stderr}"
    );
}
