//! The result type of a companion `operator fun of`, which a collection literal calls.
//!
//! Each fixture goes through kotlinc and krusty and the complete reports are compared: message,
//! position and order all come from the reference compiler.

use super::common;
use common::Reported;

const LANGUAGE: &str = "-XXLanguage:+CollectionLiterals";

/// Compile `files` with both compilers; return both complete error ledgers.
fn both(stem: &str, files: &[(&str, &str)]) -> (Vec<Reported>, Vec<Reported>) {
    let dir = common::scratch_dir().expect("scratch dir");
    let sources: Vec<std::path::PathBuf> = files
        .iter()
        .map(|(name, text)| {
            let path = dir.join(name);
            std::fs::write(&path, text).expect("write a fixture file");
            path
        })
        .collect();
    let reference_out = dir.join(format!("{stem}-reference"));
    std::fs::create_dir_all(&reference_out).expect("reference output directory");
    let mut reference_args = vec![
        LANGUAGE.to_string(),
        "-d".to_string(),
        reference_out.to_string_lossy().into_owned(),
        "-cp".to_string(),
        common::stdlib_jar().to_string_lossy().into_owned(),
    ];
    reference_args.extend(
        sources
            .iter()
            .map(|source| source.to_string_lossy().into_owned()),
    );
    let (status, reference) =
        common::kotlinc_compile(&reference_args).expect("reference kotlinc available");
    let out = std::process::Command::new(common::krusty_binary())
        .args([LANGUAGE, "-no-stdlib", "-cp"])
        .arg(common::stdlib_jar())
        .arg("-d")
        .arg(dir.join(format!("{stem}-krusty")))
        .args(&sources)
        .output()
        .expect("run krusty");
    let mut krusty = String::from_utf8_lossy(&out.stdout).into_owned();
    krusty.push_str(&String::from_utf8_lossy(&out.stderr));
    assert_eq!(
        status != 0,
        !out.status.success(),
        "the compilers disagree on whether this source is rejected\nreference:\n{reference}\nkrusty:\n{krusty}"
    );
    (common::reported(&reference), common::reported(&krusty))
}

fn assert_same(stem: &str, files: &[(&str, &str)], expected_count: usize) {
    let (reference, krusty) = both(stem, files);
    assert_eq!(reference.len(), expected_count, "reference ledger: {reference:#?}");
    assert_eq!(reference, krusty);
}

#[test]
fn nullable_result_is_reported_explicit_or_inferred() {
    assert_same(
        "of_nullable",
        &[(
            "Main.kt",
            "class Explicit { companion object {\n\
             \x20   operator fun of(vararg args: String): Explicit? = null\n\
             } }\n\
             class Implicit { companion object {\n\
             \x20   fun nullable(): Implicit? = null\n\
             \x20   operator fun of(vararg args: String) = nullable()\n\
             } }\n\
             class GenericExplicit<T> { companion object {\n\
             \x20   operator fun of(vararg args: String): GenericExplicit<String>? = null\n\
             } }\n",
        )],
        3,
    );
}

#[test]
fn results_must_match_the_vararg_overload() {
    assert_same(
        "of_inconsistent",
        &[(
            "Main.kt",
            "class A { companion object {\n\
             \x20   operator fun of(vararg s: String): A = A()\n\
             \x20   operator fun of(x: String): A? = null\n\
             } }\n\
             class B<T> { companion object {\n\
             \x20   operator fun of(vararg s: String): B<String> = B()\n\
             \x20   operator fun of(x: String): B<String> = B()\n\
             } }\n\
             class S { companion object {\n\
             \x20   operator fun of(vararg s: String): S? = null\n\
             \x20   operator fun of(x: String): S? = null\n\
             } }\n",
        )],
        4,
    );
}

#[test]
fn platform_inferred_result_is_potentially_nullable() {
    assert_same(
        "of_platform",
        &[
            (
                "Intermediate.java",
                "public class Intermediate {\n\
                 \x20   static <T> T flexible() { return null; }\n\
                 }\n",
            ),
            (
                "Main.kt",
                "class KotlinClass { companion object {\n\
                 \x20   operator fun of() = Intermediate.flexible<KotlinClass>()\n\
                 \x20   operator fun of(vararg ps: String): KotlinClass = KotlinClass()\n\
                 } }\n\
                 class ReversedClass { companion object {\n\
                 \x20   operator fun of(vararg ss: String) = Intermediate.flexible<ReversedClass>()\n\
                 } }\n",
            ),
        ],
        2,
    );
}

#[test]
fn overloads_with_their_own_type_parameters_are_not_compared_by_identity() {
    assert_same(
        "of_generic",
        &[(
            "Main.kt",
            "class MyList<T> { companion object {\n\
             \x20   operator fun <T> of(vararg vs: T): MyList<T> = MyList()\n\
             \x20   operator fun <T> of(v: T): MyList<T> = MyList()\n\
             } }\n",
        )],
        0,
    );
}
