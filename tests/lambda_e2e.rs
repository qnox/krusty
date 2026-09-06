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
    common::expect_box_ok_with_stdlib(src, "L");
}

#[test]
fn function_n_lambda_unpacks_high_arity_arguments() {
    let parameter_types = std::iter::repeat_n("Int", 23)
        .collect::<Vec<_>>()
        .join(", ");
    let parameter_names = (0..23)
        .map(|index| format!("p{index}"))
        .collect::<Vec<_>>()
        .join(", ");
    let arguments = (1..=23)
        .map(|value| value.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    let src = format!(
        "fun apply(f: ({parameter_types}) -> Int): Int = f({arguments})\n\
         fun box(): String {{\n\
             val result = apply {{ {parameter_names} -> p0 + p22 }}\n\
             return if (result == 24) \"OK\" else \"fail: $result\"\n\
         }}\n"
    );
    common::expect_box_ok_with_stdlib(&src, "HighArityLambda");
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
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let classes = common::compile_in_process(
        src,
        "LambdaSequence",
        std::slice::from_ref(&stdlib),
        Some(jdk.as_path()),
    )
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

/// Class-initialization lambdas take their impl-method prefix from the DECLARATION, not from
/// whichever function the lowerer visited last: a property initializer uses the property name
/// (`h$lambda$0`) and an `init` block uses kotlinc's `_init_` (`_init_$lambda$0`), each numbering
/// its own sequence. Expected names measured against kotlinc 2.4.10. Before the fix these all took
/// a stale enclosing-function prefix (`member$lambda$0..3`).
#[test]
fn class_init_lambda_impl_methods_use_declaration_prefixes() {
    let src = r#"
fun use(f: () -> Int) = f()

class Holder {
    val h: () -> Int = { 4 }
    var q = 0
    init {
        val x = { 9 }
        q = use(x)
        q += use { 11 }
    }
    fun member(): Int {
        val m = { 5 }
        return m()
    }
}

fun box(): String {
    val holder = Holder()
    val total = holder.h() + holder.q + holder.member()
    return if (total == 29) "OK" else "fail: $total"
}
"#;
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let classes = common::compile_in_process(
        src,
        "InitLambdaPrefix",
        std::slice::from_ref(&stdlib),
        Some(jdk.as_path()),
    )
    .expect("class-init lambdas should compile");
    let bytes = &classes
        .iter()
        .find(|(name, _)| name == "Holder")
        .expect("missing Holder")
        .1;
    let class = parse_class(bytes).expect("invalid Holder");
    let mut names = class
        .methods
        .iter()
        .filter(|method| method.name.contains("$lambda$"))
        .map(|method| method.name.as_str())
        .collect::<Vec<_>>();
    names.sort_unstable();
    assert_eq!(
        names,
        [
            "_init_$lambda$0",
            "_init_$lambda$1",
            "h$lambda$0",
            "member$lambda$0"
        ],
        "Holder synthetic lambda methods"
    );

    let box_class = common::find_box_class(&classes).expect("box method");
    assert_eq!(
        common::run_box(&classes, &box_class, &[stdlib]).as_deref(),
        Some("OK")
    );
}
