//! A backticked qualifier segment that contains dots names one package edge per dot-separated
//! part, as kotlinc joins a qualifier's segments into one fully qualified name. Protobuf's
//! generated Kotlin DSL qualifies its own package this way (`` `com.example`.FooKt.Dsl ``), in both
//! type and expression position. A part that names no package leaves the whole written segment
//! unresolved, even when an earlier part does (`` a.`b.x` `` with no package `a.b.x`).

use super::common;

const HOLDER: &str = "\
package a.b

const val LIMIT = 3

fun top(): Int = 2

object Holder {
    fun f(): Int = 1

    class Dsl private constructor(private val values: MutableList<Int>) {
        companion object {
            @PublishedApi
            internal fun create(values: MutableList<Int>): Dsl = Dsl(values)
        }

        fun add(value: Int) {
            values += value
        }

        @PublishedApi
        internal fun build(): List<Int> = values
    }
}
";

const LEAF: &str = "\
package a.b.c

object Leaf {
    fun g(): Int = 4
}
";

#[test]
fn a_backticked_dotted_segment_walks_its_packages_like_kotlinc() {
    let classes = common::classes_against_kotlinc_module(&[
        ("Holder.kt", HOLDER),
        ("Leaf.kt", LEAF),
        (
            "Use.kt",
            "package use\n\
             \n\
             inline fun numbers(block: `a.b`.Holder.Dsl.() -> Unit): List<Int> =\n\
             \x20   `a.b`.Holder.Dsl.create(mutableListOf()).apply { block() }.build()\n\
             \n\
             fun sum(): Int =\n\
             \x20   `a.b`.Holder.f() + a.`b.c`.Leaf.g() + `a.b.c`.Leaf.g() + `a.b`.top() + `a.b`.LIMIT\n\
             \n\
             fun listed(): List<Int> = numbers { add(sum()) }\n",
        ),
    ]);
    let differences = classes.differences();
    assert!(
        differences.is_empty(),
        "classes differ from kotlinc's:\n\n{}",
        differences.join("\n\n")
    );
}

#[test]
fn a_backticked_dotted_segment_naming_no_package_is_unresolved_like_kotlinc() {
    let sources = [
        ("Holder.kt", HOLDER),
        (
            "Bad.kt",
            "package bad\n\
             fun bad1(): Int = a.`x.b`.Holder.f()\n\
             fun bad2(): Int = a.`b..x`.Holder.f()\n\
             fun bad3(): Int = a.`b.x`.Holder.f()\n",
        ),
    ];
    let result =
        common::compiler_diagnostics_with_shared_args(&sources, &[common::stdlib_jar()], &[]);
    let render = |diagnostics: &[common::CompilerError]| {
        diagnostics
            .iter()
            .map(|diagnostic| {
                format!(
                    "{}:{}:{}: {}",
                    diagnostic.file, diagnostic.line, diagnostic.column, diagnostic.message
                )
            })
            .collect::<Vec<_>>()
    };
    let expected = [
        "Bad.kt:2:21: unresolved reference 'x.b'.",
        "Bad.kt:3:21: unresolved reference 'b..x'.",
        "Bad.kt:4:21: unresolved reference 'b.x'.",
    ];
    assert_eq!(
        render(&common::compiler_errors(&result.reference_stderr)),
        expected,
        "kotlinc\n{}",
        result.reference_stderr
    );
    let krusty_output = format!("{}{}", result.krusty_stderr, result.krusty_stdout);
    assert_eq!(
        render(&common::compiler_errors(&krusty_output)),
        expected,
        "krusty\n{krusty_output}"
    );
    assert_eq!(
        render(&common::compiler_warnings(&krusty_output)),
        render(&common::compiler_warnings(&result.reference_stderr)),
        "warnings\n{krusty_output}"
    );
    assert_ne!(result.krusty_code, 0);
    assert_ne!(result.reference_code, 0);
}
