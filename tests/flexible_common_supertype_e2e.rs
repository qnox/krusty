//! The common supertype of inferred lower bounds keeps the first of equal types. `MutableList<J>`
//! and `MutableList<J!>` are each a subtype of the other, so kotlinc joins them at whichever came
//! first rather than at the platform form or at an out-projection.

use super::common;
use super::diagnostics_parity_support::errors;

fn java_producer_classpath() -> std::path::PathBuf {
    common::javac_compile(
        &[(
            "p/J.java".to_string(),
            "package p;\n\
             public class J {\n\
             \x20 public static J j() { return null; }\n\
             }\n"
            .to_string(),
        )],
        &[],
    )
    .expect("javac compiles the Java producer")
    .0
}

#[test]
fn equal_lower_bounds_join_at_the_first_one_like_kotlinc() {
    let source = "import p.*\n\
                  \n\
                  interface Out<out T1>\n\
                  \n\
                  fun <T> three(a: Out<T>, b: Out<T>, c: Out<T>): T = null!!\n\
                  fun <T> two(b: Out<T>, c: Out<T>): T = null!!\n\
                  fun <T> out(t: T): Out<MutableList<T>> = null!!\n\
                  \n\
                  fun test(a: Out<Nothing>, b: Out<MutableList<J>>) {\n\
                  \x20   val inferred = three(a, b, out(J.j()))\n\
                  \x20   val first: Int = inferred\n\
                  \x20   val fixed = out(J.j())\n\
                  \x20   val platformFirst: Int = two(fixed, b)\n\
                  \x20   val plainFirst: Int = two(b, fixed)\n\
                  }\n";
    let classpath = [java_producer_classpath(), common::stdlib_jar()];
    let result = common::compiler_diagnostics(&[("EqualBounds.kt", source)], &classpath);
    assert_ne!(result.reference_code, 0, "kotlinc must reject the fixture");
    assert_ne!(result.krusty_code, 0, "krusty must reject the fixture");
    let mut krusty = errors(&result.krusty_stderr);
    krusty.extend(errors(&result.krusty_stdout));
    let reference = errors(&result.reference_stderr);
    assert_eq!(krusty, reference);
    assert_eq!(
        krusty
            .iter()
            .map(|error| (error.line, error.column, error.message.as_str()))
            .collect::<Vec<_>>(),
        [
            (
                11,
                20,
                "initializer type mismatch: expected 'Int', actual 'MutableList<J>'."
            ),
            (
                13,
                28,
                "initializer type mismatch: expected 'Int', actual 'MutableList<J!>'."
            ),
            (
                14,
                25,
                "initializer type mismatch: expected 'Int', actual 'MutableList<J>'."
            ),
        ]
    );
}
