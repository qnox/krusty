//! A call whose named arguments reorder it stores each argument in a temporary in source order,
//! except those kotlinc passes in place: a constant (folded ones included), a read of a `val`, a
//! parameter or `this`, a function literal, an unbound reference and an unbound class literal. A
//! scalar stored for a reference parameter is boxed where it is passed, not before it is stored. A
//! bound reference evaluates its receiver where it is written, so it is stored. Constructor
//! delegations follow the same rules.

use super::common;

const NAMED: &str = "object Marker
var counter = 0
val shared = 3
fun next(): Int { counter += 1; return counter }
fun take(first: Int, second: Any?): Int = first
class Holder(val label: String) {
    fun pass(): Int = take(second = this, first = next())
    fun member(): Int = label.length
}
fun make(): Holder { counter += 100; return Holder(\"m\") }
open class Base(val first: Int, val second: Any?) {
    constructor(label: String, first: Int) : this(second = next() + 1, first = first + label.length)
}
class Derived : Base(second = next() + 1, first = next())
class Delegating : Base(\"y\", next())
fun references(): Int {
    var total = take(second = make()::member, first = next())
    total += take(second = Holder::member, first = next())
    return total
}
fun reordered(parameter: Int): Int {
    val fixed = \"v\"
    var changing = \"w\"
    changing += \"x\"
    var total = take(second = \"n\", first = next())
    total += take(second = fixed, first = next())
    total += take(second = changing, first = next())
    total += take(second = parameter, first = next())
    total += take(second = Marker, first = next())
    total += take(second = null, first = next())
    total += take(second = 1 + 2, first = next())
    total += take(second = \"x$fixed\", first = next())
    total += take(second = { parameter }, first = next())
    total += take(second = String::class, first = next())
    total += take(second = shared, first = next())
    total += take(second = 'c', first = next())
    total += references()
    total += Derived().first + Delegating().first
    return total + Holder(\"h\").pass()
}
fun box(): String {
    val total = reordered(4)
    return if (total == 658 && counter == 119) \"OK\" else \"$total,$counter\"
}
";

#[test]
fn reordered_named_arguments_store_only_what_kotlinc_stores() {
    let sources = [("Named.kt", NAMED)];
    for class in ["Holder", "Base", "Derived", "Delegating"] {
        let pair = common::ModuleClassPair::compile(&sources, class);
        assert!(pair.krusty == pair.kotlinc, "{class} differs from kotlinc");
    }
    let facade = common::ModuleClassPair::compile(&sources, "NamedKt");
    for function in ["reordered", "references"] {
        let (kotlinc, krusty) = facade.method_code("NamedKt", function);
        assert_eq!(krusty, kotlinc, "{function}");
    }
}

#[test]
fn reordered_named_arguments_evaluate_in_source_order() {
    common::expect_box_same_as_kotlinc(NAMED, "NamedOperands");
}

const CONSTRUCTOR_ARRAY: &str = r#"class Element
class ArrayHolder(val first: String, val second: Array<Element>)
fun make(): ArrayHolder {
    return ArrayHolder(second = arrayOf(Element()), first = "OK")
}
fun box(): String {
    val value = make()
    return if (value.first == "OK" && value.second.size == 1) "OK" else "bad"
}
"#;

#[test]
fn reordered_constructor_array_arguments_match_complete_artifacts() {
    let classes =
        common::classes_against_kotlinc_module(&[("ConstructorArray.kt", CONSTRUCTOR_ARRAY)]);
    assert_eq!(classes.differences(), Vec::<String>::new());
}

#[test]
fn reordered_constructor_array_arguments_run_like_kotlin() {
    common::expect_box_same_as_kotlinc(CONSTRUCTOR_ARRAY, "ConstructorArray");
}

const EFFECTFUL_CONSTRUCTOR_ARRAY: &str = r#"var events = ""
class EffectElement(val label: String) {
    init { events += label }
}
class EffectHolder(val first: String, val second: Array<EffectElement>) {
    init { events += "h" }
}
fun label(): String { events += "s"; return "OK" }
fun box(): String {
    val value = EffectHolder(second = arrayOf(EffectElement("a"), EffectElement("b")), first = label())
    return if (value.first == "OK" && value.second.size == 2 && events == "absh") "OK" else events
}
"#;

#[test]
fn effectful_constructor_array_operands_keep_their_complete_artifacts() {
    let classes = common::classes_against_kotlinc_module(&[(
        "EffectfulConstructorArray.kt",
        EFFECTFUL_CONSTRUCTOR_ARRAY,
    )]);
    assert_eq!(classes.differences(), Vec::<String>::new());
}

#[test]
fn effectful_constructor_array_operands_run_once_in_source_order() {
    assert_eq!(
        common::kotlinc_box_result(EFFECTFUL_CONSTRUCTOR_ARRAY),
        "OK"
    );
    common::expect_box_same_as_kotlinc(EFFECTFUL_CONSTRUCTOR_ARRAY, "EffectfulConstructorArray");
}

const WIDE_OPERAND: &str = r#"class WideElement
fun wide(): Long {
    val elements = arrayOf(WideElement())
    return elements.size.toLong()
}
fun choose(first: String, second: Long): Long = second
fun box(): String = if (choose(second = wide(), first = "OK") == 1L) "OK" else "bad"
"#;

#[test]
fn wide_named_operand_storage_matches_complete_artifacts() {
    let classes = common::classes_against_kotlinc_module(&[("WideOperand.kt", WIDE_OPERAND)]);
    assert_eq!(classes.differences(), Vec::<String>::new());
}

#[test]
fn wide_named_operand_storage_runs_like_kotlin() {
    common::expect_box_same_as_kotlinc(WIDE_OPERAND, "WideOperand");
}

const COPIED_CONSTRUCTOR_ARRAY: &str = r#"class CopiedElement
class CopiedHolder(val first: String, val second: Array<CopiedElement>)
inline fun copiedHolder(): CopiedHolder =
    CopiedHolder(second = arrayOf(CopiedElement()), first = "OK")
fun box(): String {
    val value = copiedHolder()
    return if (value.first == "OK" && value.second.size == 1) "OK" else "bad"
}
"#;

#[test]
fn copied_argument_order_holders_match_complete_artifacts() {
    let classes = common::classes_against_kotlinc_module(&[(
        "CopiedConstructorArray.kt",
        COPIED_CONSTRUCTOR_ARRAY,
    )]);
    assert_eq!(classes.differences(), Vec::<String>::new());
}

#[test]
fn copied_argument_order_holders_run_like_kotlin() {
    common::expect_box_same_as_kotlinc(COPIED_CONSTRUCTOR_ARRAY, "CopiedConstructorArray");
}

const INLINE_ARRAY_PARAMETER: &str = r#"class ParameterElement
inline fun <T> readFirst(items: Array<T>, action: (T) -> String): String = action(items[0])
fun box(): String = readFirst(arrayOf(ParameterElement())) { "OK" }
"#;

#[test]
fn inline_array_parameters_match_complete_artifacts() {
    let classes = common::classes_against_kotlinc_module(&[(
        "InlineArrayParameter.kt",
        INLINE_ARRAY_PARAMETER,
    )]);
    assert_eq!(classes.differences(), Vec::<String>::new());
}

#[test]
fn inline_array_parameters_run_like_kotlin() {
    common::expect_box_same_as_kotlinc(INLINE_ARRAY_PARAMETER, "InlineArrayParameter");
}
