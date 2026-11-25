//! A Java class's supertype names its own type variables as flexible arguments (`J<T!>`), so an
//! extension declared on the supertype binds its variable to the flexible `String!` when the
//! receiver is `Sub<String>`. kotlinc then accepts `null` and a platform-typed read, where a
//! receiver that is the supertype itself, `J<String>`, binds the variable to the strict `String`.

use super::common;

const SOURCE: &str = "fun <K> J<K>.take(k: K) {}

fun through(sub: Sub<String>) {
    sub.take(null)
    val value: String = sub.get()
}

fun direct(base: J<String>) {
    base.take(null)
}
";

fn library() -> std::path::PathBuf {
    let java = [
        (
            "J.java".to_string(),
            "public class J<T> { public T get() { return null; } }\n".to_string(),
        ),
        (
            "Sub.java".to_string(),
            "public class Sub<T> extends J<T> {}\n".to_string(),
        ),
    ];
    common::javac_compile(&java, &[])
        .map(|(dir, _)| dir)
        .expect("javac must build the Java fixture")
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
fn a_subclass_receiver_binds_the_variable_of_its_java_supertype_flexibly() {
    let result = common::compiler_diagnostics_with_shared_args(
        &[("Use.kt", SOURCE)],
        &[library()],
        &["-no-reflect".to_string()],
    );
    let krusty_output = format!("{}{}", result.krusty_stderr, result.krusty_stdout);
    let krusty = render(common::compiler_errors(&krusty_output));
    let kotlinc = render(common::compiler_errors(&result.reference_stderr));
    assert_eq!(krusty, kotlinc);
    assert_eq!(
        kotlinc,
        ["Use.kt:9:15: null cannot be a value of a non-null type 'String'."]
    );
}
