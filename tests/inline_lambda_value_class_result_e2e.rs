//! A library inline call (`let`, `run`, `with`) whose lambda produces a value class is expanded in
//! place: the lambda's body becomes the call's value, typed by the call's result. That value must
//! keep the representation the call's type gives it. krusty boxed it as if it crossed a
//! `FunctionN.invoke`, so `fun f(s: String): Id = s.let { Id(it) }` returned the box from a method
//! declared to return the carrier, and `map[k]?.let { Id(it) }` cast that box to `String`.

use super::common;

const SRC: &str = r#"@JvmInline value class Id(val value: String)

class Registry {
    private val ids = mapOf("a" to "x")
    fun find(slug: String): Id? = ids[slug]?.let { Id(it) }
}

fun viaLet(s: String): Id = s.let { Id(it) }
fun viaSafeLet(s: String?): Id? = s?.let { Id(it) }
fun viaRun(s: String): Id = s.run { Id(this) }
fun viaWith(s: String): Id = with(s) { Id(this) }
fun stored(s: String?): Id? {
    val id = s?.let { Id(it) }
    return id
}

fun box(): String {
    if (Registry().find("a") != Id("x")) return "find"
    if (Registry().find("b") != null) return "find missing"
    if (viaLet("l").value != "l") return "let"
    if (viaSafeLet("s") != Id("s") || viaSafeLet(null) != null) return "safe let"
    if (viaRun("r").value != "r") return "run"
    if (viaWith("w").value != "w") return "with"
    if (stored("v") != Id("v")) return "stored"
    return "OK"
}
"#;

#[test]
fn an_inlined_lambda_value_class_result_keeps_the_call_representation() {
    let classes = common::classes_against_kotlinc_module(&[("InlineLambdaValueClass.kt", SRC)]);
    assert_eq!(
        classes.krusty.keys().collect::<Vec<_>>(),
        classes.reference.keys().collect::<Vec<_>>()
    );
    for (class, reference) in &classes.reference {
        assert!(
            classes.krusty[class] == *reference,
            "{class} differs from kotlinc"
        );
    }
}

#[test]
fn an_inlined_lambda_value_class_result_runs() {
    common::expect_box_same_as_kotlinc(SRC, "InlineLambdaValueClassResult");
}
