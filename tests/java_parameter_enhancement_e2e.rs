//! kotlinc enhances a Java parameter with the nullability of the declaration it overrides: a Java
//! class implementing a Kotlin `fun f(x: String)` rejects `null` for `f`, because the override's
//! `String!` becomes the overridden `String`. A parameter that overrides a type variable stays
//! flexible, since a Java supertype names that argument flexibly (`KG<String!>`).

use super::common;

const KOTLIN_LIBRARY: &str = "interface KI { fun f(x: String): String }
interface KG<T> { fun h(x: T) }
";

const SOURCE: &str = "fun strict(bar: Bar) {
    bar.f(null)
    val value: String = bar.f(\"a\")
}

fun flexible(generic: JG) {
    generic.h(null)
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
            "JG.java".to_string(),
            "public class JG implements KG<String> { public void h(String x) {} }\n".to_string(),
        ),
    ];
    let (java_dir, _) = common::javac_compile(&java, std::slice::from_ref(&kotlin))
        .map(|output| (output.0, ()))
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
        ["Use.kt:2:11: null cannot be a value of a non-null type 'String'."]
    );
}
