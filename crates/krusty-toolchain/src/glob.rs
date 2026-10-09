//! Module globs in `project.yaml`.
//!
//! The Kotlin Toolchain matches a `modules:` glob with the JDK's `glob:` path matcher
//! (`FileSystems.getDefault().getPathMatcher`) after normalizing the pattern's dot segments and
//! slashes. This is a transcription of that matcher (`sun.nio.fs.Globs.toUnixRegexPattern`), so a
//! pattern is accepted, refused, and matched exactly as the toolchain does:
//!
//! * `*` matches within one path segment, `**` across segments, `?` one character other than `/`;
//! * `[...]` is a character class (`!` negates, `a-z` is a range, `/` is refused), never matching `/`;
//! * `{a,b}` is a group of alternatives, which cannot nest;
//! * `\` escapes the next character.
//!
//! Errors carry the JDK's `PatternSyntaxException` message: the description, the index (in UTF-16
//! units, like Java's), the pattern, and a caret. One spelling the JDK reports through its regex
//! compiler instead, an empty class such as `[]` or `[!]`, is refused here with krusty's own message
//! rather than by reconstructing a regex-engine error.

use std::fmt;

/// A compiled glob.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Glob {
    tokens: Vec<Token>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Token {
    Literal(u16),
    /// `*`: any run of characters other than `/`.
    WithinSegment,
    /// `**`: any run of characters.
    AcrossSegments,
    /// `?`: one character other than `/`.
    OneCharacter,
    Class(Class),
    /// `{a,b}`: one of the alternatives.
    Group(Vec<Vec<Token>>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Class {
    negated: bool,
    /// Inclusive ranges; a single character is a one-character range.
    ranges: Vec<(u16, u16)>,
}

impl Class {
    fn matches(&self, unit: u16) -> bool {
        unit != u16::from(b'/')
            && self
                .ranges
                .iter()
                .any(|&(low, high)| (low..=high).contains(&unit))
                != self.negated
    }
}

/// A pattern the JDK's glob syntax rejects.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GlobError {
    description: &'static str,
    /// UTF-16 index of the offending character, as `PatternSyntaxException.getIndex()`.
    index: Option<usize>,
    pattern: String,
}

impl fmt::Display for GlobError {
    /// `PatternSyntaxException.getMessage()`, with `\n` as the line separator.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.description)?;
        if let Some(index) = self.index {
            write!(f, " near index {index}")?;
        }
        write!(f, "\n{}", self.pattern)?;
        if let Some(index) = self.index {
            if index < self.pattern.encode_utf16().count() {
                write!(f, "\n{}^", " ".repeat(index))?;
            }
        }
        Ok(())
    }
}

/// Characters that make a `modules:` entry a glob rather than a plain path
/// (`String.hasGlobCharacters` in the toolchain).
pub fn has_glob_characters(entry: &str) -> bool {
    entry
        .chars()
        .any(|c| matches!(c, '*' | '?' | '{' | '}' | '[' | ']' | ','))
}

impl Glob {
    /// Compile `pattern` as the JDK does, without normalizing it first.
    pub fn compile(pattern: &str) -> Result<Self, GlobError> {
        Compiler::new(pattern).compile()
    }

    /// Whether `path`, a `/`-separated relative path, matches.
    pub fn matches(&self, path: &str) -> bool {
        let units: Vec<u16> = path.encode_utf16().collect();
        match_frames(&[&self.tokens], &units, 0)
    }
}

/// The toolchain's glob normalization (`normalize` in its `globs.kt`), applied to the pattern before
/// it is compiled for matching; validation compiles the pattern as written. Runs of `/` collapse,
/// then every match of `(^|/)\.(/\.)*(/|$)` is replaced, then `(^|/)(?!\.\./)[^/]+/\.\.(/|$)` is
/// replaced until none is left (a replacement is `/` when the match starts and ends with `/`, else
/// nothing), and one trailing `/` is dropped unless the pattern is `/`.
pub fn normalize(pattern: &str) -> String {
    let mut collapsed = String::with_capacity(pattern.len());
    for c in pattern.chars() {
        if !(c == '/' && collapsed.ends_with('/')) {
            collapsed.push(c);
        }
    }
    let mut cleaned = replace_all(&collapsed, dot_segment_match);
    loop {
        let next = replace_all(&cleaned, parent_pair_match);
        if next == cleaned {
            break;
        }
        cleaned = next;
    }
    if cleaned != "/" {
        if let Some(stripped) = cleaned.strip_suffix('/') {
            cleaned = stripped.to_string();
        }
    }
    cleaned
}

/// Replace every non-overlapping match, scanning left to right as `String.replace(Regex, …)` does.
/// `find` returns the end of a match starting at the given byte offset.
fn replace_all(text: &str, find: fn(&[u8], usize) -> Option<usize>) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    let mut position = 0;
    while position <= bytes.len() {
        match find(bytes, position) {
            Some(end) => {
                out.push_str(&text[copied..position]);
                let matched = &bytes[position..end];
                if matched.first() == Some(&b'/') && matched.last() == Some(&b'/') {
                    out.push('/');
                }
                copied = end;
                // An empty match cannot occur: both expressions consume at least one character.
                position = end;
            }
            None => position += 1,
        }
    }
    out.push_str(&text[copied..]);
    out
}

/// `(^|/)\.(/\.)*(/|$)` at `start`: the end of the match, if one starts there.
fn dot_segment_match(bytes: &[u8], start: usize) -> Option<usize> {
    let try_from = |dot: usize| -> Option<usize> {
        if bytes.get(dot) != Some(&b'.') {
            return None;
        }
        // Greedy `(/\.)*`, remembering where each repetition began for backtracking.
        let mut end = dot + 1;
        let mut repetitions = Vec::new();
        while bytes.get(end) == Some(&b'/') && bytes.get(end + 1) == Some(&b'.') {
            repetitions.push(end);
            end += 2;
        }
        loop {
            match bytes.get(end) {
                None => return Some(end),
                Some(b'/') => return Some(end + 1),
                Some(_) => end = repetitions.pop()?,
            }
        }
    };
    // `^` alternative first, then `/`.
    if start == 0 {
        if let Some(end) = try_from(0) {
            return Some(end);
        }
    }
    if bytes.get(start) == Some(&b'/') {
        return try_from(start + 1);
    }
    None
}

/// `(^|/)(?!\.\./)[^/]+/\.\.(/|$)` at `start`: the end of the match, if one starts there.
fn parent_pair_match(bytes: &[u8], start: usize) -> Option<usize> {
    let try_from = |segment: usize| -> Option<usize> {
        if bytes[segment..].starts_with(b"../") {
            return None;
        }
        let length = bytes[segment..].iter().position(|&b| b == b'/')?;
        if length == 0 {
            return None;
        }
        let slash = segment + length;
        if !bytes[slash..].starts_with(b"/..") {
            return None;
        }
        match bytes.get(slash + 3) {
            None => Some(slash + 3),
            Some(b'/') => Some(slash + 4),
            Some(_) => None,
        }
    };
    if start == 0 {
        if let Some(end) = try_from(0) {
            return Some(end);
        }
    }
    if bytes.get(start) == Some(&b'/') {
        return try_from(start + 1);
    }
    None
}

struct Compiler<'a> {
    pattern: &'a str,
    units: Vec<u16>,
    index: usize,
    /// Where the first empty class (`[]`, `[!]`) closed. The JDK finishes translating the pattern
    /// before its regex compiler rejects that class, so a later glob error is reported first.
    empty_class: Option<usize>,
}

const EOL: u16 = 0;

impl<'a> Compiler<'a> {
    fn new(pattern: &'a str) -> Self {
        Self {
            pattern,
            units: pattern.encode_utf16().collect(),
            index: 0,
            empty_class: None,
        }
    }

    fn error(&self, description: &'static str, index: usize) -> GlobError {
        GlobError {
            description,
            index: Some(index),
            pattern: self.pattern.to_string(),
        }
    }

    fn peek(&self) -> u16 {
        self.units.get(self.index).copied().unwrap_or(EOL)
    }

    fn compile(mut self) -> Result<Glob, GlobError> {
        let mut top: Vec<Token> = Vec::new();
        // The alternatives of the open group, and the one being built.
        let mut group: Option<(Vec<Vec<Token>>, Vec<Token>)> = None;
        while self.index < self.units.len() {
            let unit = self.units[self.index];
            self.index += 1;
            let token = match unit {
                0x5c => {
                    // `\`
                    if self.index == self.units.len() {
                        return Err(self.error("No character to escape", self.index - 1));
                    }
                    let escaped = self.units[self.index];
                    self.index += 1;
                    Token::Literal(escaped)
                }
                0x5b => Token::Class(self.class()?), // `[`
                0x7b => {
                    // `{`
                    if group.is_some() {
                        return Err(self.error("Cannot nest groups", self.index - 1));
                    }
                    group = Some((Vec::new(), Vec::new()));
                    continue;
                }
                0x7d => match group.take() {
                    // `}`
                    Some((mut alternatives, current)) => {
                        alternatives.push(current);
                        Token::Group(alternatives)
                    }
                    None => Token::Literal(unit),
                },
                0x2c => match group.as_mut() {
                    // `,`
                    Some((alternatives, current)) => {
                        alternatives.push(std::mem::take(current));
                        continue;
                    }
                    None => Token::Literal(unit),
                },
                0x2a => {
                    // `*`
                    if self.peek() == 0x2a {
                        self.index += 1;
                        Token::AcrossSegments
                    } else {
                        Token::WithinSegment
                    }
                }
                0x3f => Token::OneCharacter, // `?`
                _ => Token::Literal(unit),
            };
            match group.as_mut() {
                Some((_, current)) => current.push(token),
                None => top.push(token),
            }
        }
        if group.is_some() {
            return Err(self.error("Missing '}", self.index - 1));
        }
        if let Some(index) = self.empty_class {
            return Err(GlobError {
                description: "Empty character class (krusty-toolchain refuses it; the JDK reports a regex error)",
                index: Some(index),
                pattern: self.pattern.to_string(),
            });
        }
        Ok(Glob { tokens: top })
    }

    /// The class after `[`, through its closing `]`.
    fn class(&mut self) -> Result<Class, GlobError> {
        let mut class = Class {
            negated: false,
            ranges: Vec::new(),
        };
        if self.peek() == u16::from(b'^') {
            // A leading `^` is a literal caret, not a negation.
            class.ranges.push((0x5e, 0x5e));
            self.index += 1;
        } else {
            if self.peek() == u16::from(b'!') {
                class.negated = true;
                self.index += 1;
            }
            if self.peek() == u16::from(b'-') {
                class.ranges.push((0x2d, 0x2d));
                self.index += 1;
            }
        }
        let mut range_start: Option<u16> = None;
        let mut unit = EOL;
        while self.index < self.units.len() {
            unit = self.units[self.index];
            self.index += 1;
            if unit == u16::from(b']') {
                break;
            }
            if unit == u16::from(b'/') {
                return Err(self.error("Explicit 'name separator' in class", self.index - 1));
            }
            if unit == u16::from(b'-') {
                let Some(low) = range_start.take() else {
                    return Err(self.error("Invalid range", self.index - 1));
                };
                unit = self.peek();
                self.index += 1;
                if unit == EOL || unit == u16::from(b']') {
                    // A trailing `-` is literal; the range start was already recorded.
                    class.ranges.push((0x2d, 0x2d));
                    break;
                }
                if unit < low {
                    return Err(self.error("Invalid range", self.index - 3));
                }
                // `low` was pushed as a single character when it was read; widen it.
                let last = class
                    .ranges
                    .last_mut()
                    .expect("the range start was recorded");
                *last = (low, unit);
            } else {
                class.ranges.push((unit, unit));
                range_start = Some(unit);
            }
        }
        if unit != u16::from(b']') {
            return Err(self.error("Missing ']", self.index - 1));
        }
        if class.ranges.is_empty() && self.empty_class.is_none() {
            self.empty_class = Some(self.index - 1);
        }
        Ok(class)
    }
}

/// Match the concatenation of `frames` against `text[position..]`. Groups push their alternative as
/// a new frame ahead of the rest, so the remainder of the pattern is tried after every alternative.
fn match_frames(frames: &[&[Token]], text: &[u16], position: usize) -> bool {
    let Some((first, rest)) = frames.split_first() else {
        return position == text.len();
    };
    let Some((token, tail)) = first.split_first() else {
        return match_frames(rest, text, position);
    };
    let continue_at = |next: usize| -> bool {
        let mut following: Vec<&[Token]> = Vec::with_capacity(rest.len() + 1);
        following.push(tail);
        following.extend_from_slice(rest);
        match_frames(&following, text, next)
    };
    let current = text.get(position).copied();
    match token {
        Token::Literal(unit) => current == Some(*unit) && continue_at(position + 1),
        Token::OneCharacter => {
            current.is_some_and(|unit| unit != u16::from(b'/')) && continue_at(position + 1)
        }
        Token::Class(class) => {
            current.is_some_and(|unit| class.matches(unit)) && continue_at(position + 1)
        }
        Token::WithinSegment => {
            let mut end = position;
            loop {
                if continue_at(end) {
                    return true;
                }
                match text.get(end) {
                    Some(&unit) if unit != u16::from(b'/') => end += 1,
                    _ => return false,
                }
            }
        }
        Token::AcrossSegments => (position..=text.len()).any(continue_at),
        Token::Group(alternatives) => alternatives.iter().any(|alternative| {
            let mut following: Vec<&[Token]> = Vec::with_capacity(rest.len() + 2);
            following.push(alternative);
            following.push(tail);
            following.extend_from_slice(rest);
            match_frames(&following, text, position)
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Recorded from the JDK 25 `glob:` path matcher (`scripts/kotlin-toolchain/GlobOracle.java`):
    /// each pattern with the paths it matches (`+`) and does not (`-`), or the exception message.
    const JDK_CASES: &[(&str, &[(&str, bool)])] = &[
        ("libs/[a-]", &[("libs/a", true), ("libs/-", true)]),
        ("libs/[-a]", &[("libs/a", true), ("libs/-", true)]),
        ("libs/[^a]", &[("libs/^", true), ("libs/a", true)]),
        ("libs/[!a]", &[("libs/b", true), ("libs/a", false)]),
        ("libs/[a-c]x", &[("libs/bx", true), ("libs/dx", false)]),
        ("a\\*", &[("a*", true), ("ab", false)]),
        ("{a,b}", &[("a", true), ("b", true), ("c", false)]),
        ("a}", &[("a}", true)]),
        ("a,b", &[("a,b", true)]),
        ("*", &[("a", true), ("a/b", false)]),
        ("**", &[("a/b", true)]),
        ("x/*/y", &[("x/a/y", true), ("x/a/b/y", false)]),
        ("?", &[("a", true), ("ab", false), ("/", false)]),
        ("[\\]", &[("\\", true)]),
        ("[&&a]", &[("&", true), ("a", true)]),
        ("[[]", &[("[", true)]),
        (".*", &[(".a", true), ("ab", false)]),
        ("a+b", &[("a+b", true)]),
        ("a(b)|c", &[("a(b)|c", true)]),
        ("$^", &[("$^", true)]),
    ];

    const JDK_ERRORS: &[(&str, &str)] = &[
        (
            "libs/[z-a]",
            "Invalid range near index 6\nlibs/[z-a]\n      ^",
        ),
        ("a\\", "No character to escape near index 1\na\\\n ^"),
        ("{a,{b}}", "Cannot nest groups near index 3\n{a,{b}}\n   ^"),
        ("{a", "Missing '} near index 1\n{a\n ^"),
        ("libs/{a", "Missing '} near index 6\nlibs/{a\n      ^"),
        (
            "[a/b]",
            "Explicit 'name separator' in class near index 2\n[a/b]\n  ^",
        ),
        ("libs/[a", "Missing '] near index 6\nlibs/[a\n      ^"),
    ];

    #[test]
    fn globs_match_as_the_jdk_matcher_does() {
        for (pattern, paths) in JDK_CASES {
            let glob = Glob::compile(pattern).unwrap_or_else(|error| panic!("{pattern}: {error}"));
            for (path, expected) in *paths {
                assert_eq!(glob.matches(path), *expected, "{pattern} against {path}");
            }
        }
    }

    #[test]
    fn invalid_globs_report_the_jdk_message() {
        for (pattern, message) in JDK_ERRORS {
            let error = Glob::compile(pattern).expect_err(pattern);
            assert_eq!(error.to_string(), *message, "{pattern}");
        }
    }

    #[test]
    fn an_empty_class_is_refused() {
        for pattern in ["libs/[]", "libs/[!]"] {
            let error = Glob::compile(pattern).expect_err(pattern);
            assert_eq!(
                error.to_string(),
                format!(
                    "Empty character class (krusty-toolchain refuses it; the JDK reports a regex error) near index {}\n{pattern}\n{}^",
                    pattern.len() - 1,
                    " ".repeat(pattern.len() - 1)
                )
            );
        }
    }

    /// `tests/recorded/globs.tsv`, recorded by `scripts/kotlin-toolchain/GlobOracle.java` from the JDK
    /// matcher and the toolchain's normalization: every pattern's normalized form, then either each
    /// path's verdict or the exception message.
    #[test]
    fn a_recorded_jdk_corpus_matches() {
        let recorded = include_str!("../tests/recorded/globs.tsv");
        let mut mismatches = Vec::new();
        for line in recorded.lines() {
            let mut fields = line.split('\t');
            let pattern = fields.next().expect("pattern");
            let normalized = fields.next().expect("normalized pattern");
            if normalize(pattern) != normalized {
                mismatches.push(format!(
                    "{pattern:?}: normalized {:?}, JDK {normalized:?}",
                    normalize(pattern)
                ));
            }
            let verdicts: Vec<&str> = fields.collect();
            let compiled = Glob::compile(pattern).and_then(|_| Glob::compile(normalized));
            match (verdicts.as_slice(), compiled) {
                ([error], Err(actual)) if error.starts_with('!') => {
                    let expected = error[1..].replace("\\n", "\n").replace("\\\\", "\\");
                    let regex_level = expected.starts_with("Unclosed character class");
                    if !regex_level && actual.to_string() != expected {
                        mismatches.push(format!("{pattern:?}: error {actual:?}, JDK {expected:?}"));
                    }
                }
                ([error], Ok(_)) if error.starts_with('!') => {
                    mismatches.push(format!("{pattern:?}: accepted, JDK {error}"));
                }
                (_, Err(actual)) => {
                    mismatches.push(format!("{pattern:?}: refused ({actual}), JDK accepts"))
                }
                (verdicts, Ok(glob)) => {
                    for verdict in verdicts {
                        let (expected, path) = verdict.split_at(1);
                        if glob.matches(&normalize_path(path)) != (expected == "+") {
                            mismatches
                                .push(format!("{pattern:?} against {path:?}: JDK {expected}"));
                        }
                    }
                }
            }
        }
        assert!(
            mismatches.is_empty(),
            "{} mismatches:\n{}",
            mismatches.len(),
            mismatches.join("\n")
        );
    }

    /// `Path.normalize()`: empty and `.` segments go, `x/..` pairs cancel, a leading `/` stays.
    fn normalize_path(path: &str) -> String {
        let root = if path.starts_with('/') { "/" } else { "" };
        let mut segments: Vec<&str> = Vec::new();
        for segment in path.split('/') {
            match segment {
                "" | "." => {}
                ".." if segments.last().is_some_and(|last| *last != "..") => {
                    segments.pop();
                }
                ".." if !root.is_empty() && segments.is_empty() => {}
                other => segments.push(other),
            }
        }
        format!("{root}{}", segments.join("/"))
    }

    #[test]
    fn normalization_follows_the_toolchain() {
        for (pattern, normalized) in [
            ("./libs/*", "libs/*"),
            ("libs//a/", "libs/a"),
            ("libs/./a/.", "libs/a"),
            ("libs/x/../a", "libs/a"),
            ("../a", "../a"),
            ("a/../../b", "../b"),
        ] {
            assert_eq!(normalize(pattern), normalized, "{pattern}");
        }
    }

    #[test]
    fn glob_characters_are_the_toolchains() {
        assert!(has_glob_characters("libs/*"));
        assert!(has_glob_characters("a,b"));
        assert!(!has_glob_characters("libs/a-b.c"));
    }
}
