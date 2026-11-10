//! Kotlin's bytecode-listing corpus, `compiler/testData/codegen/bytecodeListing`, compiled by
//! krusty.
//!
//! Each case `<name>.kt` comes with `<name>.txt`: every generated class's declarations, flags,
//! signatures and annotations as JetBrains' `BytecodeListingTextCollectingVisitor` renders them.
//! Releases that still carry K1 dumps keep the FIR one in `<name>.fir.txt`, which the FIR test
//! reads in preference.
//! That is `AbstractFirLightTreeBytecodeListingTest`; this runner mirrors its
//! `BytecodeListingHandler`:
//!
//! - Each module's Kotlin classes are read without code, sorted by output path, and rendered with
//!   `WITH_SIGNATURES`, `IGNORE_ANNOTATIONS` and `DONT_SORT_DECLARATIONS` applied; the coroutine
//!   `helpers/` classes are left out. The listings join with a blank line and end with a newline.
//! - A case with several modules heads each module's dump with `Module: <name>`, as
//!   `MultiModuleInfoDumper` does; a single module's dump has no header.
//! - The dump and the `.txt` file are compared after `assertEqualsToDump`'s normalization:
//!   surrounding whitespace trimmed, line separators converted, trailing whitespace removed from
//!   each line, and one final newline.
//!
//! Only Kotlin's classes are listed: a mixed case's javac output is not in the reference compiler's
//! class factory, and the harness's coroutine `support` module is not one of the case's modules.
//! The ratchet is `bytecode_corpus`'s, under `tests/bytecode_listing_expected_*`, blessed with
//! `KRUSTY_BLESS_BYTECODE_LISTING_EXPECTATIONS=1` and focused with `KRUSTY_BYTECODE_LISTING_ONLY`.

use super::asm_text::{self, ListingOptions};
use super::bytecode_corpus::{Case, CaseClasses, CheckError, ClassFile, Outcome, Suite};

/// The listing of one module: its Kotlin classes, sorted by output path, joined by blank lines.
fn module_listing(
    classes: &[ClassFile<'_>],
    options: ListingOptions,
) -> Result<String, CheckError> {
    let bytes: Vec<&[u8]> = classes.iter().map(|(_, bytes)| *bytes).collect();
    let mut listings = asm_text::listing(&bytes, options).map_err(|error| match error {
        asm_text::TextifyError::Unreadable(reason) => {
            CheckError::Failed(format!("ASM cannot read the emitted classes: {reason}"))
        }
        asm_text::TextifyError::Server(reason) => CheckError::Unavailable(reason),
    })?;
    listings.retain(|listing| {
        !listing
            .source_file
            .as_deref()
            .is_some_and(|file| file.ends_with(".java"))
            && !listing.name.starts_with("helpers/")
    });
    // By output path, `<name>.class`: `A$B.class` sorts before `A.class`.
    listings.sort_by_cached_key(|listing| format!("{}.class", listing.name));
    let mut dump = listings
        .iter()
        .map(|listing| listing.text.as_str())
        .collect::<Vec<_>>()
        .join("\n\n");
    dump.push('\n');
    Ok(dump)
}

/// `MultiModuleInfoDumper.generateResultingDump` over each module's dump, in module order.
fn resulting_dump(modules: &[(&str, String)]) -> String {
    let mut dump = String::new();
    if let [(_, only)] = modules {
        dump.push_str(only);
    } else {
        for (name, module) in modules {
            dump.push_str(&format!("Module: {name}\n"));
            dump.push_str(module);
        }
    }
    if !dump.is_empty() && !dump.ends_with('\n') {
        dump.push('\n');
    }
    dump
}

/// `assertEqualsToDump`'s normalization of both sides.
fn sanitize(text: &str) -> String {
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    let trimmed = text.trim_matches(|c: char| c <= ' ');
    let mut out = String::new();
    for line in trimmed.split('\n') {
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

/// The first line on which `actual` departs from `expected`, for the failure report.
fn first_difference(expected: &str, actual: &str) -> String {
    let mut expected_lines = expected.lines();
    let mut actual_lines = actual.lines();
    for line in 1.. {
        match (expected_lines.next(), actual_lines.next()) {
            (Some(e), Some(a)) if e == a => continue,
            (None, None) => break,
            (e, a) => {
                return format!(
                    "line {line}: expected `{}`, found `{}`",
                    e.unwrap_or("<end>"),
                    a.unwrap_or("<end>")
                )
            }
        }
    }
    "the dumps differ".to_string()
}

fn check_case(case: &Case, classes: &CaseClasses<'_>) -> Outcome {
    let options = ListingOptions {
        with_signatures: krusty::conformance::directive(&case.src, "WITH_SIGNATURES"),
        with_annotations: !krusty::conformance::directive(&case.src, "IGNORE_ANNOTATIONS"),
        sort_declarations: !krusty::conformance::directive(&case.src, "DONT_SORT_DECLARATIONS"),
    };
    let mut dumps = Vec::new();
    for module in case.modules.iter().filter(|module| !module.additional) {
        let Some(classes) = classes.module(&module.name) else {
            return Outcome::Fail(format!("module {} emitted no classes", module.name));
        };
        match module_listing(classes, options) {
            Ok(dump) => dumps.push((module.name.as_str(), dump)),
            Err(error) => return error.into_outcome(),
        }
    }
    // The FIR golden file wins where a release still carries one beside the K1 dump.
    let expected_path = Some(case.path.with_extension("fir.txt"))
        .filter(|fir| fir.exists())
        .unwrap_or_else(|| case.path.with_extension("txt"));
    let expected = match std::fs::read_to_string(&expected_path) {
        Ok(expected) => expected,
        Err(error) => {
            return Outcome::Unavailable(format!("read {}: {error}", expected_path.display()))
        }
    };
    let expected = sanitize(&expected);
    let actual = sanitize(&resulting_dump(&dumps));
    if expected == actual {
        Outcome::Pass
    } else {
        Outcome::Fail(first_difference(&expected, &actual))
    }
}

const SUITE: Suite = Suite {
    directory: "bytecodeListing",
    label: "bytecode listing",
    inventory: "bytecode_listing",
    env: "BYTECODE_LISTING",
    with_reflect: false,
    check: check_case,
};

#[test]
fn krusty_reproduces_kotlincs_bytecode_listing_corpus() {
    SUITE.assert_krusty_ratchet();
}

/// Runs the reference kotlinc's classes through the same runner; see
/// [`Suite::assert_reference_reproduces_applicable_cases`]. Ignored in ordinary runs: a cold
/// reference cache compiles the whole corpus with kotlinc. `scripts/conformance-regressions.sh`
/// runs it for every supported release.
#[test]
#[ignore = "compiles the whole bytecode listing corpus with the reference kotlinc"]
fn the_reference_compiler_reproduces_every_applicable_bytecode_listing_case() {
    SUITE.assert_reference_reproduces_applicable_cases();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_single_module_dump_has_no_header() {
        assert_eq!(
            resulting_dump(&[("main", "class A\n".to_string())]),
            "class A\n"
        );
    }

    #[test]
    fn a_multi_module_dump_heads_each_module() {
        assert_eq!(
            resulting_dump(&[
                ("lib", "class A\n".to_string()),
                ("main", "class B\n".to_string())
            ]),
            "Module: lib\nclass A\nModule: main\nclass B\n"
        );
    }

    #[test]
    fn dumps_compare_without_surrounding_or_trailing_whitespace() {
        assert_eq!(sanitize("\n class A {  \r\n}\n\n"), "class A {\n}\n");
    }

    #[test]
    fn the_first_difference_names_its_line() {
        assert_eq!(
            first_difference("a\nb\n", "a\nc\n"),
            "line 2: expected `b`, found `c`"
        );
        assert_eq!(
            first_difference("a\n", "a\nb\n"),
            "line 2: expected `<end>`, found `b`"
        );
    }
}
