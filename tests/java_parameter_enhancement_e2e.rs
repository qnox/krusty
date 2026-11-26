//! kotlinc enhances a Java parameter with the nullability of the declaration it overrides: a Java
//! class implementing a Kotlin `fun f(x: String)` rejects `null` for `f`, because the override's
//! `String!` becomes the overridden `String`. A parameter that overrides a type variable stays
//! flexible through a Java supertype, which names that argument flexibly (`KG<String!>`), and rigid
//! through a Kotlin one (`KS : KG<String>`, and `KA : KG<Any>`, whose `Any` is as rigid). A pure
//! implementation reaches its mapped collection rigidly, so `ArrayList<String>.add(null)` is
//! rejected. A parameter that states its own head (`String`, `List<T>`) is enhanced wherever its
//! owner is generic, one whose overridden declarations disagree on nullability stays flexible, and
//! a flexible collection keeps its flexible mutability.

use super::common;

const KOTLIN_LIBRARY: &str = "interface KI { fun f(x: String): String }
interface KN { fun f(x: String?): String }
interface KG<T> { fun g(x: String); fun h(x: T); fun l(x: List<T>) }
abstract class KS : KG<String>
abstract class KA : KG<Any>
interface KC { fun c(x: Collection<String>) }
";

const SOURCE: &str = "fun strict(bar: Bar) {
    bar.f(null)
    val value: String = bar.f(\"a\")
}

fun flexible(generic: JG) {
    generic.g(null)
    generic.h(null)
    generic.l(null)
}

fun rigid(intermediate: JS) {
    intermediate.g(null)
    intermediate.h(null)
    intermediate.l(null)
}

fun conflicting(both: Both) {
    both.f(null)
}

fun collection(jc: JC) {
    jc.c(listOf(\"a\"))
    jc.c(mutableListOf(\"a\"))
    jc.c(null)
}

fun anyArgument(a: JA) {
    a.g(null)
    a.h(null)
    a.l(null)
}

fun pure(list: java.util.ArrayList<String>) {
    list.add(null)
}
";

fn libraries() -> Vec<std::path::PathBuf> {
    let kotlin = common::kotlinc_library(KOTLIN_LIBRARY).expect("kotlinc must build the library");
    let java = [
        (
            "Bar.java".to_string(),
            "public class Bar implements KI { public String f(String x) { return x; } }\n"
                .to_string(),
        ),
        (
            "Both.java".to_string(),
            "public class Both implements KI, KN { public String f(String x) { return \"\"; } }\n"
                .to_string(),
        ),
        (
            "JS.java".to_string(),
            "public abstract class JS extends KS { public void g(String x) {} public void h(String x) {} public void l(java.util.List<? extends String> x) {} }\n".to_string(),
        ),
        (
            "JA.java".to_string(),
            "public abstract class JA extends KA { public void g(String x) {} public void h(Object x) {} public void l(java.util.List<? extends Object> x) {} }\n".to_string(),
        ),
        (
            "JC.java".to_string(),
            "public class JC implements KC { public void c(java.util.Collection<String> x) {} }\n".to_string(),
        ),
        (
            "JG.java".to_string(),
            "public class JG implements KG<String> { public void g(String x) {} public void h(String x) {} public void l(java.util.List<? extends String> x) {} }\n".to_string(),
        ),
    ];
    let java_dir = common::javac_compile(&java, std::slice::from_ref(&kotlin))
        .map(|output| output.0)
        .expect("javac must build the Java fixture");
    vec![kotlin, java_dir]
}

fn render(diagnostics: Vec<common::CompilerError>) -> Vec<String> {
    diagnostics
        .iter()
        .map(|diagnostic| {
            format!(
                "{}:{}:{}: {}",
                diagnostic.file, diagnostic.line, diagnostic.column, diagnostic.message
            )
        })
        .collect()
}

#[test]
fn a_java_override_of_a_kotlin_parameter_takes_its_nullability() {
    let result = common::compiler_diagnostics_with_shared_args(
        &[("Use.kt", SOURCE)],
        &libraries(),
        &["-no-reflect".to_string()],
    );
    let krusty_output = format!("{}{}", result.krusty_stderr, result.krusty_stdout);
    let krusty = render(common::compiler_errors(&krusty_output));
    let kotlinc = render(common::compiler_errors(&result.reference_stderr));
    assert_eq!(krusty, kotlinc);
    assert_eq!(
        kotlinc,
        [
            "Use.kt:2:11: null cannot be a value of a non-null type 'String'.",
            "Use.kt:7:15: null cannot be a value of a non-null type 'String'.",
            "Use.kt:9:15: null cannot be a value of a non-null type 'List<out String!>'.",
            "Use.kt:13:20: null cannot be a value of a non-null type 'String'.",
            "Use.kt:14:20: null cannot be a value of a non-null type 'String'.",
            "Use.kt:15:20: null cannot be a value of a non-null type 'List<out String>'.",
            "Use.kt:25:10: null cannot be a value of a non-null type 'Collection<String>'.",
            "Use.kt:29:9: null cannot be a value of a non-null type 'String'.",
            "Use.kt:30:9: null cannot be a value of a non-null type 'Any'.",
            "Use.kt:31:9: null cannot be a value of a non-null type 'List<out Any>'.",
            "Use.kt:35:14: null cannot be a value of a non-null type 'String'.",
        ]
    );
}
