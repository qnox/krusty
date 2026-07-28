//! Non-capturing lambdas `{ a -> … }` passed to a function-typed parameter, lowered to
//! `invokedynamic` + `LambdaMetafactory` producing a `kotlin/jvm/functions/FunctionN`, then invoked
//! through `FunctionN.invoke`. Round-tripped against the JVM under `-Xverify:all`.

use super::common;
use krusty::jvm::classreader::parse_class;

#[test]
fn lambdas_run() {
    let src = "fun call1(f: (Int) -> Int, x: Int): Int = f(x)\n\
fun call0(f: () -> Int): Int = f()\n\
fun call2(f: (Int, Int) -> Int): Int = f(20, 22)\n\
fun box(): String {\n\
if (call1({ n -> n + 1 }, 41) != 42) return \"f1\"\n\
if (call0({ 7 }) != 7) return \"f2\"\n\
if (call1({ it * 2 }, 41) != 82) return \"f3\"\n\
if (call2({ a, b -> a + b }) != 42) return \"f4\"\n\
return \"OK\"\n\
}\n";
    common::assert_box_ok_with_stdlib(src, "L");
}

#[test]
fn same_named_declarations_share_lambda_sequence() {
    let src = r#"
fun consume(block: () -> String): String = block()

val shared: String = consume { "O" }
fun shared(): String = consume { "K" }

class Sample {
    val member: String = consume { "O" }
    fun member(): String = consume { "K" }
}

fun overloaded(value: Int): String = consume { if (value == 1) "O" else "X" }
fun overloaded(value: String): String = consume { if (value == "K") "K" else "X" }

fun box(): String {
    if (shared + shared() != "OK") return "property"
    if (Sample().member + Sample().member() != "OK") return "member"
    if (overloaded(1) + overloaded("K") != "OK") return "overload"
    return "OK"
}
"#;
    let Some(stdlib) = common::stdlib_jar() else {
        return;
    };
    let Some(jdk) = common::jdk_modules() else {
        return;
    };
    let classes = common::compile_in_process(src, "LambdaSequence", &[stdlib.clone()], Some(&jdk))
        .expect("same-named declarations should compile");

    for (class_name, expected) in [
        (
            "LambdaSequenceKt",
            &[
                "overloaded$lambda$0",
                "overloaded$lambda$1",
                "shared$lambda$0",
                "shared$lambda$1",
            ][..],
        ),
        ("Sample", &["member$lambda$0", "member$lambda$1"][..]),
    ] {
        let bytes = &classes
            .iter()
            .find(|(name, _)| name == class_name)
            .unwrap_or_else(|| panic!("missing {class_name}"))
            .1;
        let class = parse_class(bytes).unwrap_or_else(|_| panic!("invalid {class_name}"));
        let mut names = class
            .methods
            .iter()
            .filter(|method| method.name.contains("$lambda$"))
            .map(|method| method.name.as_str())
            .collect::<Vec<_>>();
        names.sort_unstable();
        assert_eq!(names, expected, "{class_name} synthetic lambda methods");
    }

    let box_class = common::find_box_class(&classes).expect("box method");
    assert_eq!(
        common::run_box(&classes, &box_class, &[stdlib]).as_deref(),
        Some("OK")
    );
}
