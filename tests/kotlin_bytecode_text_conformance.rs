//! Kotlin's bytecode-text corpus, `compiler/testData/codegen/bytecodeText`, compiled by krusty.
//!
//! Each case is a box-style source whose comment lines `// <N> <pattern>` say how many times the
//! Java regular expression `(<pattern>)` occurs in the text ASM's `Textifier` prints for the
//! classes the case compiles to. That is JetBrains' `AbstractFirLightTreeBytecodeTextTest`; this
//! runner mirrors its `BytecodeTextHandler` and `BytecodeTextUtils`:
//!
//! - The suite's default directives are `WITH_STDLIB`, `WITH_REFLECT` and `ALLOW_KOTLIN_PACKAGE`.
//!   The stdlib is on every box compile's classpath already and the Kotlin-package permission
//!   follows the source, so only kotlin-reflect is added here.
//! - A module with one Kotlin file counts its expectations over every class it emitted except the
//!   coroutine `helpers/` ones, each preceded by an `@<path>.class` header line.
//! - A module with several Kotlin files (the coroutine helpers count as one) and no
//!   `TREAT_AS_ONE_FILE` counts each file's expectations over every class, except those after a
//!   `// @<path>.class:` line, which count over that one class alone.
//! - Expectations after `// JVM_IR_TEMPLATES_WITH_INLINE_SCOPES` hold only when inline scope numbers
//!   are on, and that is a separate JetBrains configuration of the corpus; this lane runs the
//!   default one and skips them, exactly as the reference test does.
//!
//! Only Kotlin's classes are textified: a mixed case's javac output is not in the reference
//! compiler's class factory. Applicability and the compilation topology are the JVM box lane's.
//!
//! The ratchet is `bytecode_corpus`'s, under `tests/bytecode_text_expected_*`, blessed with
//! `KRUSTY_BLESS_BYTECODE_TEXT_EXPECTATIONS=1` and focused with `KRUSTY_BYTECODE_TEXT_ONLY`.

use std::collections::BTreeMap;

use super::asm_text;
use super::box_reference_classes;
use super::bytecode_corpus::{Case, CaseClasses, CheckError, Outcome, Suite, TestModule};

/// Classes under this prefix are the generated coroutine helpers; a single-file module's
/// expectations never count them.
const HELPERS_PREFIX: &str = "helpers/";
const JVM_IR_TEMPLATES: &str = "// JVM_IR_TEMPLATES";
const JVM_IR_TEMPLATES_WITH_INLINE_SCOPES: &str = "// JVM_IR_TEMPLATES_WITH_INLINE_SCOPES";

/// One `// <N> <pattern>` line.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Occurrence {
    count: usize,
    needle: String,
    /// Declared under `// JVM_IR_TEMPLATES_WITH_INLINE_SCOPES`.
    inline_scopes: bool,
}

impl Occurrence {
    fn render(&self) -> String {
        format!("{} {}", self.count, self.needle)
    }
}

/// Strip a leading `\s*` as `java.util.regex` defines it.
fn trim_java_space(text: &str) -> &str {
    text.trim_start_matches([' ', '\t', '\n', '\u{b}', '\u{c}', '\r'])
}

/// The text `^...(.*)$` captures at the end of a line: `.` stops at a line terminator, and `$`
/// may match only before a final one.
fn rest_of_line(text: &str) -> Option<&str> {
    let body = text
        .strip_suffix("\r\n")
        .or_else(|| text.strip_suffix(['\r', '\u{85}', '\u{2028}', '\u{2029}']))
        .unwrap_or(text);
    (!body.contains(['\n', '\r', '\u{85}', '\u{2028}', '\u{2029}'])).then_some(body)
}

/// `^\s*//\s*(\d+)\s*(.*)$`.
fn expected_occurrence(line: &str, inline_scopes: bool) -> Option<Occurrence> {
    let rest = trim_java_space(trim_java_space(line).strip_prefix("//")?);
    let digits = rest.len() - rest.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    if digits == 0 {
        return None;
    }
    let count = rest[..digits].parse().ok()?;
    let needle = rest_of_line(trim_java_space(&rest[digits..]))?;
    Some(Occurrence {
        count,
        needle: needle.to_string(),
        inline_scopes,
    })
}

/// `^\s*//\s*@(.*):$`.
fn output_file(line: &str) -> Option<&str> {
    let rest = trim_java_space(trim_java_space(line).strip_prefix("//")?).strip_prefix('@')?;
    rest_of_line(rest)?.strip_suffix(':')
}

fn inline_scopes_after(line: &str, current: bool) -> bool {
    if line.contains(JVM_IR_TEMPLATES_WITH_INLINE_SCOPES) {
        true
    } else if line.contains(JVM_IR_TEMPLATES) {
        false
    } else {
        current
    }
}

fn expected_occurrences(content: &str) -> Vec<Occurrence> {
    let mut inline_scopes = false;
    let mut occurrences = Vec::new();
    for line in content.split('\n') {
        inline_scopes = inline_scopes_after(line, inline_scopes);
        occurrences.extend(expected_occurrence(line, inline_scopes));
    }
    occurrences
}

/// A multi-file module's expectations: those counted over every class, and those counted over
/// one output file each.
#[derive(Debug, Default, PartialEq, Eq)]
struct MultiFileOccurrences {
    global: Vec<Occurrence>,
    by_output: BTreeMap<String, Vec<Occurrence>>,
}

impl MultiFileOccurrences {
    fn read(&mut self, file: &str, content: &str) -> Result<(), String> {
        let mut inline_scopes = false;
        let mut output: Option<String> = None;
        for line in content.split('\n') {
            inline_scopes = inline_scopes_after(line, inline_scopes);
            if let Some(name) = output_file(line) {
                if self.by_output.contains_key(name) {
                    return Err(format!(
                        "{file}: expected occurrences for output file {name} were already provided"
                    ));
                }
                self.by_output.insert(name.to_string(), Vec::new());
                output = Some(name.to_string());
            }
            if let Some(occurrence) = expected_occurrence(line, inline_scopes) {
                match &output {
                    Some(name) => self.by_output.get_mut(name).unwrap().push(occurrence),
                    None => self.global.push(occurrence),
                }
            }
        }
        Ok(())
    }
}

/// A Kotlin-compiled class of a module: its output path and its text.
struct OutputClass {
    path: String,
    text: String,
}

/// The module's Kotlin classes, in emission order; javac's output in a mixed case is dropped.
fn output_classes(classes: &[(&str, &[u8])]) -> Result<Vec<OutputClass>, CheckError> {
    let bytes: Vec<&[u8]> = classes.iter().map(|(_, bytes)| *bytes).collect();
    let texts = asm_text::textify(&bytes).map_err(|error| match error {
        asm_text::TextifyError::Unreadable(reason) => {
            CheckError::Failed(format!("ASM cannot read the emitted classes: {reason}"))
        }
        asm_text::TextifyError::Server(reason) => CheckError::Unavailable(reason),
    })?;
    Ok(classes
        .iter()
        .zip(texts)
        .filter(|(_, text)| {
            !text
                .source_file
                .as_deref()
                .is_some_and(|file| file.ends_with(".java"))
        })
        .map(|((name, _), text)| OutputClass {
            path: format!("{name}.class"),
            text: text.text,
        })
        .collect())
}

/// `ClassFileFactory.createText`: each class's text after an `@<path>` line.
fn create_text(classes: &[OutputClass], ignored_prefix: Option<&str>) -> String {
    let mut text = String::new();
    for class in classes {
        if ignored_prefix.is_some_and(|prefix| class.path.starts_with(prefix)) {
            continue;
        }
        text.push('@');
        text.push_str(&class.path);
        text.push('\n');
        text.push_str(&class.text);
    }
    text
}

/// `checkGeneratedTextAgainstExpectedOccurrences` under the default configuration (inline scope
/// numbers off). `None` when every counted expectation holds.
fn check_occurrences(text: &str, expected: &[Occurrence]) -> Result<Option<String>, CheckError> {
    let counted: Vec<&Occurrence> = expected
        .iter()
        .filter(|occurrence| !occurrence.inline_scopes)
        .collect();
    let needles: Vec<&str> = counted.iter().map(|o| o.needle.as_str()).collect();
    let counts = asm_text::count_matches(text, &needles).map_err(CheckError::Unavailable)?;
    let mismatches: Vec<String> = counted
        .iter()
        .zip(counts)
        .filter_map(|(occurrence, count)| match count {
            Some(count) if count == occurrence.count => None,
            Some(count) => Some(format!("expected {}, found {count}", occurrence.render())),
            None => Some(format!(
                "`{}` is not a valid Java pattern",
                occurrence.needle
            )),
        })
        .collect();
    Ok((!mismatches.is_empty()).then(|| mismatches.join("; ")))
}

/// Every expectation of `module` against its `classes`. `Ok(None)` when all hold.
fn check_module(
    case: &str,
    module: &TestModule,
    classes: &[(&str, &[u8])],
) -> Result<Option<String>, CheckError> {
    let outputs = output_classes(classes)?;
    let mut problems = Vec::new();
    if module.files.len() > 1 && !krusty::conformance::directive(case, "TREAT_AS_ONE_FILE") {
        let mut expected = MultiFileOccurrences::default();
        for file in &module.files {
            if let Err(error) = expected.read(&file.name, &file.content) {
                problems.push(error);
            }
        }
        if !expected.global.is_empty() {
            problems.extend(check_occurrences(
                &create_text(&outputs, None),
                &expected.global,
            )?);
        }
        for (path, occurrences) in &expected.by_output {
            match outputs.iter().find(|class| &class.path == path) {
                Some(class) => problems.extend(
                    check_occurrences(&class.text, occurrences)?.map(|m| format!("{path}: {m}")),
                ),
                None => problems.push(format!("missing output file {path}, got {}", outputs.len())),
            }
        }
    } else if let Some(file) = module.files.iter().find(|file| !file.additional) {
        problems.extend(check_occurrences(
            &create_text(&outputs, Some(HELPERS_PREFIX)),
            &expected_occurrences(&file.content),
        )?);
    }
    Ok((!problems.is_empty()).then(|| {
        let mut message = problems.join("; ");
        if module.name != box_reference_classes::MAIN_MODULE {
            message = format!("module {}: {message}", module.name);
        }
        message
    }))
}

/// Every module's expectations against the classes a compiler emitted for it.
fn check_case(case: &Case, classes: &CaseClasses<'_>) -> Outcome {
    let mut problems = Vec::new();
    for module in &case.modules {
        let Some(classes) = classes.module(&module.name) else {
            problems.push(format!("module {} emitted no classes", module.name));
            continue;
        };
        match check_module(&case.src, module, classes) {
            Ok(None) => {}
            Ok(Some(problem)) | Err(CheckError::Failed(problem)) => problems.push(problem),
            Err(error @ CheckError::Unavailable(_)) => return error.into_outcome(),
        }
    }
    if problems.is_empty() {
        Outcome::Pass
    } else {
        Outcome::Fail(problems.join("\n"))
    }
}

const SUITE: Suite = Suite {
    directory: "bytecodeText",
    label: "bytecode text",
    inventory: "bytecode_text",
    env: "BYTECODE_TEXT",
    with_reflect: true,
    check: check_case,
};

#[test]
fn krusty_reproduces_kotlincs_bytecode_text_corpus() {
    SUITE.assert_krusty_ratchet();
}

/// Runs the reference kotlinc's classes through the same runner; see
/// [`Suite::assert_reference_reproduces_applicable_cases`]. Ignored in ordinary runs: a cold
/// reference cache compiles the whole corpus with kotlinc. `scripts/conformance-regressions.sh`
/// runs it for every supported release.
#[test]
#[ignore = "compiles the whole bytecode text corpus with the reference kotlinc"]
fn the_reference_compiler_reproduces_every_applicable_bytecode_text_case() {
    SUITE.assert_reference_reproduces_applicable_cases();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_expectation_line_is_a_count_and_a_java_pattern() {
        assert_eq!(
            expected_occurrence("// 2 INVOKESTATIC kotlin/jvm/internal/Intrinsics", false),
            Some(Occurrence {
                count: 2,
                needle: "INVOKESTATIC kotlin/jvm/internal/Intrinsics".to_string(),
                inline_scopes: false,
            })
        );
        assert_eq!(
            expected_occurrence("  //0 L2I", true).map(|o| (o.count, o.needle, o.inline_scopes)),
            Some((0, "L2I".to_string(), true))
        );
        assert_eq!(
            expected_occurrence("// 1", false).map(|o| o.needle),
            Some(String::new())
        );
        assert_eq!(expected_occurrence("// FILE: a.kt", false), None);
        assert_eq!(expected_occurrence("val x = 1 // 2 nope", false), None);
    }

    #[test]
    fn an_output_file_line_names_one_class() {
        assert_eq!(output_file("// @TestKt.class:"), Some("TestKt.class"));
        assert_eq!(output_file("//@a/B.class:"), Some("a/B.class"));
        assert_eq!(output_file("// @TestKt.class"), None);
    }

    #[test]
    fn inline_scope_templates_switch_until_the_next_marker() {
        let occurrences = expected_occurrences(
            "// 1 A\n// JVM_IR_TEMPLATES_WITH_INLINE_SCOPES\n// 2 B\n// JVM_IR_TEMPLATES\n// 3 C\n",
        );
        let flags: Vec<bool> = occurrences.iter().map(|o| o.inline_scopes).collect();
        assert_eq!(flags, [false, true, false]);
    }

    #[test]
    fn multi_file_expectations_split_at_output_files() {
        let mut occurrences = MultiFileOccurrences::default();
        occurrences
            .read("a.kt", "// 1 X\n// @A.class:\n// 2 Y\n")
            .unwrap();
        occurrences.read("b.kt", "// @B.class:\n// 0 Z\n").unwrap();
        assert_eq!(
            occurrences
                .global
                .iter()
                .map(Occurrence::render)
                .collect::<Vec<_>>(),
            ["1 X"]
        );
        assert_eq!(
            occurrences
                .by_output
                .iter()
                .map(|(file, o)| (file.as_str(), o.iter().map(Occurrence::render).collect()))
                .collect::<Vec<(&str, Vec<String>)>>(),
            [
                ("A.class", vec!["2 Y".to_string()]),
                ("B.class", vec!["0 Z".to_string()])
            ]
        );
        assert_eq!(
            occurrences.read("c.kt", "// @A.class:\n"),
            Err("c.kt: expected occurrences for output file A.class were already provided".into())
        );
    }

    #[test]
    fn created_text_heads_each_class_with_its_path_and_skips_helpers() {
        let classes = [
            OutputClass {
                path: "TestKt.class".into(),
                text: "class TestKt\n".into(),
            },
            OutputClass {
                path: "helpers/CoroutineUtilKt.class".into(),
                text: "class helpers\n".into(),
            },
        ];
        assert_eq!(
            create_text(&classes, Some(HELPERS_PREFIX)),
            "@TestKt.class\nclass TestKt\n"
        );
        assert_eq!(
            create_text(&classes, None),
            "@TestKt.class\nclass TestKt\n@helpers/CoroutineUtilKt.class\nclass helpers\n"
        );
    }
}
