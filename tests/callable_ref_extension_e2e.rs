//! Callable references to EXTENSION functions: unbound `Type::ext` and bound `obj::ext`. Both lower to
//! a `FunctionReferenceImpl` calling the lifted static extension — unbound via `Static` (receiver is the
//! first invoke param), bound via `StaticBound` (receiver captured, passed as the first static arg).
//! Both carry real reference EQUALITY. Round-tripped on the JVM.

use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn unbound_extension_reference_call() {
    const SRC: &str = "class A { var result = \"Fail\" }\n\
fun A.foo() { result = \"OK\" }\n\
fun box(): String {\n\
    val a = A()\n\
    val x = A::foo\n\
    x(a)\n\
    return a.result\n\
}\n";
    assert_eq!(run(SRC).expect("unbound A::ext call"), "OK");
}

#[test]
fn bound_extension_reference_call() {
    const SRC: &str = "class A(val v: String)\n\
fun A.foo(suffix: String): String = v + suffix\n\
fun box(): String {\n\
    val a = A(\"O\")\n\
    val g = a::foo\n\
    return g(\"K\")\n\
}\n";
    assert_eq!(run(SRC).expect("bound obj::ext call"), "OK");
}

#[test]
fn extension_reference_equality() {
    // Unbound refs to the same extension are equal; bound refs on the same receiver are equal; a bound
    // and an unbound ref are NOT equal.
    const SRC: &str = "class Foo\n\
fun Foo.ext(): Unit {}\n\
fun box(): String {\n\
    val foo = Foo()\n\
    if (Foo::ext != Foo::ext) return \"f1\"\n\
    if (foo::ext != foo::ext) return \"f2\"\n\
    if (foo::ext == Foo::ext) return \"f3\"\n\
    return \"OK\"\n\
}\n";
    assert_eq!(run(SRC).expect("ext ref equality"), "OK");
}

#[test]
fn bound_extension_reference_selects_the_most_specific_receiver_overload() {
    const SRC: &str = "interface JsonParser\n\
interface JsonCodingParser : JsonParser\n\
var result = \"FAIL\"\n\
fun JsonCodingParser.parseValue(source: String): Any = source\n\
fun JsonParser.parseValue(source: String): Any = \"wrong\"\n\
fun decodeWith(decode: (String) -> Any) { result = decode(\"OK\") as String }\n\
fun box(): String {\n\
    val parser: JsonCodingParser = object : JsonCodingParser {}\n\
    decodeWith(parser::parseValue)\n\
    return result\n\
}\n";
    assert_eq!(
        run(SRC).expect("most-specific bound extension callable reference"),
        "OK"
    );
}

#[test]
fn bound_extension_reference_uses_the_expected_function_parameter_types() {
    const SRC: &str = "class Parser\n\
fun Parser.decode(source: String): String = source\n\
fun Parser.decode(value: Int): String = \"wrong\"\n\
fun box(): String {\n\
    val parser = Parser()\n\
    val decode: (String) -> String = parser::decode\n\
    return decode(\"OK\")\n\
}\n";
    assert_eq!(
        run(SRC).expect("expected-type-selected bound extension callable reference"),
        "OK"
    );
}

#[test]
fn bound_extension_reference_uses_function_type_variance() {
    const SRC: &str = "class Parser\n\
fun Parser.decode(value: Any): String = value as String\n\
fun Parser.decode(value: Int): Int = value\n\
fun box(): String {\n\
    val parser = Parser()\n\
    val decode: (String) -> Any = parser::decode\n\
    return decode(\"OK\") as String\n\
}\n";
    assert_eq!(
        run(SRC).expect("variance-compatible bound extension callable reference"),
        "OK"
    );
}

#[test]
fn bound_extension_reference_boxes_a_primitive_for_a_reference_supertype_parameter() {
    const SRC: &str = "class Parser\n\
fun Parser.decode(value: Number): String = if (value.toInt() == 1) \"OK\" else \"FAIL\"\n\
fun box(): String {\n\
    val parser = Parser()\n\
    val decode: (Int) -> String = parser::decode\n\
    return decode(1)\n\
}\n";
    assert_eq!(
        run(SRC).expect("boxed variance-compatible bound extension callable reference"),
        "OK"
    );
}

#[test]
fn bound_extension_reference_coerces_a_value_return_to_unit() {
    const SRC: &str = "class Parser(var result: String)\n\
fun Parser.decode(value: String): String { result = value; return value }\n\
fun box(): String {\n\
    val parser = Parser(\"FAIL\")\n\
    val decode: (String) -> Unit = parser::decode\n\
    decode(\"OK\")\n\
    return parser.result\n\
}\n";
    assert_eq!(
        run(SRC).expect("Unit-coerced bound extension callable reference"),
        "OK"
    );
}

#[test]
fn bound_extension_reference_selects_the_most_specific_compatible_parameter_overload() {
    const SRC: &str = "class Parser\n\
fun Parser.decode(value: Any): Any = \"wrong\"\n\
fun Parser.decode(value: CharSequence): Any = value\n\
fun box(): String {\n\
    val parser = Parser()\n\
    val decode: (String) -> Any = parser::decode\n\
    return decode(\"OK\") as String\n\
}\n";
    assert_eq!(
        run(SRC).expect("most-specific compatible bound extension callable reference"),
        "OK"
    );
}

#[test]
fn bound_extension_return_exactness_does_not_hide_a_more_specific_parameter_overload() {
    const SRC: &str = "class Parser\n\
fun Parser.pick(value: Any): Any = \"wrong\"\n\
fun Parser.pick(value: CharSequence): String = value.toString()\n\
fun box(): String {\n\
    val parser = Parser()\n\
    val selected: (String) -> Any = parser::pick\n\
    return selected(\"OK\") as String\n\
}\n";
    assert_eq!(
        run(SRC).expect("unbiased most-specific bound extension callable reference"),
        "OK"
    );
}

#[test]
fn full_arity_bound_extension_reference_keeps_defaulted_parameter() {
    const SRC: &str = "class Parser\n\
fun Parser.decode(value: String = \"wrong\"): String = value\n\
fun box(): String {\n\
    val parser = Parser()\n\
    val decode: (String) -> String = parser::decode\n\
    return decode(\"OK\")\n\
}\n";
    assert_eq!(
        run(SRC).expect("full-arity defaulted bound extension callable reference"),
        "OK"
    );
}

#[test]
fn inferred_bound_extension_reference_keeps_default_adaptation() {
    const SRC: &str = "class Parser\n\
fun Parser.decode(value: String = \"OK\"): String = value\n\
fun use(parser: Parser) { val decode = parser::decode }\n";
    let Some(diagnostics) = common::checker_diags_with_stdlib(SRC) else {
        return;
    };
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
}
