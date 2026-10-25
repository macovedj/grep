// This file is part of the uutils grep package.
//
// For the full copyright and license information, please view the LICENSE
// file that was distributed with this source code.

//! Experimental BRE/ERE execution over Rust engines. The syntax adapter and
//! locale profile deliberately define a narrower contract than GNU grep.

use super::gnu_syntax;
use crate::RegexMode;
use uucore::error::{UResult, USimpleError};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TextProfile {
    Ascii,
    Unicode,
}

impl TextProfile {
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
}

fn error(message: impl std::fmt::Display) -> Box<dyn uucore::error::UError> {
    USimpleError::new(2, format!("experimental Rust matcher: {message}"))
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
