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

/// One file the map names: its simple source name and the path the reference compiler records for
/// it, which is the internal name of the class that owns it.
#[derive(Clone, Debug, Eq, PartialEq)]
struct MappedFile {
    name: String,
    path: String,
}

/// One `*L` entry: `input_start#file,count:output_start`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct LineRange {
    input_start: u16,
    file: u16,
    count: u16,
    output_start: u16,
}

/// One `KotlinDebug` entry: the call-site line an inlined output range came from.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CallSite {
    call_line: u16,
    output_start: u16,
    count: u16,
}

/// The source map accumulated while a class is emitted.
#[derive(Clone, Debug, Default)]
pub struct SourceMap {
    files: Vec<MappedFile>,
    ranges: Vec<LineRange>,
    call_sites: Vec<CallSite>,
    /// One past the highest output line handed out. Output lines above the file's own line count
    /// are what identify inlined code.
    next_output_line: u16,
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
            files: vec![MappedFile {
                name: name.to_string(),
                path: path.to_string(),
            }],
            ranges: vec![LineRange {
                input_start: 1,
                file: 1,
                count: lines,
                output_start: 1,
            }],
            call_sites: Vec::new(),
            next_output_line: lines + 1,
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

    /// Reserve output lines for an inlined region covering `first..=last` of `name`, and return the
    /// offset to add to a line of that file to get its output line.
    ///
    /// `call_line` is the line of the call being expanded; the `KotlinDebug` stratum sends the whole
    /// region back to it.
    pub fn inline_region(
        &mut self,
        name: &str,
        path: &str,
        first: u16,
        last: u16,
        call_line: u16,
    ) -> Option<i32> {
        if last < first {
            return None;
        }
        let count = last.checked_sub(first)?.checked_add(1)?;
        let output_start = self.next_output_line;
        self.next_output_line = self.next_output_line.checked_add(count)?;
        let file = match self
            .files
            .iter()
            .position(|known| known.name == name && known.path == path)
        {
            Some(at) => at as u16 + 1,
            None => {
                self.files.push(MappedFile {
                    name: name.to_string(),
                    path: path.to_string(),
                });
                self.files.len() as u16
            }
        };
        self.ranges.push(LineRange {
            input_start: first,
            file,
            count,
            output_start,
        });
        self.call_sites.push(CallSite {
            call_line,
            output_start,
            count,
        });
        Some(i32::from(output_start) - i32::from(first))
    }

    /// The `SourceDebugExtension` payload, or `None` when nothing was inlined.
    pub fn render(&self) -> Option<String> {
        if self.is_unstarted() || self.is_empty() {
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
        out.push_str("*L\n");
        for range in &self.ranges {
            out.push_str(&line_row(
                range.input_start,
                range.file,
                range.count,
                range.output_start,
                1,
            ));
        }
        // The debug stratum names only the owning file: every inlined range is reported at the line
        // of the call that expanded it.
        out.push_str("*S KotlinDebug\n*F\n");
        out.push_str(&format!("+ 1 {}\n{}\n", owner.name, owner.path));
        out.push_str("*L\n");
        for site in &self.call_sites {
            // The debug stratum fixes the file and repeat count at 1. Its output-line increment is
            // the inlined region's size; like the repeat count, an increment of 1 is omitted.
            out.push_str(&line_row(
                site.call_line,
                1,
                1,
                site.output_start,
                site.count,
            ));
        }
        out.push_str("*E\n");
        Some(out)
    }
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

    #[test]
    fn a_class_with_nothing_inlined_has_no_map() {
        let map = SourceMap::new("Main.kt", "MainKt", 3);
        assert!(map.is_empty());
        assert_eq!(map.render(), None);
    }

    /// The shape `docs/JVM_INLINE_BEFORE_CPS.md` measures: a two-line caller — so three claimable
    /// lines — expanding lines 2..9 of a library at its line 2.
    #[test]
    fn one_inlined_region_reproduces_the_reference_map() {
        let mut map = SourceMap::new("Main.kt", "MainKt", 3);
        let shift = map
            .inline_region("Lib.kt", "LibKt", 2, 9, 2)
            .expect("a well-formed region");
        assert_eq!(shift, 2, "library line 2 becomes output line 4");
        assert_eq!(
            map.render().expect("a map"),
            "SMAP\nMain.kt\nKotlin\n*S Kotlin\n*F\n\
             + 1 Main.kt\nMainKt\n+ 2 Lib.kt\nLibKt\n\
             *L\n1#1,3:1\n2#2,8:4\n\
             *S KotlinDebug\n*F\n+ 1 Main.kt\nMainKt\n*L\n2#1:4,8\n*E\n"
        );
    }

    /// A second inlining of the same file reuses its id but takes fresh output lines, because the
    /// two regions occupy different code.
    #[test]
    fn a_repeated_file_keeps_its_id_and_takes_new_output_lines() {
        let mut map = SourceMap::new("Main.kt", "MainKt", 3);
        assert_eq!(map.inline_region("Lib.kt", "LibKt", 2, 9, 2), Some(2));
        assert_eq!(map.inline_region("Lib.kt", "LibKt", 2, 9, 3), Some(10));
        let rendered = map.render().expect("a map");
        assert_eq!(rendered.matches("+ 2 Lib.kt").count(), 1);
        assert!(rendered.contains("2#2,8:4\n2#2,8:12\n"));
        assert!(rendered.contains("2#1:4,8\n3#1:12,8\n"));
    }

    #[test]
    fn an_empty_or_inverted_region_is_refused() {
        let mut map = SourceMap::new("Main.kt", "MainKt", 3);
        assert_eq!(map.inline_region("Lib.kt", "LibKt", 9, 2, 2), None);
        assert!(map.is_empty());
    }
}
