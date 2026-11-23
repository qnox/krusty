//! The type of a collection literal passed for a type variable.
//!
//! kotlinc builds such a literal from the classifier its variable's other bounds name. One
//! classifier builds it; several leave it ambiguous, and the error lists every candidate
//! declaration. Each fixture goes through both compilers and the complete reports are compared:
//! message, candidate listing, position and order.

use super::common;
use common::Reported;

const LANGUAGE: &str = "-XXLanguage:+CollectionLiterals";

/// The ledger of `report` with each message in full: the lines that follow an error up to the next
/// report line belong to it, minus the source echo and caret kotlinc prints under it.
fn complete(report: &str) -> Vec<Reported> {
    let mut ledger = common::reported(report);
    let lines: Vec<&str> = report.lines().collect();
    let starts = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.contains(": error: "))
        .map(|(at, _)| at)
        .collect::<Vec<_>>();
    assert_eq!(starts.len(), ledger.len());
    for (entry, start) in ledger.iter_mut().zip(&starts) {
        let mut block: Vec<&str> = lines[start + 1..]
            .iter()
            .take_while(|line| {
                !line.contains(": error: ")
                    && !line.contains(": warning: ")
                    && !line.starts_with("krusty:")
            })
            .copied()
            .collect();
        while block.last().is_some_and(|line| line.trim().is_empty()) {
            block.pop();
        }
        let is_caret =
            |line: &str| !line.trim().is_empty() && line.chars().all(|c| c == '^' || c == ' ');
        if block.last().is_some_and(|line| is_caret(line)) {
            block.truncate(block.len().saturating_sub(2));
        }
        while block.last().is_some_and(|line| line.trim().is_empty()) {
            block.pop();
        }
        for line in block {
            entry.rendered.push('\n');
            entry.rendered.push_str(line);
        }
    }
    ledger
}

fn assert_same(stem: &str, source: &str, expected_count: usize) {
    let dir = common::scratch_dir().expect("scratch dir");
    let file = dir.join("Main.kt");
    std::fs::write(&file, source).expect("write the fixture");
    let reference_out = dir.join(format!("{stem}-reference"));
    std::fs::create_dir_all(&reference_out).expect("reference output directory");
    let (status, reference) = common::kotlinc_compile(&[
        LANGUAGE.to_string(),
        "-d".to_string(),
        reference_out.to_string_lossy().into_owned(),
        "-cp".to_string(),
        common::stdlib_jar().to_string_lossy().into_owned(),
        file.to_string_lossy().into_owned(),
    ])
    .expect("reference kotlinc available");
    let out = std::process::Command::new(common::krusty_binary())
        .args([LANGUAGE, "-no-stdlib", "-cp"])
        .arg(common::stdlib_jar())
        .arg("-d")
        .arg(dir.join(format!("{stem}-krusty")))
        .arg(&file)
        .output()
        .expect("run krusty");
    let mut krusty = String::from_utf8_lossy(&out.stdout).into_owned();
    krusty.push_str(&String::from_utf8_lossy(&out.stderr));
    assert_eq!(
        status != 0,
        !out.status.success(),
        "the compilers disagree on whether this source is rejected\nreference:\n{reference}\nkrusty:\n{krusty}"
    );
    let (reference, krusty) = (complete(&reference), complete(&krusty));
    assert_eq!(
        reference.len(),
        expected_count,
        "reference ledger: {reference:#?}"
    );
    assert_eq!(reference, krusty);
}

#[test]
fn lower_and_upper_bound_classifiers_leave_a_standard_literal_ambiguous() {
    // `MutableSet<E>` from the first argument and the expected `Set<*>` both declare a factory.
    assert_same(
        "ambiguous_standard",
        "fun <T> select(vararg x: T): T = x[0]\n\
         fun f() {\n\
         \x20   val a: Set<*> = select(mutableSetOf<Int>(), [1])\n\
         }\n",
        1,
    );
}

#[test]
fn a_single_bound_classifier_builds_the_literal() {
    assert_same(
        "single_standard",
        "fun <T> select(vararg x: T): T = x[0]\n\
         fun f() {\n\
         \x20   val a = select(mutableSetOf<Int>(), [1])\n\
         \x20   val b: Set<*> = select(setOf<Int>(), [1])\n\
         \x20   val c = select(setOf<Int>(), [1])\n\
         }\n",
        0,
    );
}

#[test]
fn source_classifiers_are_listed_with_their_supertypes_and_data_modifier() {
    assert_same(
        "ambiguous_source",
        "fun <T> select(vararg x: T): T = x[0]\n\
         open class A<X> { companion object { operator fun <X> of(vararg x: X): A<X> = A() } }\n\
         open class B<X> : A<X>() { companion object { operator fun <X> of(vararg x: X): B<X> = B() } }\n\
         abstract class D { companion object { operator fun of(vararg x: Int): D = TODO() } }\n\
         data class E(val a: Int) : D() { companion object { operator fun of(vararg x: Int): E = TODO() } }\n\
         fun f(b: B<Int>, e: E) {\n\
         \x20   val r1: A<Int> = select(b, [1])\n\
         \x20   val r2: D = select(e, [1])\n\
         }\n",
        2,
    );
}
