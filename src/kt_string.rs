//! [`KtString`] — the value of a Kotlin `String` constant.
//!
//! A Kotlin `String` is a sequence of UTF-16 code UNITS, not of Unicode scalar values. `"\uD800"`
//! is a one-element string whose element is `Char.MIN_HIGH_SURROGATE`, and `"\uD83D\uDE00"` is the
//! two-element spelling of U+1F600. A Rust `String` holds only scalar values, so neither an
//! unpaired surrogate nor a `\uXXXX` escape pair decoded one half at a time has a `String` form:
//! `char::from_u32` rejects both halves and the constant silently collapses to `""`.
//!
//! Nearly every string in a Kotlin program *is* scalar text, so this keeps a `String` fast path and
//! degrades to a code-unit vector only for content that has no `String` spelling. That degraded
//! form is reachable only through [`KtStringBuf`], which re-canonicalizes on [`KtStringBuf::finish`],
//! so the two representations never both spell the same value. Consumers may therefore rely on
//! `Eq`/`Hash` being the value's identity — the class-file constant pool dedups on it.

/// A Kotlin `String` value: a sequence of UTF-16 code units.
///
/// Build one from a `&str`/`String` when the content is ordinary text, or through [`KtStringBuf`]
/// when it is assembled from code units (a literal unescaper, a constant fold).
#[derive(Clone, PartialEq, Eq, Hash, Default)]
pub struct KtString(Repr);

/// INVARIANT: `Units` holds a code-unit sequence that `String::from_utf16` REJECTS. Canonicalizing
/// in [`KtStringBuf::finish`] and [`KtString::from_units`] is what makes `Text`/`Units` disjoint.
#[derive(Clone, PartialEq, Eq, Hash, Default)]
enum Repr {
    #[default]
    Empty,
    Text(String),
    Units(Vec<u16>),
}

impl KtString {
    pub fn new() -> KtString {
        KtString(Repr::Empty)
    }

    /// The value of `units`, taking the `String` form when the sequence is well-formed UTF-16.
    pub fn from_units(units: Vec<u16>) -> KtString {
        match String::from_utf16(&units) {
            Ok(text) => KtString::from(text),
            Err(_) => KtString(Repr::Units(units)),
        }
    }

    /// The `&str` spelling, or `None` when the value contains an unpaired surrogate.
    ///
    /// Callers that need `&str` for a JVM *name* (a descriptor, a `@JvmName`) can treat `None` as
    /// "not a name"; callers that carry a string VALUE must use [`KtString::units`] instead so the
    /// code units survive.
    pub fn as_str(&self) -> Option<&str> {
        match &self.0 {
            Repr::Empty => Some(""),
            Repr::Text(text) => Some(text),
            Repr::Units(_) => None,
        }
    }

    /// The UTF-16 code units of this value, in order.
    pub fn units(&self) -> impl Iterator<Item = u16> + '_ {
        enum Iter<'a> {
            Text(std::str::EncodeUtf16<'a>),
            Units(std::slice::Iter<'a, u16>),
        }
        impl Iterator for Iter<'_> {
            type Item = u16;
            fn next(&mut self) -> Option<u16> {
                match self {
                    Iter::Text(it) => it.next(),
                    Iter::Units(it) => it.next().copied(),
                }
            }
        }
        match &self.0 {
            Repr::Empty => Iter::Text("".encode_utf16()),
            Repr::Text(text) => Iter::Text(text.encode_utf16()),
            Repr::Units(units) => Iter::Units(units.iter()),
        }
    }

    /// The Kotlin `String.length` of this value — a count of code units, not of characters.
    pub fn len_utf16(&self) -> usize {
        match &self.0 {
            Repr::Empty => 0,
            Repr::Text(text) => text.encode_utf16().count(),
            Repr::Units(units) => units.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        match &self.0 {
            Repr::Empty => true,
            Repr::Text(text) => text.is_empty(),
            Repr::Units(units) => units.is_empty(),
        }
    }

    /// The single code unit of a one-unit value, else `None`.
    pub fn single_unit(&self) -> Option<u16> {
        let mut units = self.units();
        let first = units.next()?;
        units.next().is_none().then_some(first)
    }

    /// A `String` rendering for DIAGNOSTICS ONLY: an unpaired surrogate becomes U+FFFD, so this is
    /// lossy and must never reach emitted output.
    pub fn to_lossy(&self) -> String {
        match &self.0 {
            Repr::Empty => String::new(),
            Repr::Text(text) => text.clone(),
            Repr::Units(units) => String::from_utf16_lossy(units),
        }
    }
}

/// Whether a UTF-16 code unit is `Char.isWhitespace()` under Kotlin's JVM semantics:
/// `Character.isWhitespace(c) || Character.isSpaceChar(c)`.
///
/// Rust's Unicode `White_Space` predicate differs for `U+001C..U+001F` and `U+0085`. A surrogate
/// half is not whitespace and deliberately never passes through a lossy scalar conversion.
fn is_kotlin_whitespace(unit: u16) -> bool {
    if (0x1c..=0x1f).contains(&unit) {
        return true;
    }
    unit != 0x85 && char::from_u32(unit as u32).is_some_and(char::is_whitespace)
}

fn is_blank_line(line: &[u16]) -> bool {
    line.iter().all(|&unit| is_kotlin_whitespace(unit))
}

const LF: u16 = b'\n' as u16;

fn join_lines(lines: Vec<Vec<u16>>) -> KtString {
    let mut output = Vec::new();
    for (index, line) in lines.into_iter().enumerate() {
        if index > 0 {
            output.push(LF);
        }
        output.extend_from_slice(&line);
    }
    KtString::from_units(output)
}

/// The value semantics of Kotlin's `String.trimIndent()` over UTF-16 code units.
pub(crate) fn trim_indent(value: &KtString) -> KtString {
    let units = value.units().collect::<Vec<_>>();
    let lines = units.split(|&unit| unit == LF).collect::<Vec<_>>();
    let minimum_indent = lines
        .iter()
        .filter(|line| !is_blank_line(line))
        .map(|line| {
            line.iter()
                .take_while(|&&unit| is_kotlin_whitespace(unit))
                .count()
        })
        .min()
        .unwrap_or(0);
    let last = lines.len().saturating_sub(1);
    join_lines(
        lines
            .iter()
            .enumerate()
            .filter_map(|(index, line)| {
                if (index == 0 || index == last) && is_blank_line(line) {
                    return None;
                }
                let cut = minimum_indent.min(line.len());
                Some(line[cut..].to_vec())
            })
            .collect(),
    )
}

/// The value semantics of Kotlin's `String.trimMargin(prefix)` over UTF-16 code units.
pub(crate) fn trim_margin(value: &KtString, margin: &KtString) -> KtString {
    let margin = margin.units().collect::<Vec<_>>();
    let units = value.units().collect::<Vec<_>>();
    let lines = units.split(|&unit| unit == LF).collect::<Vec<_>>();
    let last = lines.len().saturating_sub(1);
    join_lines(
        lines
            .iter()
            .enumerate()
            .filter_map(|(index, line)| {
                if (index == 0 || index == last) && is_blank_line(line) {
                    return None;
                }
                let indent = line
                    .iter()
                    .take_while(|&&unit| is_kotlin_whitespace(unit))
                    .count();
                let trimmed = &line[indent..];
                Some(if trimmed.starts_with(&margin) {
                    trimmed[margin.len()..].to_vec()
                } else {
                    line.to_vec()
                })
            })
            .collect(),
    )
}

impl From<String> for KtString {
    fn from(text: String) -> KtString {
        if text.is_empty() {
            KtString(Repr::Empty)
        } else {
            KtString(Repr::Text(text))
        }
    }
}

impl From<&str> for KtString {
    fn from(text: &str) -> KtString {
        KtString::from(text.to_string())
    }
}

impl std::fmt::Debug for KtString {
    /// Renders like a Rust string literal, with an unpaired surrogate shown as `\u{d800}` — the
    /// value has no literal spelling, so this is deliberately not round-trippable Kotlin.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.as_str() {
            Some(text) => write!(f, "{text:?}"),
            None => {
                f.write_str("\"")?;
                for unit in self.units() {
                    match char::from_u32(unit as u32) {
                        Some(c) => write!(f, "{}", c.escape_debug())?,
                        None => write!(f, "\\u{{{unit:x}}}")?,
                    }
                }
                f.write_str("\"")
            }
        }
    }
}

/// An accumulator for a [`KtString`] under construction.
///
/// Stays on the `String` path until a code unit with no scalar form arrives, then switches to code
/// units. [`KtStringBuf::finish`] re-tests the result, so a high surrogate followed by its low half
/// comes back out as ordinary text.
pub struct KtStringBuf(Repr);

impl KtStringBuf {
    pub fn new() -> KtStringBuf {
        KtStringBuf(Repr::Text(String::new()))
    }

    pub fn with_capacity(bytes: usize) -> KtStringBuf {
        KtStringBuf(Repr::Text(String::with_capacity(bytes)))
    }

    pub fn push(&mut self, c: char) {
        match &mut self.0 {
            Repr::Text(text) => text.push(c),
            Repr::Units(units) => {
                let mut buf = [0u16; 2];
                units.extend_from_slice(c.encode_utf16(&mut buf));
            }
            Repr::Empty => unreachable!("KtStringBuf never holds the empty marker"),
        }
    }

    pub fn push_str(&mut self, s: &str) {
        match &mut self.0 {
            Repr::Text(text) => text.push_str(s),
            Repr::Units(units) => units.extend(s.encode_utf16()),
            Repr::Empty => unreachable!("KtStringBuf never holds the empty marker"),
        }
    }

    /// Append one UTF-16 code unit, degrading to the code-unit representation if it is a surrogate.
    pub fn push_unit(&mut self, unit: u16) {
        match char::from_u32(unit as u32) {
            Some(c) => self.push(c),
            None => {
                self.degrade().push(unit);
            }
        }
    }

    pub fn push_kt(&mut self, other: &KtString) {
        match other.as_str() {
            Some(text) => self.push_str(text),
            None => self.degrade().extend(other.units()),
        }
    }

    pub fn finish(self) -> KtString {
        match self.0 {
            Repr::Empty => KtString::new(),
            Repr::Text(text) => KtString::from(text),
            Repr::Units(units) => KtString::from_units(units),
        }
    }

    /// Switch to the code-unit representation, transcoding whatever text is already buffered.
    fn degrade(&mut self) -> &mut Vec<u16> {
        if let Repr::Text(text) = &self.0 {
            self.0 = Repr::Units(text.encode_utf16().collect());
        }
        match &mut self.0 {
            Repr::Units(units) => units,
            _ => unreachable!("degrade leaves the code-unit representation"),
        }
    }
}

impl Default for KtStringBuf {
    fn default() -> KtStringBuf {
        KtStringBuf::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scalar_text_keeps_the_string_representation() {
        let s = KtString::from("hi");
        assert_eq!(s.as_str(), Some("hi"));
        assert_eq!(s.len_utf16(), 2);
    }

    #[test]
    fn an_unpaired_surrogate_has_no_str_form_but_keeps_its_unit() {
        let mut buf = KtStringBuf::new();
        buf.push_unit(0xD800);
        buf.push('x');
        let s = buf.finish();

        assert_eq!(s.as_str(), None);
        assert_eq!(s.units().collect::<Vec<_>>(), vec![0xD800, b'x' as u16]);
        assert_eq!(s.len_utf16(), 2);
    }

    #[test]
    fn a_completed_surrogate_pair_canonicalizes_back_to_text() {
        // U+1F600 written as its two halves must compare EQUAL to the same character written
        // directly, or the constant pool would hold two entries for one value.
        let mut buf = KtStringBuf::new();
        buf.push_unit(0xD83D);
        buf.push_unit(0xDE00);
        let s = buf.finish();

        assert_eq!(s.as_str(), Some("\u{1F600}"));
        assert_eq!(s, KtString::from("\u{1F600}"));
        assert_eq!(s.len_utf16(), 2);
    }

    #[test]
    fn from_units_canonicalizes_well_formed_input() {
        assert_eq!(
            KtString::from_units("ok".encode_utf16().collect()),
            KtString::from("ok")
        );
        assert_eq!(KtString::from_units(vec![0xDC00]).as_str(), None);
    }

    #[test]
    fn empty_is_one_value_however_it_is_built() {
        assert_eq!(KtString::new(), KtString::from(""));
        assert_eq!(KtString::new(), KtStringBuf::new().finish());
        assert_eq!(KtString::new(), KtString::from_units(Vec::new()));
        assert!(KtString::new().is_empty());
    }

    #[test]
    fn single_unit_sees_a_lone_surrogate() {
        let mut buf = KtStringBuf::new();
        buf.push_unit(0xDFFF);
        assert_eq!(buf.finish().single_unit(), Some(0xDFFF));
        assert_eq!(KtString::from("ab").single_unit(), None);
        // A supplementary character is TWO code units, so it is not a single-unit string.
        assert_eq!(KtString::from("\u{1F600}").single_unit(), None);
    }

    #[test]
    fn debug_escapes_an_unpaired_surrogate() {
        let mut buf = KtStringBuf::new();
        buf.push('a');
        buf.push_unit(0xD800);
        assert_eq!(format!("{:?}", buf.finish()), "\"a\\u{d800}\"");
    }

    #[test]
    fn trim_indent_uses_kotlin_whitespace_and_preserves_surrogates() {
        let source = KtString::from_units(vec![LF, b' ' as u16, 0xd800, b'x' as u16, LF]);
        assert_eq!(
            trim_indent(&source).units().collect::<Vec<_>>(),
            [0xd800, b'x' as u16]
        );

        let nel = KtString::from("\u{85}a\n\u{85}b");
        assert_eq!(trim_indent(&nel), nel);
    }

    #[test]
    fn trim_margin_operates_on_code_units() {
        let source =
            KtString::from_units(vec![LF, b' ' as u16, b'|' as u16, b'a' as u16, 0xdfff, LF]);
        assert_eq!(
            trim_margin(&source, &KtString::from("|"))
                .units()
                .collect::<Vec<_>>(),
            [b'a' as u16, 0xdfff]
        );
    }
}
