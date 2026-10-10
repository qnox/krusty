//! Kotlin/JVM `String` operations the compiler evaluates over constants: the `trim` family, which
//! removes `Char.isWhitespace` code units, and `lowercase`/`uppercase`, which are Java's
//! `toLowerCase(Locale.ROOT)`/`toUpperCase(Locale.ROOT)`.

use super::{is_kotlin_whitespace, KtString, KtStringBuf};

/// Kotlin's `String.trim()`.
pub(crate) fn trim(value: &KtString) -> KtString {
    trimmed(value, true, true)
}

/// Kotlin's `String.trimStart()`.
pub(crate) fn trim_start(value: &KtString) -> KtString {
    trimmed(value, true, false)
}

/// Kotlin's `String.trimEnd()`.
pub(crate) fn trim_end(value: &KtString) -> KtString {
    trimmed(value, false, true)
}

fn trimmed(value: &KtString, start: bool, end: bool) -> KtString {
    let units = value.units().collect::<Vec<_>>();
    let mut first = 0;
    let mut last = units.len();
    if start {
        while first < last && is_kotlin_whitespace(units[first]) {
            first += 1;
        }
    }
    if end {
        while last > first && is_kotlin_whitespace(units[last - 1]) {
            last -= 1;
        }
    }
    KtString::from_units(units[first..last].to_vec())
}

/// Kotlin's `String.lowercase()`: Unicode full case mapping, including the final-sigma rule.
pub(crate) fn lowercase(value: &KtString) -> KtString {
    case_mapped(value, str::to_lowercase)
}

/// Kotlin's `String.uppercase()`: Unicode full case mapping (`"ß"` becomes `"SS"`).
pub(crate) fn uppercase(value: &KtString) -> KtString {
    case_mapped(value, str::to_uppercase)
}

/// Map each well-formed run of `value` with `map`; an unpaired surrogate has no case and is kept,
/// as Java keeps it.
fn case_mapped(value: &KtString, map: fn(&str) -> String) -> KtString {
    if let Some(text) = value.as_str() {
        return KtString::from(map(text));
    }
    let units = value.units().collect::<Vec<_>>();
    let mut output = KtStringBuf::new();
    let mut run = Vec::new();
    let mut index = 0;
    while index < units.len() {
        let unit = units[index];
        let pair = (0xd800..0xdc00).contains(&unit)
            && units
                .get(index + 1)
                .is_some_and(|next| (0xdc00..0xe000).contains(next));
        if pair {
            run.extend_from_slice(&units[index..index + 2]);
            index += 2;
        } else if (0xd800..0xe000).contains(&unit) {
            output.push_str(&map(&String::from_utf16_lossy(&run)));
            run.clear();
            output.push_unit(unit);
            index += 1;
        } else {
            run.push(unit);
            index += 1;
        }
    }
    output.push_str(&map(&String::from_utf16_lossy(&run)));
    output.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trimming_removes_kotlin_whitespace_only_at_the_requested_ends() {
        let value = KtString::from("\u{1c} a b\u{a0}");
        assert_eq!(trim(&value), KtString::from("a b"));
        assert_eq!(trim_start(&value), KtString::from("a b\u{a0}"));
        assert_eq!(trim_end(&value), KtString::from("\u{1c} a b"));
        assert_eq!(trim(&KtString::from("\u{85}x")), KtString::from("\u{85}x"));
    }

    #[test]
    fn case_mapping_is_full_unicode_mapping_and_keeps_lone_surrogates() {
        assert_eq!(
            uppercase(&KtString::from("straße")),
            KtString::from("STRASSE")
        );
        assert_eq!(
            lowercase(&KtString::from("\u{39f}\u{394}\u{39f}\u{3a3}")),
            KtString::from("\u{3bf}\u{3b4}\u{3bf}\u{3c2}")
        );
        let lone = KtString::from_units(vec![b'a' as u16, 0xd800, b'B' as u16]);
        assert_eq!(
            uppercase(&lone).units().collect::<Vec<_>>(),
            [b'A' as u16, 0xd800, b'B' as u16]
        );
    }
}
