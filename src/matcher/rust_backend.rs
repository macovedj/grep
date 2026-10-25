// This file is part of the uutils grep package.
//
// For the full copyright and license information, please view the LICENSE
// file that was distributed with this source code.

//! Experimental BRE/ERE execution over Rust engines. The syntax adapter and
//! locale profile deliberately define a narrower contract than GNU grep.

use super::gnu_syntax;
use crate::{Config, RegexMode};
use fancy_regex::{BytesMode, Regex as FancyRegex, RegexBuilder, RegexInput};
use regex_automata::{
    Anchored, Input, MatchKind,
    hybrid::dfa::{self, OverlappingState},
    meta,
    nfa::thompson,
    util::syntax,
};
use std::sync::Mutex;
use uucore::error::{UResult, USimpleError};

const DFA_CACHE_CAPACITY: usize = 8 * 1024 * 1024;
const BACKTRACK_LIMIT: usize = 1_000_000;

pub(super) struct RustPattern {
    engine: Engine,
    profile: TextProfile,
}

enum Engine {
    Regular(Box<Regular>),
    Backtracking(Box<Backtracking>),
}

struct Regular {
    first: meta::Regex,
    all: dfa::DFA,
    cache: Mutex<dfa::Cache>,
}

struct Backtracking {
    first: FancyRegex,
    longest: FancyRegex,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TextProfile {
    Ascii,
    Unicode,
}

impl TextProfile {
    fn is_word_match(self, line: &[u8], start: usize, end: usize) -> bool {
        if start > end || end > line.len() {
            return false;
        }
        if self == Self::Ascii {
            let word = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'_';
            return (start == 0 || !word(line[start - 1]))
                && (end == line.len() || !word(line[end]));
        }
        let before = (1..=start.min(4)).find_map(|width| {
            std::str::from_utf8(&line[start - width..start])
                .ok()
                .and_then(|text| {
                    let mut chars = text.chars();
                    let first = chars.next()?;
                    chars.next().is_none().then_some(first)
                })
        });
        let after = (1..=(line.len() - end).min(4)).find_map(|width| {
            std::str::from_utf8(&line[end..end + width])
                .ok()
                .and_then(|text| {
                    let mut chars = text.chars();
                    let first = chars.next()?;
                    chars.next().is_none().then_some(first)
                })
        });
        !before.is_some_and(regex_syntax::is_word_character)
            && !after.is_some_and(regex_syntax::is_word_character)
    }

    fn from_environment() -> UResult<Self> {
        // POSIX precedence ignores empty variables. Read once at compilation,
        // rather than making the meaning of a compiled regex change mid-search.
        let name = ["LC_ALL", "LC_CTYPE", "LANG"]
            .into_iter()
            .filter_map(std::env::var_os)
            .find(|value| !value.is_empty());
        match name {
            None => Ok(Self::Ascii),
            Some(name) => name.to_str().map_or_else(
                || Err(error("locale name must be valid UTF-8")),
                Self::from_name,
            ),
        }
    }

    fn from_name(name: &str) -> UResult<Self> {
        let normalized = name.to_ascii_lowercase().replace('-', "");
        match normalized.as_str() {
            "c" | "posix" => Ok(Self::Ascii),
            "c.utf8" | "en_us.utf8" => Ok(Self::Unicode),
            _ => Err(error(format!(
                "unsupported locale {name:?}; use LC_ALL=C, C.UTF-8 or en_US.UTF-8"
            ))),
        }
    }

    fn unicode(self) -> bool {
        self == Self::Unicode
    }
}

fn error(message: impl std::fmt::Display) -> Box<dyn uucore::error::UError> {
    USimpleError::new(2, format!("experimental Rust matcher: {message}"))
}

impl Regular {
    fn compile(
        pattern: &str,
        profile: TextProfile,
        ignore_case: bool,
        dot_matches_new_line: bool,
    ) -> Result<Self, String> {
        let syntax = syntax::Config::new()
            .utf8(false)
            .unicode(profile.unicode())
            .case_insensitive(ignore_case)
            .dot_matches_new_line(dot_matches_new_line);
        let first = meta::Regex::builder()
            .configure(meta::Config::new().utf8_empty(false))
            .syntax(syntax)
            .build(pattern)
            .map_err(|e| e.to_string())?;
        // Validate and compile the NFA now, but construct DFA transitions
        // only as searches need them. Eager determinization makes bounded
        // Unicode repetitions expensive even when no input is searched.
        let all = dfa::Builder::new()
            .configure(
                dfa::Config::new()
                    .match_kind(MatchKind::All)
                    .cache_capacity(DFA_CACHE_CAPACITY),
            )
            .syntax(syntax)
            .thompson(thompson::Config::new().utf8(false))
            .build(pattern)
            .map_err(|e| e.to_string())?;
        let cache = Mutex::new(all.create_cache());
        Ok(Self { first, all, cache })
    }

    fn search_leftmost(&self, line: &[u8], offset: usize) -> UResult<Option<(usize, usize)>> {
        if offset > line.len() {
            return Ok(None);
        }
        Ok(self
            .first
            .find(Input::new(line).range(offset..line.len()))
            .map(|found| (found.start(), found.end())))
    }

    fn longest_end_at(&self, line: &[u8], start: usize) -> UResult<Option<usize>> {
        if start > line.len() {
            return Ok(None);
        }
        // MatchKind::All alone does not implement leftmost-longest.
        // Anchor at the leftmost start established by the first pass,
        // retain the original haystack for assertions, then select the
        // largest endpoint among overlapping matches at that start.
        let input = Input::new(line)
            .range(start..line.len())
            .anchored(Anchored::Yes);
        let mut state = OverlappingState::start();
        let mut longest = None;
        let mut cache = self
            .cache
            .lock()
            .map_err(|_| error("regular matcher cache was poisoned"))?;
        loop {
            self.all
                .try_search_overlapping_fwd(&mut cache, &input, &mut state)
                .map_err(|e| error(format!("match failed: {e}")))?;
            let Some(found) = state.get_match() else {
                return Ok(longest);
            };
            longest = Some(longest.map_or(found.offset(), |end: usize| end.max(found.offset())));
        }
    }

    fn is_match(&self, line: &[u8]) -> UResult<bool> {
        Ok(self.first.is_match(line))
    }
}

impl RustPattern {
    pub(super) fn compile(pattern: &str, config: &Config<'_>) -> UResult<Self> {
        let profile = TextProfile::from_environment()?;
        let translated = if config.regex_mode == RegexMode::Fixed {
            escape_literal(pattern, profile)
        } else {
            translate_for_profile(pattern, config.regex_mode, profile)?
        };
        Self::compile_translated(&translated, profile, config.ignore_case, config.null_data)
    }

    fn compile_translated(
        pattern: &str,
        profile: TextProfile,
        ignore_case: bool,
        dot_matches_new_line: bool,
    ) -> UResult<Self> {
        let engine = if let Ok(regular) =
            Regular::compile(pattern, profile, ignore_case, dot_matches_new_line)
        {
            Engine::Regular(Box::new(regular))
        } else {
            // This includes backreferences and assertions or NFAs unsupported
            // by the lazy DFA. It never falls back to Oniguruma.
            let make = |longest| {
                RegexBuilder::new(pattern)
                    .bytes_mode(if profile.unicode() {
                        BytesMode::UnicodeBytes
                    } else {
                        BytesMode::Ascii
                    })
                    .case_insensitive(ignore_case)
                    .dot_matches_new_line(dot_matches_new_line)
                    .leftmost_longest(longest)
                    .backtrack_limit(BACKTRACK_LIMIT)
                    .build()
                    .map_err(|e| error(format!("invalid pattern: {e}")))
            };
            Engine::Backtracking(Box::new(Backtracking {
                first: make(false)?,
                longest: make(true)?,
            }))
        };
        Ok(Self { engine, profile })
    }

    pub(super) fn search_leftmost(
        &self,
        line: &[u8],
        offset: usize,
    ) -> UResult<Option<(usize, usize)>> {
        if offset > line.len() {
            return Ok(None);
        }
        match &self.engine {
            Engine::Regular(regular) => regular.search_leftmost(line, offset),
            Engine::Backtracking(backtracking) => {
                let offset = self.fancy_offset(line, offset)?;
                backtracking
                    .first
                    .find_from_pos(line, offset)
                    .map(|found| found.map(|found| (found.start(), found.end())))
                    .map_err(|e| error(format!("match failed: {e}")))
            }
        }
    }

    pub(super) fn longest_end_at(&self, line: &[u8], start: usize) -> UResult<Option<usize>> {
        if start > line.len() {
            return Ok(None);
        }
        match &self.engine {
            Engine::Regular(regular) => regular.longest_end_at(line, start),
            Engine::Backtracking(backtracking) => {
                if self.fancy_offset(line, start)? != start {
                    return Ok(None);
                }
                backtracking
                    .longest
                    .find_input(RegexInput::new(line).from_pos(start).anchored(true))
                    .map(|found| found.map(|found| found.end()))
                    .map_err(|e| error(format!("match failed: {e}")))
            }
        }
    }

    pub(super) fn is_match(&self, line: &[u8]) -> UResult<bool> {
        match &self.engine {
            Engine::Regular(regular) => regular.is_match(line),
            Engine::Backtracking(backtracking) => {
                self.fancy_offset(line, 0)?;
                backtracking
                    .first
                    .is_match(line)
                    .map_err(|e| error(format!("match failed: {e}")))
            }
        }
    }

    /// Stock fancy-regex 0.19.2 can skip valid bytes or panic when its Unicode
    /// VM encounters malformed UTF-8, and its ASCII VM can miss high-byte
    /// backreferences. Reject those inputs before entering that VM until the
    /// engine fixes are upstream. The regular engine continues to search
    /// arbitrary byte strings without this guard.
    fn fancy_offset(&self, line: &[u8], mut offset: usize) -> UResult<usize> {
        if self.profile == TextProfile::Ascii && !line.is_ascii() {
            return Err(error(
                "backreference or assertion matching on non-ASCII bytes in the C locale is not supported yet",
            ));
        }
        if self.profile.unicode() {
            let text = std::str::from_utf8(line).map_err(|_| {
                error("backreference or assertion matching on invalid UTF-8 is not supported yet")
            })?;
            // The caller advances one byte after empty matches. Do not begin a
            // Unicode VM search in the middle of an otherwise valid scalar.
            while !text.is_char_boundary(offset) {
                offset += 1;
            }
        }
        Ok(offset)
    }

    pub(super) fn is_word_match(&self, line: &[u8], start: usize, end: usize) -> bool {
        self.profile.is_word_match(line, start, end)
    }
}

fn translate_for_profile(pattern: &str, mode: RegexMode, profile: TextProfile) -> UResult<String> {
    if profile == TextProfile::Unicode {
        return gnu_syntax::translate(pattern, mode);
    }
    // Preserve the C-locale parser's byte atoms. Mapping bytes to U+0000..FF
    // gives the string-based syntax adapter one scalar per input byte, then
    // lowering those placeholders makes the Rust engines consume that byte.
    // Lowering only after translation of the original Unicode pattern would
    // incorrectly group multibyte literals when normalizing repeated GNU
    // quantifiers (for example, é+? quantifies just the final byte in C).
    let byte_atoms: String = pattern.bytes().map(char::from).collect();
    let translated = gnu_syntax::translate(&byte_atoms, mode)?;
    let mut lowered = String::with_capacity(translated.len());
    for ch in translated.chars() {
        if ch.is_ascii() {
            lowered.push(ch);
        } else {
            use std::fmt::Write;
            let _ = write!(lowered, "\\x{:02X}", u32::from(ch));
        }
    }
    Ok(lowered)
}

fn escape_literal(pattern: &str, profile: TextProfile) -> String {
    if profile == TextProfile::Ascii {
        // Hex escapes make even a UTF-8 pattern literal a sequence of bytes in
        // the C locale, without parsing multibyte literals as Unicode scalars.
        use std::fmt::Write;
        let mut escaped = String::with_capacity(pattern.len() * 4);
        for byte in pattern.bytes() {
            let _ = write!(escaped, "\\x{byte:02X}");
        }
        escaped
    } else {
        regex_syntax::escape(pattern)
    }
}

#[cfg(test)]
mod tests {
    use super::{Engine, RustPattern, TextProfile, escape_literal, translate_for_profile};
    use crate::RegexMode;

    fn compile(pattern: &str, profile: TextProfile) -> RustPattern {
        RustPattern::compile_translated(pattern, profile, false, false)
            .expect("valid experimental pattern")
    }

    #[test]
    fn regular_path_selects_leftmost_then_longest() {
        let pattern = compile("a|aa", TextProfile::Ascii);
        assert!(matches!(pattern.engine, Engine::Regular(_)));
        assert_eq!(pattern.search_leftmost(b"zaab", 0).unwrap(), Some((1, 2)));
        assert_eq!(pattern.longest_end_at(b"zaab", 1).unwrap(), Some(3));
        assert_eq!(pattern.search_leftmost(b"zaab", 2).unwrap(), Some((2, 3)));
    }

    #[test]
    fn bounded_unicode_repetitions_use_the_regular_engine() {
        let pattern = compile(r"\w{3,12}_(a|b)", TextProfile::Unicode);
        assert!(matches!(pattern.engine, Engine::Regular(_)));
        for line in ["!Δelta_a?", "!Привет_b?"] {
            let start = 1;
            let end = line.len() - 1;
            assert_eq!(
                pattern.search_leftmost(line.as_bytes(), 0).unwrap(),
                Some((start, end)),
            );
            assert_eq!(
                pattern.longest_end_at(line.as_bytes(), start).unwrap(),
                Some(end),
            );
        }
        assert_eq!(pattern.longest_end_at(b"!ab_a?", 1).unwrap(), None);
    }

    #[test]
    fn anchored_longest_keeps_the_original_haystack() {
        let pattern = compile("^a+", TextProfile::Ascii);
        assert_eq!(pattern.longest_end_at(b"baaa", 1).unwrap(), None);
        assert_eq!(pattern.search_leftmost(b"baaa", 1).unwrap(), None);
        assert_eq!(pattern.longest_end_at(b"aaa", 0).unwrap(), Some(3));
    }

    #[test]
    fn ambiguous_regular_patterns_do_not_enter_the_backtracking_vm() {
        let pattern = compile("(a|aa)+", TextProfile::Ascii);
        assert!(matches!(pattern.engine, Engine::Regular(_)));
        let line = vec![b'a'; 10_000];
        assert_eq!(pattern.longest_end_at(&line, 0).unwrap(), Some(line.len()));
    }

    #[test]
    fn unicode_regular_searches_survive_malformed_byte_prefixes() {
        let pattern = compile("[[:alpha:]]", TextProfile::Unicode);
        let line = b"\xE2\x82b";
        assert!(matches!(pattern.engine, Engine::Regular(_)));
        assert_eq!(pattern.search_leftmost(line, 0).unwrap(), Some((2, 3)));
        assert!(pattern.is_match(line).unwrap());
    }

    #[test]
    fn c_locale_dot_consumes_individual_non_ascii_bytes() {
        let pattern = compile(".", TextProfile::Ascii);
        assert_eq!(pattern.search_leftmost(b"\xFF", 0).unwrap(), Some((0, 1)));
        let pattern = compile(".", TextProfile::Unicode);
        assert_eq!(pattern.search_leftmost(b"\xFF", 0).unwrap(), None);
    }

    #[test]
    fn backreferences_use_the_two_pass_fallback() {
        let pattern = compile(r"(ab|a)\1", TextProfile::Ascii);
        assert!(matches!(pattern.engine, Engine::Backtracking(_)));
        assert_eq!(pattern.search_leftmost(b"xxabab", 0).unwrap(), Some((2, 6)));
        assert_eq!(pattern.longest_end_at(b"xxabab", 2).unwrap(), Some(6));
    }

    #[test]
    fn a_long_regular_miss_does_not_enter_the_backtracking_vm() {
        let pattern = compile(r"foo[0-9]+", TextProfile::Ascii);
        assert!(matches!(pattern.engine, Engine::Regular(_)));
        assert_eq!(
            pattern.search_leftmost(&vec![b'x'; 1 << 20], 0).unwrap(),
            None
        );
    }

    #[test]
    fn a_long_backreference_miss_reports_the_vm_budget() {
        let pattern = compile(r"(foo[0-9]+)\1", TextProfile::Ascii);
        assert!(pattern.search_leftmost(&vec![b'x'; 1 << 20], 0).is_err());
    }

    #[test]
    fn backtracking_exhaustion_is_an_error_instead_of_a_miss() {
        let pattern = compile(r"(a|aa)+\1", TextProfile::Ascii);
        assert!(pattern.longest_end_at(&[b'a'; 28], 0).is_err());
    }

    #[test]
    fn unicode_fancy_rejects_malformed_input_before_entering_the_vm() {
        let pattern = compile(r"(.)\1", TextProfile::Unicode);
        assert!(matches!(pattern.engine, Engine::Backtracking(_)));
        // A truncated multibyte lead exposed an out-of-bounds slice in the
        // stock engine during the standalone investigation.
        for line in [b"\xE2".as_slice(), b"\xE2\x82b", b"a\x80a"] {
            assert!(pattern.search_leftmost(line, 0).is_err());
            assert!(pattern.longest_end_at(line, 0).is_err());
            assert!(pattern.is_match(line).is_err());
        }
    }

    #[test]
    fn unicode_fancy_does_not_start_in_a_continuation_byte() {
        let pattern = compile(r"()\1", TextProfile::Unicode);
        assert_eq!(
            pattern.search_leftmost("é".as_bytes(), 1).unwrap(),
            Some((2, 2))
        );
        assert_eq!(pattern.longest_end_at("é".as_bytes(), 1).unwrap(), None);
    }

    #[test]
    fn c_fancy_rejects_non_ascii_input_before_entering_the_vm() {
        let pattern = compile(r"(.)\1", TextProfile::Ascii);
        for line in [b"\x80\x80".as_slice(), "éé".as_bytes()] {
            assert!(pattern.search_leftmost(line, 0).is_err());
            assert!(pattern.longest_end_at(line, 0).is_err());
            assert!(pattern.is_match(line).is_err());
        }
    }

    #[test]
    fn word_policy_uses_the_compiled_locale_and_bounded_scalars() {
        let ascii = compile("a", TextProfile::Ascii);
        let unicode = compile("a", TextProfile::Unicode);
        assert!(ascii.is_word_match("éaé".as_bytes(), 2, 3));
        assert!(!unicode.is_word_match("éaé".as_bytes(), 2, 3));
        assert!(unicode.is_word_match(b"\x80a\xE2", 1, 2));
        assert!(!unicode.is_word_match(b"_a!", 1, 2));
        assert!(!unicode.is_word_match(b"a", 1, 2));
    }

    #[test]
    fn fixed_strings_escape_bytes_in_c_and_scalars_in_unicode() {
        let ascii = compile(
            &escape_literal("é.*", TextProfile::Ascii),
            TextProfile::Ascii,
        );
        let unicode = compile(
            &escape_literal("é.*", TextProfile::Unicode),
            TextProfile::Unicode,
        );
        assert!(ascii.is_match("xé.*x".as_bytes()).unwrap());
        assert!(unicode.is_match("xé.*x".as_bytes()).unwrap());
        assert!(!ascii.is_match("xéxx".as_bytes()).unwrap());
        assert!(!unicode.is_match("xéxx".as_bytes()).unwrap());
    }

    #[test]
    fn c_regex_translation_preserves_byte_atoms_and_bracket_ranges() {
        for (raw, input, expected) in [
            ("café", "café".as_bytes(), Some((0, 5))),
            ("ß", "ß".as_bytes(), Some((0, 2))),
            ("é+", "éé".as_bytes(), Some((0, 2))),
            ("[é]", "é".as_bytes(), Some((0, 1))),
            ("[é-ê]", b"\xB0".as_slice(), Some((0, 1))),
            ("é+?", b"x".as_slice(), None),
            ("é+?", b"\xC3".as_slice(), Some((0, 1))),
        ] {
            let translated =
                translate_for_profile(raw, RegexMode::Extended, TextProfile::Ascii).unwrap();
            let pattern = compile(&translated, TextProfile::Ascii);
            let found = pattern.search_leftmost(input, 0).unwrap();
            assert_eq!(found, expected, "C-locale pattern {raw:?}");
        }
    }

    #[test]
    fn locale_profiles_are_explicit_and_do_not_guess_other_encodings() {
        assert_eq!(TextProfile::from_name("C").unwrap(), TextProfile::Ascii);
        assert_eq!(TextProfile::from_name("POSIX").unwrap(), TextProfile::Ascii);
        assert_eq!(
            TextProfile::from_name("C.utf8").unwrap(),
            TextProfile::Unicode
        );
        assert_eq!(
            TextProfile::from_name("en_US.UTF-8").unwrap(),
            TextProfile::Unicode
        );
        assert!(TextProfile::from_name("tr_TR.UTF-8").is_err());
        assert!(TextProfile::from_name("en_US.ISO-8859-1").is_err());
    }
}

#[cfg(test)]
mod profile_tests {
    use super::{TextProfile, escape_literal, translate_for_profile};
    use crate::RegexMode;

    #[test]
    fn word_policy_uses_the_compiled_locale_and_bounded_scalars() {
        let ascii = TextProfile::Ascii;
        let unicode = TextProfile::Unicode;
        assert!(ascii.is_word_match("éaé".as_bytes(), 2, 3));
        assert!(!unicode.is_word_match("éaé".as_bytes(), 2, 3));
        assert!(unicode.is_word_match(b"\x80a\xE2", 1, 2));
        assert!(!unicode.is_word_match(b"_a!", 1, 2));
        assert!(!unicode.is_word_match(b"a", 1, 2));
    }

    #[test]
    fn locale_profiles_are_explicit_and_do_not_guess_other_encodings() {
        assert_eq!(TextProfile::from_name("C").unwrap(), TextProfile::Ascii);
        assert!(!TextProfile::from_name("C").unwrap().unicode());
        assert!(TextProfile::from_name("C.UTF-8").unwrap().unicode());
        assert_eq!(TextProfile::from_name("POSIX").unwrap(), TextProfile::Ascii);
        assert_eq!(
            TextProfile::from_name("C.utf8").unwrap(),
            TextProfile::Unicode
        );
        assert_eq!(
            TextProfile::from_name("en_US.UTF-8").unwrap(),
            TextProfile::Unicode
        );
        assert!(TextProfile::from_name("tr_TR.UTF-8").is_err());
        assert!(TextProfile::from_name("en_US.ISO-8859-1").is_err());
    }

    #[test]
    fn c_literals_are_encoded_as_individual_bytes() {
        assert_eq!(
            escape_literal("é.*", TextProfile::Ascii),
            r"\xC3\xA9\x2E\x2A"
        );
        assert_eq!(escape_literal("é.*", TextProfile::Unicode), r"é\.\*");
    }

    #[test]
    fn c_translation_keeps_the_final_byte_as_the_repeated_atom() {
        assert_eq!(
            translate_for_profile("é+", RegexMode::Extended, TextProfile::Ascii).unwrap(),
            r"\xC3\xA9+",
        );
        assert_eq!(
            translate_for_profile("é+", RegexMode::Extended, TextProfile::Unicode).unwrap(),
            "é+",
        );
    }
}
