//! The JSR-045 source map (`SMAP`) a class carries when it contains inlined code.
//!
//! Splicing a dependency's `inline fun` puts that dependency's source into this class's methods, and
//! a debugger stepping through it must be told so. The reference compiler does not record the
//! library's own line numbers directly — two files would then claim the same numbers. It allocates a
//! fresh range of *output* lines for each inlined region, writes those into the caller's
//! `LineNumberTable`, and emits a `SourceDebugExtension` saying how to read them back:
//!
//! ```text
//! SMAP
//! Main.kt
//! Kotlin
//! *S Kotlin
//! *F
//! + 1 Main.kt
//! MainKt
//! + 2 Lib.kt
//! LibKt
//! *L
//! 1#1,3:1
//! 2#2,8:4
//! *S KotlinDebug
//! *F
//! + 1 Main.kt
//! MainKt
//! *L
//! 2#1:4,8
//! *E
//! ```
//!
//! `2#2,8:4` reads "input line 2 of file 2, 8 lines, starting at output line 4", so a dependency
//! line maps to `line - first + output_start`. The second stratum, `KotlinDebug`, maps that same
//! output range back to the line of the CALL, which is what a debugger shows when it declines to
//! step into inlined code.
//!
//! One of these belongs to a class, not to a splice: every inlining in every method shares the file
//! table and the output-line space.

/// One `*L` mapping: input lines `source..source+range` of its file occupy output lines
/// `dest..dest+range`. `call_site` is the line of the call that expanded it, and `None` only for the
/// owning file's own identity range.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RangeMapping {
    source: u16,
    dest: u16,
    range: u16,
    call_site: Option<u16>,
}

impl RangeMapping {
    fn max_dest(&self) -> Option<u16> {
        self.dest.checked_add(self.range.checked_sub(1)?)
    }
}

/// One file the map names: its simple source name, the path the reference compiler records for it
/// (the internal name of the class whose code carries those lines), and its ranges in the order
/// they were opened.
#[derive(Clone, Debug, Eq, PartialEq)]
struct FileMapping {
    name: String,
    path: String,
    ranges: Vec<RangeMapping>,
}

/// The file kotlinc's synthetic lines belong to (`FAKE_FILE_NAME`, `FAKE_PATH`).
const FAKE_FILE_NAME: &str = "fake.kt";
const FAKE_PATH: &str = "kotlin/jvm/internal/FakeKt";

/// How far past its end the most recently opened range may be stretched to take in a new line.
///
/// The reference compiler lets the range at the frontier of the output space absorb a line up to
/// ten past its end — wasting the skipped output lines, but saving a row. Any other range is only
/// reused for a line it already covers.
const FRONTIER_SLACK: u16 = 10;

/// The source map accumulated while a class is emitted.
#[derive(Clone, Debug, Default)]
pub struct SourceMap {
    files: Vec<FileMapping>,
    /// The highest output line handed out. Output lines above the file's own line count are what
    /// identify inlined code.
    max_used: u16,
    /// Whether the map is written even when it names only the owning file: kotlinc writes a
    /// regenerated class's map whatever it holds (`visitSMAP(…, intoInline = true, …)`).
    keep_trivial: bool,
}

impl SourceMap {
    /// Start a map for a class whose own source is `name` at `path`, spanning `lines` lines.
    ///
    /// The owning file is always file 1 and maps to itself, so a line of this class's own source
    /// keeps the number it always had. `lines` is the highest line the class's own code can claim,
    /// which is one past the file's last line — a synthesized position (a closing brace's implicit
    /// return, a generated accessor) is recorded there. The reference compiler writes `1#1,3:1` for
    /// a two-line file for exactly that reason.
    pub fn new(name: &str, path: &str, lines: u16) -> SourceMap {
        let lines = lines.max(1);
        SourceMap {
            files: vec![FileMapping {
                name: name.to_string(),
                path: path.to_string(),
                ranges: vec![RangeMapping {
                    source: 1,
                    dest: 1,
                    range: lines,
                    call_site: None,
                }],
            }],
            max_used: lines,
            keep_trivial: false,
        }
    }

    /// The map of a class regenerated from a compiled class whose own source is `name` at `path`,
    /// spanning `lines` lines (kotlinc's `SourceMapper(debugFileName, originalSmap)`). It is written
    /// even when nothing else is mapped into it.
    pub fn for_regenerated_class(name: &str, path: &str, lines: u16) -> SourceMap {
        SourceMap {
            keep_trivial: true,
            ..SourceMap::new(name, path, lines)
        }
    }

    /// Whether this map has not been started yet — a `Default` one, with no owning file.
    pub fn is_unstarted(&self) -> bool {
        self.files.is_empty()
    }

    /// Whether anything has been inlined into this class. A class with no inlined code carries no
    /// `SourceDebugExtension` at all.
    pub fn is_empty(&self) -> bool {
        self.files.len() < 2
    }

    /// The output line for line `source` of file `name` at `path`, expanded by a call at
    /// `call_line` of this class's own source.
    ///
    /// Lines are mapped one at a time, in the order the inlined code carries them, the way the
    /// reference compiler does it: a line joins an existing range of its file when that range came
    /// from the same call and already covers it — or, for the range at the frontier of the output
    /// space, lies within [`FRONTIER_SLACK`] past its end — and otherwise opens a new range at the
    /// next free output line. So a body whose lines are 1739 and 1814..=1816 takes two rows, not one
    /// row spanning the whole distance between them.
    pub fn map_line(&mut self, name: &str, path: &str, source: u16, call_line: u16) -> Option<u16> {
        self.map(name, path, source, Some(call_line))
    }

    /// The output line of synthetic line `id` (`mapSyntheticLineNumber`): a line of kotlinc's fake
    /// file, which no call expanded, so the debug stratum does not name it. An `@InlineOnly` call
    /// marks one before a lambda that starts on the call's own line.
    pub fn map_synthetic_line(&mut self, id: u16) -> Option<u16> {
        self.map(FAKE_FILE_NAME, FAKE_PATH, id, None)
    }

    /// The output line for line `source` of file `name` at `path` as a copied class's own map
    /// placed it, under the call it was expanded by there, if any (`SourceMapCopier`).
    pub fn map_copied_line(
        &mut self,
        name: &str,
        path: &str,
        source: u16,
        call_line: Option<u16>,
    ) -> Option<u16> {
        self.map(name, path, source, call_line)
    }

    fn map(&mut self, name: &str, path: &str, source: u16, call_site: Option<u16>) -> Option<u16> {
        let global_max = self.max_used;
        let file = self
            .files
            .iter()
            .position(|known| known.name == name && known.path == path);
        if let Some(file) = file {
            let ranges = &mut self.files[file].ranges;
            let reusable = |range: &RangeMapping| {
                let slack = if range.max_dest() == Some(global_max) {
                    FRONTIER_SLACK
                } else {
                    0
                };
                range.call_site == call_site
                    && source >= range.source
                    && source - range.source < range.range.saturating_add(slack)
            };
            if let Some(at) = ranges.iter().rposition(reusable) {
                let range = &mut ranges[at];
                let offset = source.checked_sub(range.source)?;
                let output = range.dest.checked_add(offset)?;
                let expanded = offset.checked_add(1)?;
                range.range = range.range.max(expanded);
                self.max_used = self.max_used.max(output);
                return Some(output);
            }
        }

        // Compute every fallible value before changing the map. Exhausting the class-file line
        // space declines this mark without leaving an empty file or an overflowing range behind.
        let output = global_max.checked_add(1)?;
        let range = RangeMapping {
            source,
            dest: output,
            range: 1,
            call_site,
        };
        match file {
            Some(file) => self.files[file].ranges.push(range),
            None => self.files.push(FileMapping {
                name: name.to_string(),
                path: path.to_string(),
                ranges: vec![range],
            }),
        }
        self.max_used = self.max_used.max(output);
        Some(output)
    }

    /// The `SourceDebugExtension` payload, or `None` when nothing was inlined.
    pub fn render(&self) -> Option<String> {
        if self.is_unstarted() || (self.is_empty() && !self.keep_trivial) {
            return None;
        }
        let owner = self.files.first()?;
        let mut out = String::new();
        out.push_str("SMAP\n");
        out.push_str(&owner.name);
        out.push_str("\nKotlin\n*S Kotlin\n*F\n");
        for (at, file) in self.files.iter().enumerate() {
            out.push_str(&format!("+ {} {}\n{}\n", at + 1, file.name, file.path));
        }
        // Rows are grouped by file, each file's ranges in the order they were opened.
        out.push_str("*L\n");
        for (at, file) in self.files.iter().enumerate() {
            for range in &file.ranges {
                out.push_str(&line_row(
                    range.source,
                    at as u16 + 1,
                    range.range,
                    range.dest,
                    1,
                ));
            }
        }
        // The debug stratum names only the owning file: every inlined range is reported at the line
        // of the call that expanded it. A map with no such range has no debug stratum at all
        // (`SMAPBuilder` writes a stratum only for mappings it has).
        let calls: Vec<_> = self
            .files
            .iter()
            .flat_map(|file| &file.ranges)
            .filter_map(|range| Some((range.call_site?, range)))
            .collect();
        if !calls.is_empty() {
            out.push_str("*S KotlinDebug\n*F\n");
            out.push_str(&format!("+ 1 {}\n{}\n", owner.name, owner.path));
            out.push_str("*L\n");
            for (call_line, range) in calls {
                // The debug stratum fixes the file and repeat count at 1. Its output-line increment
                // is the inlined range's size; like the repeat count, an increment of 1 is omitted.
                out.push_str(&line_row(call_line, 1, 1, range.dest, range.range));
            }
        }
        out.push_str("*E\n");
        Some(out)
    }
}

/// The `Kotlin` stratum of a DEPENDENCY's own `SourceDebugExtension`, read back so a line of its
/// code can be named by the file it really came from, and its `KotlinDebug` stratum, which names the
/// call each inlined range was expanded by.
///
/// A body from a class that already inlines something carries lines above its own source length;
/// the reference compiler resolves each through this map before mapping it into the caller, which
/// is why a spliced `map` names `_Collections.kt` lines 1739 and 1814 rather than one line 1739 and
/// one somewhere past the end of that file.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DependencyMap {
    kotlin: Stratum,
    debug: Stratum,
}

/// One stratum's `*F` and `*L` sections.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct Stratum {
    /// `(file id, name, path)`.
    files: Vec<(u16, String, String)>,
    rows: Vec<Row>,
}

/// One `*L` row as read. kotlinc's reader takes a repeat count other than 1 and an output increment
/// other than 1 alike as the range's length (`SMAPParser`), which is what [`Row::covers`] does.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Row {
    input_start: u32,
    file: u16,
    repeat_count: u32,
    output_start: u32,
    output_increment: u32,
}

impl Row {
    /// The input line output `line` stands for, when the row covers it.
    fn source_of(&self, line: u32) -> Option<u32> {
        let covered = self.repeat_count.checked_mul(self.output_increment)?;
        let offset = line.checked_sub(self.output_start)?;
        if offset >= covered {
            return None;
        }
        self.input_start.checked_add(offset / self.output_increment)
    }
}

impl Stratum {
    /// The `(file name, path, input line)` output `line` stands for.
    fn resolve(&self, line: u16) -> Option<(&str, &str, u16)> {
        let line = u32::from(line);
        let (file, source) = self
            .rows
            .iter()
            .find_map(|row| Some((row.file, row.source_of(line)?)))?;
        let (_, name, path) = self.files.iter().find(|(id, _, _)| *id == file)?;
        Some((name.as_str(), path.as_str(), u16::try_from(source).ok()?))
    }
}

impl DependencyMap {
    /// Parse the `Kotlin` and `KotlinDebug` strata of `text`. `None` when there is no `Kotlin`
    /// stratum or either does not read as JSR-045 — a map that cannot be trusted names no lines.
    pub fn parse(text: &str) -> Option<DependencyMap> {
        let mut lines = text.lines();
        if lines.next()? != "SMAP" {
            return None;
        }
        lines.next()?; // the generated file's own name
        lines.next()?; // the default stratum
        let kotlin = parse_stratum(&mut lines.skip_while(|line| *line != "*S Kotlin").skip(1))?;
        if kotlin.files.is_empty() || kotlin.rows.is_empty() {
            return None;
        }
        let mut debug = text
            .lines()
            .skip_while(|line| *line != "*S KotlinDebug")
            .skip(1)
            .peekable();
        let debug = if debug.peek().is_some() {
            parse_stratum(&mut debug)?
        } else {
            Stratum::default()
        };
        Some(DependencyMap { kotlin, debug })
    }

    /// The `(file name, path, source line)` that output `line` of the dependency's code stands for.
    pub fn resolve(&self, line: u16) -> Option<(&str, &str, u16)> {
        self.kotlin.resolve(line)
    }

    /// The `(line, file name, path)` of the call output `line` was expanded by, if it was inlined
    /// (`SMAPParser`'s call site: the start of the debug range that covers it).
    pub fn call_site(&self, line: u16) -> Option<(u16, &str, &str)> {
        let row = self
            .debug
            .rows
            .iter()
            .find(|row| row.source_of(u32::from(line)).is_some())?;
        let (_, name, path) = self.debug.files.iter().find(|(id, _, _)| *id == row.file)?;
        Some((
            u16::try_from(row.input_start).ok()?,
            name.as_str(),
            path.as_str(),
        ))
    }

    /// The path and length of the file named `name` (`FileMapping.toSourceInfo`): the highest input
    /// line any of its ranges covers.
    pub fn source_info(&self, name: &str) -> Option<(&str, u16)> {
        let (id, _, path) = self
            .kotlin
            .files
            .iter()
            .find(|(_, known, _)| known == name)?;
        let lines = self
            .kotlin
            .rows
            .iter()
            .filter(|row| row.file == *id)
            .map(|row| row.input_start + row.repeat_count * row.output_increment - 1)
            .max()?;
        Some((path.as_str(), u16::try_from(lines).ok()?))
    }
}

/// The `*F` and `*L` sections of the stratum `lines` is positioned in, up to the next stratum or
/// the end. `None` when they do not read as JSR-045: a file id repeated or zero, or a row naming a
/// file the section does not list or covering nothing.
fn parse_stratum<'a>(lines: &mut impl Iterator<Item = &'a str>) -> Option<Stratum> {
    let mut stratum = Stratum::default();
    let mut section = "";
    let mut pending_file: Option<(u16, String)> = None;
    for line in lines {
        if line.starts_with('*') {
            if line == "*E" || line.starts_with("*S ") {
                break;
            }
            section = line;
            continue;
        }
        match section {
            "*F" => {
                if let Some((id, name)) = pending_file.take() {
                    stratum.files.push((id, name, line.to_string()));
                    continue;
                }
                let rest = line.strip_prefix("+ ")?;
                let (id, name) = rest.split_once(' ')?;
                pending_file = Some((id.parse().ok()?, name.to_string()));
            }
            "*L" => {
                let (input, output) = line.split_once(':')?;
                let (input_start, file_and_count) = input.split_once('#')?;
                let (file, repeat_count) = match file_and_count.split_once(',') {
                    Some((file, count)) => (file, count.parse().ok()?),
                    None => (file_and_count, 1),
                };
                let (output_start, output_increment) = match output.split_once(',') {
                    Some((start, increment)) => (start, increment.parse().ok()?),
                    None => (output, 1),
                };
                stratum.rows.push(Row {
                    input_start: input_start.parse().ok()?,
                    file: file.parse().ok()?,
                    repeat_count,
                    output_start: output_start.parse().ok()?,
                    output_increment,
                });
            }
            _ => {}
        }
    }
    let files = &stratum.files;
    let distinct_files = files.iter().enumerate().all(|(index, (id, _, _))| {
        *id != 0 && files[..index].iter().all(|(known, _, _)| known != id)
    });
    let valid_rows = stratum.rows.iter().all(|row| {
        row.input_start != 0
            && row.repeat_count != 0
            && row.output_start != 0
            && row.output_increment != 0
            && files.iter().any(|(known, _, _)| *known == row.file)
    });
    (pending_file.is_none() && distinct_files && valid_rows).then_some(stratum)
}

/// One `*L` row. JSR-045 omits a repeat count or output-line increment when it is 1. The reference
/// compiler follows that: a single mapped line is `1#2:3`, never `1#2,1:3,1`.
fn line_row(
    input_start: u16,
    file: u16,
    repeat_count: u16,
    output_start: u16,
    output_increment: u16,
) -> String {
    let repeat_count = match repeat_count {
        1 => String::new(),
        count => format!(",{count}"),
    };
    let output_increment = match output_increment {
        1 => String::new(),
        increment => format!(",{increment}"),
    };
    format!("{input_start}#{file}{repeat_count}:{output_start}{output_increment}\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map_all(map: &mut SourceMap, name: &str, path: &str, lines: &[u16], call: u16) -> Vec<u16> {
        lines
            .iter()
            .map(|&line| map.map_line(name, path, line, call).expect("a mapped line"))
            .collect()
    }

    #[test]
    fn a_class_with_nothing_inlined_has_no_map() {
        let map = SourceMap::new("Main.kt", "MainKt", 3);
        assert!(map.is_empty());
        assert_eq!(map.render(), None);
    }

    /// The shape `docs/JVM_INLINE_BEFORE_CPS.md` measures: a two-line caller — so three claimable
    /// lines — expanding lines 2..9 of a library at its line 2. Contiguous lines grow one range.
    #[test]
    fn one_inlined_region_reproduces_the_reference_map() {
        let mut map = SourceMap::new("Main.kt", "MainKt", 3);
        let outputs = map_all(&mut map, "Lib.kt", "LibKt", &[2, 3, 4, 5, 6, 7, 8, 9], 2);
        assert_eq!(outputs, vec![4, 5, 6, 7, 8, 9, 10, 11]);
        assert_eq!(
            map.render().expect("a map"),
            "SMAP\nMain.kt\nKotlin\n*S Kotlin\n*F\n\
             + 1 Main.kt\nMainKt\n+ 2 Lib.kt\nLibKt\n\
             *L\n1#1,3:1\n2#2,8:4\n\
             *S KotlinDebug\n*F\n+ 1 Main.kt\nMainKt\n*L\n2#1:4,8\n*E\n"
        );
    }

    /// kotlinc's map for `repeat(n) { s += it }` in a seven-line file, where the lambda starts on
    /// the `@InlineOnly` call's line: the synthetic line takes the fake file's line 1. No call
    /// expanded it, so the map has no debug stratum.
    #[test]
    fn a_synthetic_line_maps_into_the_fake_file() {
        let mut map = SourceMap::new("t.kt", "TKt", 7);
        assert_eq!(map.map_synthetic_line(1), Some(8));
        assert_eq!(map.map_synthetic_line(1), Some(8));
        assert_eq!(
            map.render().expect("a map"),
            "SMAP\nt.kt\nKotlin\n*S Kotlin\n*F\n\
             + 1 t.kt\nTKt\n+ 2 fake.kt\nkotlin/jvm/internal/FakeKt\n\
             *L\n1#1,7:1\n1#2:8\n*E\n"
        );
    }

    /// The reference compiler's map for `xs.map { it.toString() }` in a two-line file: the body's
    /// lines are 1739 and then 1814..=1816, and the gap between them opens a second range instead of
    /// one row spanning 78 lines.
    #[test]
    fn distant_lines_open_separate_ranges() {
        let mut map = SourceMap::new("N.kt", "NKt", 2);
        let path = "kotlin/collections/CollectionsKt___CollectionsKt";
        let outputs = map_all(
            &mut map,
            "_Collections.kt",
            path,
            &[1739, 1814, 1815, 1816],
            1,
        );
        assert_eq!(outputs, vec![3, 4, 5, 6]);
        assert_eq!(
            map.render().expect("a map"),
            "SMAP\nN.kt\nKotlin\n*S Kotlin\n*F\n\
             + 1 N.kt\nNKt\n+ 2 _Collections.kt\n\
             kotlin/collections/CollectionsKt___CollectionsKt\n\
             *L\n1#1,2:1\n1739#2:3\n1814#2,3:4\n\
             *S KotlinDebug\n*F\n+ 1 N.kt\nNKt\n*L\n1#1:3\n1#1:4,3\n*E\n"
        );
    }

    /// The range at the frontier stretches up to ten lines past its end, leaving the skipped output
    /// lines unused; one line further opens a new range.
    #[test]
    fn the_frontier_range_absorbs_a_short_gap() {
        let mut map = SourceMap::new("Main.kt", "MainKt", 2);
        assert_eq!(
            map_all(&mut map, "Lib.kt", "LibKt", &[5, 15], 1),
            vec![3, 13]
        );
        assert_eq!(map_all(&mut map, "Lib.kt", "LibKt", &[26], 1), vec![14]);
        let rendered = map.render().expect("a map");
        assert!(
            rendered.contains("*L\n1#1,2:1\n5#2,11:3\n26#2:14\n"),
            "{rendered}"
        );
    }

    #[test]
    fn a_revisited_middle_range_keeps_its_original_output() {
        let mut map = SourceMap::new("Main.kt", "MainKt", 2);
        assert_eq!(
            map_all(&mut map, "Lib.kt", "LibKt", &[1, 20, 40, 20], 1),
            vec![3, 4, 5, 4]
        );
        assert_eq!(
            map.render().expect("a map"),
            "SMAP\nMain.kt\nKotlin\n*S Kotlin\n*F\n\
             + 1 Main.kt\nMainKt\n+ 2 Lib.kt\nLibKt\n\
             *L\n1#1,2:1\n1#2:3\n20#2:4\n40#2:5\n\
             *S KotlinDebug\n*F\n+ 1 Main.kt\nMainKt\n*L\n1#1:3\n1#1:4\n1#1:5\n*E\n"
        );
    }

    #[test]
    fn exhausting_output_lines_does_not_mutate_a_range() {
        let mut map = SourceMap::new("Main.kt", "MainKt", u16::MAX - 1);
        assert_eq!(
            map.map_line("Lib.kt", "LibKt", 1, u16::MAX - 1),
            Some(u16::MAX)
        );
        let before = map.render();
        assert_eq!(map.map_line("Lib.kt", "LibKt", 2, u16::MAX - 1), None);
        assert_eq!(map.render(), before);
        assert_eq!(
            map.map_line("Lib.kt", "LibKt", 1, u16::MAX - 1),
            Some(u16::MAX)
        );
    }

    /// A second expansion from ANOTHER call line cannot share a range: the debug stratum reports
    /// each range at one call.
    #[test]
    fn a_second_call_line_takes_new_output_lines() {
        let mut map = SourceMap::new("Main.kt", "MainKt", 3);
        assert_eq!(map_all(&mut map, "Lib.kt", "LibKt", &[2, 3], 2), vec![4, 5]);
        assert_eq!(map_all(&mut map, "Lib.kt", "LibKt", &[2, 3], 3), vec![6, 7]);
        let rendered = map.render().expect("a map");
        assert_eq!(rendered.matches("+ 2 Lib.kt").count(), 1);
        assert!(rendered.contains("2#2,2:4\n2#2,2:6\n"), "{rendered}");
        assert!(rendered.contains("2#1:4,2\n3#1:6,2\n"), "{rendered}");
    }

    /// A dependency's own map sends a line above its source length back to the file it came from.
    #[test]
    fn a_dependency_map_resolves_its_inlined_lines() {
        let map = DependencyMap::parse(
            "SMAP\n_Collections.kt\nKotlin\n*S Kotlin\n*F\n\
             + 1 _Collections.kt\nkotlin/collections/CollectionsKt___CollectionsKt\n\
             + 2 Iterables.kt\nkotlin/collections/CollectionsKt__IterablesKt\n\
             *L\n1#1,3900:1\n1814#1,3:3901\n10#2:3904\n\
             *S KotlinDebug\n*F\n+ 1 _Collections.kt\n\
             kotlin/collections/CollectionsKt___CollectionsKt\n*L\n1739#1:3901,3\n*E\n",
        )
        .expect("a readable map");
        let collections = "kotlin/collections/CollectionsKt___CollectionsKt";
        assert_eq!(
            map.resolve(1739),
            Some(("_Collections.kt", collections, 1739))
        );
        assert_eq!(
            map.resolve(3902),
            Some(("_Collections.kt", collections, 1815))
        );
        assert_eq!(
            map.resolve(3904),
            Some((
                "Iterables.kt",
                "kotlin/collections/CollectionsKt__IterablesKt",
                10
            ))
        );
        assert_eq!(map.resolve(3905), None);
    }

    #[test]
    fn a_map_without_a_kotlin_stratum_is_not_read() {
        assert_eq!(
            DependencyMap::parse("SMAP\nA.kt\nJava\n*S Java\n*E\n"),
            None
        );
        assert_eq!(DependencyMap::parse("not a map"), None);
    }

    #[test]
    fn an_internally_invalid_kotlin_map_is_not_read() {
        assert_eq!(
            DependencyMap::parse(
                "SMAP\nA.kt\nKotlin\n*S Kotlin\n*F\n+ 1 A.kt\nAKt\n*L\n1#1:1,0\n*E\n"
            ),
            None
        );
        assert_eq!(
            DependencyMap::parse(
                "SMAP\nA.kt\nKotlin\n*S Kotlin\n*F\n+ 1 A.kt\nAKt\n*L\n1#2:1\n*E\n"
            ),
            None
        );
    }
}
