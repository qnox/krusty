use super::common;
use super::diagnostics_parity_support::complete_errors;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn this_delegation_targets_sibling_secondary() {
    const SRC: &str = "var log: String = \"\"\n\
        class A() {\n\
        \x20 var prop: String = \"\"\n\
        \x20 init { log += \"i\" }\n\
        \x20 constructor(x: String): this() { prop = x; log += \"s\" }\n\
        \x20 constructor(x: Int): this(x.toString()) { prop += \"#int\"; log += \"n\" }\n\
        }\n\
        fun box(): String {\n\
        \x20 val a1 = A(\"abc\")\n\
        \x20 if (a1.prop != \"abc\" || log != \"is\") return \"fail1: ${a1.prop} $log\"\n\
        \x20 log = \"\"\n\
        \x20 val a2 = A(7)\n\
        \x20 if (a2.prop != \"7#int\" || log != \"isn\") return \"fail2: ${a2.prop} $log\"\n\
        \x20 return \"OK\"\n\
        }\n\
        fun main() { println(box()) }\n";
    assert_eq!(run(SRC).expect("this-delegation to sibling"), "OK");
}

#[test]
fn value_class_secondary_constructor_may_reach_primary_through_a_sibling() {
    const SRC: &str = r#"
        import kotlin.jvm.JvmInline

        @JvmInline
        value class Foo<T : String>(val value: T) {
            constructor(number: Int) : this("OK" as T) {
                if (number == 0) throw IllegalArgumentException()
                if (number == 1) return
                return Unit
            }

            constructor(number: Double) : this(number.toInt())
        }

        fun box(): String = Foo<String>(42.0).value
    "#;

    assert_eq!(
        common::module_front_end_diagnostics(&[("ValueClassSiblingCtor", SRC)]),
        Some(Vec::<String>::new()),
    );
}

#[test]
fn named_this_delegation_is_reordered_for_the_primary() {
    const SRC: &str = "class A(val text: String, val number: Int) {\n\
        \x20 constructor(value: Int): this(number = value, text = \"n\")\n\
        }\n\
        fun box(): String {\n\
        \x20 val value = A(7)\n\
        \x20 return value.text + value.number\n\
        }\n\
        fun main() { println(box()) }\n";
    assert_eq!(run(SRC).expect("named this delegation"), "n7");
}

#[test]
fn named_this_delegation_keeps_source_evaluation_order() {
    const SRC: &str = "var trace = \"\"\n\
        fun mark(value: String): String { trace += value; return value }\n\
        class A(val a: String, val b: String) {\n\
        \x20 constructor(tag: Int): this(b = mark(\"B\"), a = mark(\"A\"))\n\
        }\n\
        fun box(): String {\n\
        \x20 val value = A(0)\n\
        \x20 return value.a + value.b + \"/\" + trace\n\
        }\n";
    assert_eq!(run(SRC).expect("this delegation evaluation order"), "AB/BA");
}

#[test]
fn this_delegation_supports_primary_defaults_and_varargs() {
    const SRC: &str = "class A(val prefix: String = \"D\", vararg val values: String) {\n\
        \x20 constructor(flag: Boolean): this()\n\
        \x20 constructor(count: Int): this(\"P\", \"X\", \"Y\")\n\
        \x20 constructor(prefix: CharSequence): this(prefix = prefix.toString())\n\
        \x20 fun text(): String = prefix + values.joinToString(\"\")\n\
        }\n\
        fun box(): String {\n\
        \x20 val omitted = A(true).text()\n\
        \x20 val expanded = A(2).text()\n\
        \x20 val empty = A(\"E\" as CharSequence).text()\n\
        \x20 return omitted + \"/\" + expanded + \"/\" + empty\n\
        }\n";
    assert_eq!(
        run(SRC).expect("this delegation defaults and varargs"),
        "D/PXY/E"
    );
}

#[test]
fn constructor_delegation_combines_listed_and_spread_varargs() {
    // `Text`'s secondary constructor carries a `marker` parameter it never uses: without it, its JVM
    // signature would be `<init>([Ljava/lang/String;)V` — the same one the `vararg` primary erases to —
    // and kotlinc rejects the class outright with "platform declaration clash". `Numbers` needs no such
    // marker because its two `IntArray` parameters already give it a distinct descriptor.
    const SRC: &str = "class Numbers(vararg val values: Int) {\n\
        \x20 constructor(left: IntArray, right: IntArray): this(0, *left, 3, *right, 6)\n\
        }\n\
        class Text(vararg val values: String) {\n\
        \x20 constructor(middle: Array<String>, marker: Boolean): this(\"a\", *middle, \"d\")\n\
        }\n\
        fun box(): String {\n\
        \x20 val numbers = Numbers(intArrayOf(1, 2), intArrayOf(4, 5))\n\
        \x20 val text = Text(arrayOf(\"b\", \"c\"), true)\n\
        \x20 return numbers.values.joinToString(\"\") + \"/\" + text.values.joinToString(\"\")\n\
        }\n";
    assert_eq!(run(SRC).expect("mixed constructor varargs"), "0123456/abcd");
}

#[test]
fn constructor_delegation_requires_spread_for_an_array_vararg_argument() {
    // An unspread array is one element of the vararg, so kotlinc reports the ordinary argument
    // mismatch at it, exactly as it would at a call.
    const SRC: &str = "class A(vararg val values: Int) {\n\
        \x20 constructor(values: IntArray, marker: Boolean): this(values)\n\
        }\n";
    let sources = [("Main.kt", SRC)];
    let reference_args = common::language_directives::kotlinc_args(SRC);
    assert_eq!(
        common::reference_error_blocks(&sources, &reference_args),
        ["Main.kt:2:56: argument type mismatch: actual type is 'IntArray', but 'Int' was expected."]
    );
    common::assert_error_blocks_match_kotlinc(&sources, &reference_args);
}

#[test]
fn constructor_delegation_cycle_is_diagnosed() {
    const SRC: &str = "class A {\n\
        \x20 constructor(value: Int): this(value.toString())\n\
        \x20 constructor(value: String): this(value.isNotEmpty())\n\
        \x20 constructor(value: Boolean): this(if (value) 1 else 0)\n\
        }\n";
    let stdlib = common::stdlib_jar();
    let result = common::compiler_diagnostics(&[("Cycle.kt", SRC)], std::slice::from_ref(&stdlib));
    assert_ne!(result.reference_code, 0, "kotlinc accepted the delegation");
    assert_ne!(result.krusty_code, 0, "krusty accepted the delegation");
    let observed = complete_errors(&result.krusty_stderr, false);
    let expected = complete_errors(&result.reference_stderr, true);
    assert!(
        !expected.is_empty(),
        "kotlinc reported no error:\n{}",
        result.reference_stderr
    );
    assert_eq!(
        observed, expected,
        "inspection diagnostics differ from kotlinc.\nkrusty:\n{}\nkotlinc:\n{}",
        result.krusty_stderr, result.reference_stderr
    );
}

#[test]
fn secondary_constructor_defaults_work_for_calls_and_delegation() {
    const SRC: &str = "var trace = \"\"\n\
        class Token {\n\
        \x20 private constructor(value: String = \"OK\") { trace += value }\n\
        \x20 constructor(code: Int): this()\n\
        \x20 companion object { fun create(): Token = Token() }\n\
        }\n\
        fun box(): String {\n\
        \x20 Token.create()\n\
        \x20 Token(1)\n\
        \x20 return if (trace == \"OKOK\") \"OK\" else trace\n\
        }\n";
    assert_eq!(run(SRC).expect("secondary constructor defaults"), "OK");
}

#[test]
fn secondary_constructor_vararg_is_an_array_in_its_body() {
    const SRC: &str = "class A {\n\
        \x20 var count: Int = -1\n\
        \x20 constructor(vararg values: String) { count = values.size }\n\
        }\n\
        fun box(): String = if (A(\"a\", \"b\").count == 2) \"OK\" else \"FAIL\"\n";
    assert_eq!(run(SRC).expect("secondary constructor vararg"), "OK");
}

#[test]
fn branchy_delegation_arguments_keep_this_uninitialized_in_frames() {
    const SRC: &str = "class Label(val text: String) {\n\
        \x20 constructor(ok: Boolean): this(\"O\" + if (ok) \"K\" else \"X\")\n\
        }\n\
        fun box(): String {\n\
        \x20 val yes = Label(true).text\n\
        \x20 val no = Label(false).text\n\
        \x20 return if (yes == \"OK\" && no == \"OX\") \"OK\" else yes + no\n\
        }\n";
    assert_eq!(run(SRC).expect("branchy constructor delegation"), "OK");
}

#[test]
fn ambiguous_this_delegation_is_diagnosed() {
    const SRC: &str = "interface Left\n\
        interface Right\n\
        class Both : Left, Right\n\
        class A() {\n\
        \x20 constructor(value: Left): this()\n\
        \x20 constructor(value: Right): this()\n\
        \x20 constructor(value: Both, tag: Int): this(value)\n\
        }\n";
    let stdlib = common::stdlib_jar();
    let diagnostics =
        common::front_end_diagnostics(SRC, &[stdlib], Some(common::jdk_modules().as_path()));
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.contains("ambigu")),
        "expected constructor ambiguity diagnostic, got {diagnostics:?}"
    );
}

#[test]
fn a_constructor_argument_reaches_the_companion_not_the_instance() {
    // A delegation argument runs BEFORE the instance exists, so the receiver installed
    // nearest for it is the target's companion tower — a rung backed by no `this` slot.
    // Selecting it and then materializing "the current receiver" loads the half-built
    // instance, which the JVM verifier rejects as `uninitializedThis`; the receiver has to
    // be named as the singleton it is. An enclosing `object`, and a companion reached from
    // an `inner` class's superclass argument, are the same rung under different spellings.
    const SRC: &str =
        "open class Base(val result: String)\n\
         object Holder {\n\
         \x20   fun tag(): String = \"T\"\n\
         \x20   class Nested : Base(tag())\n\
         }\n\
         class Companioned(val result: Int) {\n\
         \x20   companion object {\n\
         \x20       fun foo(): Int = 1\n\
         \x20       val prop = 2\n\
         \x20       const val C = 3\n\
         \x20   }\n\
         \x20   constructor() : this(foo() + prop + C)\n\
         }\n\
         open class Outer(val fn: (() -> String)?) {\n\
         \x20   companion object { val ok = \"OK\" }\n\
         \x20   val ok = \"Fail: Outer.ok\"\n\
         \x20   inner class Inner : Outer({ ok })\n\
         }\n\
         fun box(): String {\n\
         \x20   if (Companioned().result != 6) return \"fail: companion delegation ${Companioned().result}\"\n\
         \x20   if (Holder.Nested().result != \"T\") return \"fail: enclosing object\"\n\
         \x20   val captured = Outer(null).Inner().fn?.invoke()\n\
         \x20   if (captured != \"OK\") return \"fail: captured ${captured}\"\n\
         \x20   return \"OK\"\n\
         }\n";
    assert_eq!(run(SRC).as_deref(), Some("OK"));
    // The receiver a delegation argument denotes is a claim about what kotlinc emits, so run the
    // same program under the reference compiler where it is provisioned; CI provisions it.
    if let Some(out) = common::kotlinc_library(SRC) {
        assert_eq!(
            common::run_box(&[], "LibKt", &[out, common::stdlib_jar()]).as_deref(),
            Some("OK"),
            "reference compiler"
        );
    }
}

const UNRESOLVED_SECONDARY: &str = "\
class Box(val item: Int) {
    constructor(x: Missing) : this(0)
}
";

#[test]
fn an_unresolved_secondary_constructor_parameter_matches_kotlinc() {
    let stdlib = common::stdlib_jar();
    let result = common::compiler_diagnostics(
        &[("Secondary.kt", UNRESOLVED_SECONDARY)],
        std::slice::from_ref(&stdlib),
    );
    assert_ne!(result.reference_code, 0, "kotlinc accepted the constructor");
    assert_ne!(result.krusty_code, 0, "krusty accepted the constructor");
    let observed = complete_errors(&result.krusty_stderr, false);
    let expected = complete_errors(&result.reference_stderr, true);
    assert!(
        !expected.is_empty(),
        "kotlinc reported no error:\n{}",
        result.reference_stderr
    );
    assert_eq!(
        observed, expected,
        "inspection diagnostics differ from kotlinc.\nkrusty:\n{}\nkotlinc:\n{}",
        result.krusty_stderr, result.reference_stderr
    );
}

const SUPERTYPE_WITHOUT_PRIMARY: &str = "\
class Box : Missing() {
    constructor(x: Int) : this()
}
";

const HEADER_CONSTRUCTOR_WITHOUT_SECONDARY: &str = "\
class Box : Missing()
";

#[test]
fn a_supertype_initialization_without_a_primary_constructor_matches_kotlinc() {
    let stdlib = common::stdlib_jar();
    let result = common::compiler_diagnostics(
        &[("Supertype.kt", SUPERTYPE_WITHOUT_PRIMARY)],
        std::slice::from_ref(&stdlib),
    );
    assert_ne!(result.reference_code, 0, "kotlinc accepted the supertype");
    assert_ne!(result.krusty_code, 0, "krusty accepted the supertype");
    let observed = complete_errors(&result.krusty_stderr, false);
    let expected = complete_errors(&result.reference_stderr, true);
    assert!(
        !expected.is_empty(),
        "kotlinc reported no error:\n{}",
        result.reference_stderr
    );
    assert_eq!(
        observed, expected,
        "inspection diagnostics differ from kotlinc.\nkrusty:\n{}\nkotlinc:\n{}",
        result.krusty_stderr, result.reference_stderr
    );
}

#[test]
fn a_supertype_initialization_without_a_secondary_constructor_matches_kotlinc() {
    let stdlib = common::stdlib_jar();
    let result = common::compiler_diagnostics(
        &[("HeaderOnly.kt", HEADER_CONSTRUCTOR_WITHOUT_SECONDARY)],
        std::slice::from_ref(&stdlib),
    );
    assert_ne!(result.reference_code, 0, "kotlinc accepted the supertype");
    assert_ne!(result.krusty_code, 0, "krusty accepted the supertype");
    let observed = complete_errors(&result.krusty_stderr, false);
    let expected = complete_errors(&result.reference_stderr, true);
    assert!(
        !expected.is_empty(),
        "kotlinc reported no error:\n{}",
        result.reference_stderr
    );
    assert_eq!(
        observed, expected,
        "inspection diagnostics differ from kotlinc.\nkrusty:\n{}\nkotlinc:\n{}",
        result.krusty_stderr, result.reference_stderr
    );
}

const MISSING_DELEGATION: &str = "\
class Box {
    constructor(x: Int) : this()
}

open class Base(val item: Int)
class Child : Base {
    constructor(item: Int) : super()
}
";

#[test]
fn a_missing_constructor_delegation_argument_matches_kotlinc() {
    let stdlib = common::stdlib_jar();
    let result = common::compiler_diagnostics(
        &[("Delegation.kt", MISSING_DELEGATION)],
        std::slice::from_ref(&stdlib),
    );
    assert_ne!(result.reference_code, 0, "kotlinc accepted the delegation");
    assert_ne!(result.krusty_code, 0, "krusty accepted the delegation");
    let observed = complete_errors(&result.krusty_stderr, false);
    let expected = complete_errors(&result.reference_stderr, true);
    assert!(
        !expected.is_empty(),
        "kotlinc reported no error:\n{}",
        result.reference_stderr
    );
    assert_eq!(
        observed, expected,
        "inspection diagnostics differ from kotlinc.\nkrusty:\n{}\nkotlinc:\n{}",
        result.krusty_stderr, result.reference_stderr
    );
}

const EXCESS_DELEGATION: &str = "\
class Box {
    constructor(x: Int) : this(1, 2)
}

open class Base(val x: Int)
class Child : Base {
    constructor() : super(1, 2)
}
";

#[test]
fn an_excess_constructor_delegation_argument_matches_kotlinc() {
    let stdlib = common::stdlib_jar();
    let result = common::compiler_diagnostics(
        &[("Excess.kt", EXCESS_DELEGATION)],
        std::slice::from_ref(&stdlib),
    );
    assert_ne!(result.reference_code, 0, "kotlinc accepted the delegation");
    assert_ne!(result.krusty_code, 0, "krusty accepted the delegation");
    let observed = complete_errors(&result.krusty_stderr, false);
    let expected = complete_errors(&result.reference_stderr, true);
    assert!(
        !expected.is_empty(),
        "kotlinc reported no error:\n{}",
        result.reference_stderr
    );
    assert_eq!(
        observed, expected,
        "inspection diagnostics differ from kotlinc.\nkrusty:\n{}\nkotlinc:\n{}",
        result.krusty_stderr, result.reference_stderr
    );
}

const SELF_DELEGATION: &str = "\
class Box {
    constructor() : this()
}

class Boxed(val item: Int) {
    constructor() : this()
}

class Value {
    constructor(x: Int) : this(1)
}
";

const DELEGATION_TYPE: &str = "\
class Box {
    constructor(x: Int) : this(\"a\")
}
";

const INAPPLICABLE_DELEGATION: &str = "\
class Box {
    constructor(x: Int) : this()
    constructor(y: String) : this()
}

class Boxed(val item: Int) {
    constructor(y: String) : this()
}
";

#[test]
fn inapplicable_constructor_delegations_match_kotlinc() {
    let stdlib = common::stdlib_jar();
    let result = common::compiler_diagnostics(
        &[("Inapplicable.kt", INAPPLICABLE_DELEGATION)],
        std::slice::from_ref(&stdlib),
    );
    assert_ne!(result.reference_code, 0, "kotlinc accepted the delegation");
    assert_ne!(result.krusty_code, 0, "krusty accepted the delegation");
    let observed = complete_errors(&result.krusty_stderr, false);
    let expected = complete_errors(&result.reference_stderr, true);
    assert!(
        !expected.is_empty(),
        "kotlinc reported no error:\n{}",
        result.reference_stderr
    );
    assert_eq!(
        observed, expected,
        "inspection diagnostics differ from kotlinc.\nkrusty:\n{}\nkotlinc:\n{}",
        result.krusty_stderr, result.reference_stderr
    );
}

#[test]
fn a_delegation_argument_type_mismatch_matches_kotlinc() {
    let stdlib = common::stdlib_jar();
    let result = common::compiler_diagnostics(
        &[("Mismatch.kt", DELEGATION_TYPE)],
        std::slice::from_ref(&stdlib),
    );
    assert_ne!(result.reference_code, 0, "kotlinc accepted the delegation");
    assert_ne!(result.krusty_code, 0, "krusty accepted the delegation");
    let observed = complete_errors(&result.krusty_stderr, false);
    let expected = complete_errors(&result.reference_stderr, true);
    assert!(
        !expected.is_empty(),
        "kotlinc reported no error:\n{}",
        result.reference_stderr
    );
    assert_eq!(
        observed, expected,
        "inspection diagnostics differ from kotlinc.\nkrusty:\n{}\nkotlinc:\n{}",
        result.krusty_stderr, result.reference_stderr
    );
}

#[test]
fn a_self_delegation_cycle_matches_kotlinc() {
    let stdlib = common::stdlib_jar();
    let result = common::compiler_diagnostics(
        &[("Cycle.kt", SELF_DELEGATION)],
        std::slice::from_ref(&stdlib),
    );
    assert_ne!(result.reference_code, 0, "kotlinc accepted the delegation");
    assert_ne!(result.krusty_code, 0, "krusty accepted the delegation");
    let observed = complete_errors(&result.krusty_stderr, false);
    let expected = complete_errors(&result.reference_stderr, true);
    assert!(
        !expected.is_empty(),
        "kotlinc reported no error:\n{}",
        result.reference_stderr
    );
    assert_eq!(
        observed, expected,
        "inspection diagnostics differ from kotlinc.\nkrusty:\n{}\nkotlinc:\n{}",
        result.krusty_stderr, result.reference_stderr
    );
}

const EXCESS_TYPE: &str = "\
class Box {
    constructor(x: List<String>) : this(1, 2)
}
";

const DEFAULT_SELF_CYCLE: &str = "\
class Box(val item: Int = 0) {
    constructor() : this()
}
";

const NAMED_DELEGATION: &str = "\
class Box {
    constructor(x: Int, y: Int) : this(y = 1)
    constructor(z: String) : this(y = 1)
}
";

const SINGLE_NAMED_DELEGATION: &str = "\
class Box {
    constructor(x: Int) : this(y = 1)
}
";

const TIED_DELEGATION: &str = "\
class Box(val item: Int = 0) {
    constructor(x: String = \"\") : this()
}
";

const SPECIFIC_PARAMETER_DELEGATION: &str = "\
class Box(val item: Number = 0) {
    constructor(x: Int = 1) : this(1)
}
";

const GENERIC_PRIMARY_DELEGATION: &str = "\
class X<T>(val t: T) {
    constructor(t: String): this(t)
}
";

const FORWARDED_INT_TO_NUMBER: &str = "\
class B(val x: Number) {
    constructor(x: Int) : this(x)
}
";

#[test]
fn a_named_delegation_argument_lists_every_candidate() {
    let stdlib = common::stdlib_jar();
    let result = common::compiler_diagnostics(
        &[("Named.kt", NAMED_DELEGATION)],
        std::slice::from_ref(&stdlib),
    );
    assert_ne!(result.reference_code, 0, "kotlinc accepted the delegation");
    assert_ne!(result.krusty_code, 0, "krusty accepted the delegation");
    let observed = complete_errors(&result.krusty_stderr, false);
    let expected = complete_errors(&result.reference_stderr, true);
    assert!(
        !expected.is_empty(),
        "kotlinc reported no error:\n{}",
        result.reference_stderr
    );
    assert_eq!(
        observed, expected,
        "inspection diagnostics differ from kotlinc.\nkrusty:\n{}\nkotlinc:\n{}",
        result.krusty_stderr, result.reference_stderr
    );
}

#[test]
fn a_more_specific_self_delegation_is_a_cycle() {
    let stdlib = common::stdlib_jar();
    let result = common::compiler_diagnostics(
        &[("DefaultCycle.kt", DEFAULT_SELF_CYCLE)],
        std::slice::from_ref(&stdlib),
    );
    assert_ne!(result.reference_code, 0, "kotlinc accepted the delegation");
    assert_ne!(result.krusty_code, 0, "krusty accepted the delegation");
    let observed = complete_errors(&result.krusty_stderr, false);
    let expected = complete_errors(&result.reference_stderr, true);
    assert!(
        !expected.is_empty(),
        "kotlinc reported no error:\n{}",
        result.reference_stderr
    );
    assert_eq!(
        observed, expected,
        "inspection diagnostics differ from kotlinc.\nkrusty:\n{}\nkotlinc:\n{}",
        result.krusty_stderr, result.reference_stderr
    );
}

#[test]
fn a_single_constructor_named_delegation_matches_kotlinc() {
    let stdlib = common::stdlib_jar();
    let result = common::compiler_diagnostics(
        &[("SingleNamed.kt", SINGLE_NAMED_DELEGATION)],
        std::slice::from_ref(&stdlib),
    );
    assert_ne!(result.reference_code, 0, "kotlinc accepted the delegation");
    assert_ne!(result.krusty_code, 0, "krusty accepted the delegation");
    let observed = complete_errors(&result.krusty_stderr, false);
    let expected = complete_errors(&result.reference_stderr, true);
    assert!(
        !expected.is_empty(),
        "kotlinc reported no error:\n{}",
        result.reference_stderr
    );
    assert_eq!(
        observed, expected,
        "inspection diagnostics differ from kotlinc.\nkrusty:\n{}\nkotlinc:\n{}",
        result.krusty_stderr, result.reference_stderr
    );
}

#[test]
fn a_tied_constructor_delegation_is_ambiguous() {
    let stdlib = common::stdlib_jar();
    let result = common::compiler_diagnostics(
        &[("Tied.kt", TIED_DELEGATION)],
        std::slice::from_ref(&stdlib),
    );
    assert_ne!(result.reference_code, 0, "kotlinc accepted the delegation");
    assert_ne!(result.krusty_code, 0, "krusty accepted the delegation");
    let observed = complete_errors(&result.krusty_stderr, false);
    let expected = complete_errors(&result.reference_stderr, true);
    assert!(
        !expected.is_empty(),
        "kotlinc reported no error:\n{}",
        result.reference_stderr
    );
    assert_eq!(
        observed, expected,
        "inspection diagnostics differ from kotlinc.\nkrusty:\n{}\nkotlinc:\n{}",
        result.krusty_stderr, result.reference_stderr
    );
}

#[test]
fn a_forwarded_int_parameter_to_a_number_primary_matches_kotlinc() {
    let stdlib = common::stdlib_jar();
    let result = common::compiler_diagnostics(
        &[("ForwardedInt.kt", FORWARDED_INT_TO_NUMBER)],
        std::slice::from_ref(&stdlib),
    );
    assert_ne!(result.reference_code, 0, "kotlinc accepted the delegation");
    assert_ne!(result.krusty_code, 0, "krusty accepted the delegation");
    let observed = complete_errors(&result.krusty_stderr, false);
    let expected = complete_errors(&result.reference_stderr, true);
    assert!(
        !expected.is_empty(),
        "kotlinc reported no error:\n{}",
        result.reference_stderr
    );
    assert_eq!(
        observed, expected,
        "inspection diagnostics differ from kotlinc.\nkrusty:\n{}\nkotlinc:\n{}",
        result.krusty_stderr, result.reference_stderr
    );
}

#[test]
fn a_generic_primary_parameter_delegation_is_a_cycle() {
    let stdlib = common::stdlib_jar();
    let result = common::compiler_diagnostics(
        &[("Generic.kt", GENERIC_PRIMARY_DELEGATION)],
        std::slice::from_ref(&stdlib),
    );
    assert_ne!(result.reference_code, 0, "kotlinc accepted the delegation");
    assert_ne!(result.krusty_code, 0, "krusty accepted the delegation");
    let observed = complete_errors(&result.krusty_stderr, false);
    let expected = complete_errors(&result.reference_stderr, true);
    assert!(
        !expected.is_empty(),
        "kotlinc reported no error:\n{}",
        result.reference_stderr
    );
    assert_eq!(
        observed, expected,
        "inspection diagnostics differ from kotlinc.\nkrusty:\n{}\nkotlinc:\n{}",
        result.krusty_stderr, result.reference_stderr
    );
}

#[test]
fn a_more_specific_parameter_self_delegation_is_a_cycle() {
    let stdlib = common::stdlib_jar();
    let result = common::compiler_diagnostics(
        &[("Specific.kt", SPECIFIC_PARAMETER_DELEGATION)],
        std::slice::from_ref(&stdlib),
    );
    assert_ne!(result.reference_code, 0, "kotlinc accepted the delegation");
    assert_ne!(result.krusty_code, 0, "krusty accepted the delegation");
    let observed = complete_errors(&result.krusty_stderr, false);
    let expected = complete_errors(&result.reference_stderr, true);
    assert!(
        !expected.is_empty(),
        "kotlinc reported no error:\n{}",
        result.reference_stderr
    );
    assert_eq!(
        observed, expected,
        "inspection diagnostics differ from kotlinc.\nkrusty:\n{}\nkotlinc:\n{}",
        result.krusty_stderr, result.reference_stderr
    );
}

fn constructor_delegation_matches_kotlinc(file: &str, source: &str) {
    let stdlib = common::stdlib_jar();
    let result = common::compiler_diagnostics(&[(file, source)], std::slice::from_ref(&stdlib));
    let observed = complete_errors(&result.krusty_stderr, false);
    let expected = complete_errors(&result.reference_stderr, true);
    assert_eq!(
        (result.krusty_code == 0, observed),
        (result.reference_code == 0, expected),
        "inspection diagnostics differ from kotlinc.\nkrusty:\n{}\nkotlinc:\n{}",
        result.krusty_stderr,
        result.reference_stderr
    );
}

const FORWARDED_STRING_TO_ANY: &str = "\
class A(x: Any) {
    constructor(x: String) : this(x)
}
";

const FORWARDED_STRING_CAST_TO_ANY: &str = "\
class A(x: Any) {
    constructor(x: String) : this(x as Any)
}
";

const NOTHING_DELEGATED_TO_INT: &str = "\
class F(val x: Int) {
    constructor(x: Long) : this(TODO())
}
";

const NOTHING_DELEGATED_TO_LONG: &str = "\
class F(val x: Long) {
    constructor(x: Int) : this(TODO())
}
";

const THREE_CONSTRUCTOR_TIE: &str = "\
class Box(val item: Int = 0) {
    constructor(x: String = \"\") : this()
    constructor(x: Long, y: Long) : this()
}
";

const ARITY_BESIDE_TYPE_MISMATCH: &str = "\
class Box(val x: String) {
    constructor(a: Int, b: Int) : this(1)
}
";

const TYPE_MISMATCHES_OMIT_ARITY: &str = "\
class Box(val x: String) {
    constructor(flag: Boolean) : this(flag.toString())
    constructor(a: Int, b: Int) : this(1)
}
";

#[test]
fn a_string_forwarded_to_any_is_a_cycle() {
    constructor_delegation_matches_kotlinc("Any.kt", FORWARDED_STRING_TO_ANY);
}

#[test]
fn a_string_cast_to_any_delegates() {
    constructor_delegation_matches_kotlinc("AnyCast.kt", FORWARDED_STRING_CAST_TO_ANY);
}

const NOTHING_AMONG_THREE_CONSTRUCTORS: &str = "\
class F(val x: Int) {
    constructor(x: Long) : this(TODO())
    constructor(x: Byte) : this(TODO())
}
";

const NOTHING_BESIDE_A_STRING: &str = "\
class F(val x: Int, val y: String) {
    constructor(x: Long, y: String) : this(TODO(), \"a\")
}
";

#[test]
fn a_nothing_argument_prefers_the_int_constructor() {
    constructor_delegation_matches_kotlinc("NothingInt.kt", NOTHING_DELEGATED_TO_INT);
}

#[test]
fn a_nothing_argument_prefers_int_among_three_constructors() {
    constructor_delegation_matches_kotlinc("NothingThree.kt", NOTHING_AMONG_THREE_CONSTRUCTORS);
}

#[test]
fn a_nothing_argument_beside_a_string_prefers_int() {
    constructor_delegation_matches_kotlinc("NothingMixed.kt", NOTHING_BESIDE_A_STRING);
}

#[test]
fn a_nothing_argument_that_selects_the_delegating_constructor_is_a_cycle() {
    constructor_delegation_matches_kotlinc("NothingLong.kt", NOTHING_DELEGATED_TO_LONG);
}

#[test]
fn an_ambiguous_delegation_names_only_the_tied_constructors() {
    constructor_delegation_matches_kotlinc("Three.kt", THREE_CONSTRUCTOR_TIE);
}

#[test]
fn an_arity_failure_beside_one_type_mismatch_reports_the_mismatch() {
    constructor_delegation_matches_kotlinc("Arity.kt", ARITY_BESIDE_TYPE_MISMATCH);
}

#[test]
fn type_mismatches_omit_the_constructor_that_never_mapped() {
    constructor_delegation_matches_kotlinc("Mismatches.kt", TYPE_MISMATCHES_OMIT_ARITY);
}

#[test]
fn an_excess_delegation_also_reports_the_mapped_type_mismatch() {
    let stdlib = common::stdlib_jar();
    let result = common::compiler_diagnostics(
        &[("ExcessType.kt", EXCESS_TYPE)],
        std::slice::from_ref(&stdlib),
    );
    assert_ne!(result.reference_code, 0, "kotlinc accepted the delegation");
    assert_ne!(result.krusty_code, 0, "krusty accepted the delegation");
    let observed = complete_errors(&result.krusty_stderr, false);
    let expected = complete_errors(&result.reference_stderr, true);
    assert!(
        !expected.is_empty(),
        "kotlinc reported no error:\n{}",
        result.reference_stderr
    );
    assert_eq!(
        observed, expected,
        "inspection diagnostics differ from kotlinc.\nkrusty:\n{}\nkotlinc:\n{}",
        result.krusty_stderr, result.reference_stderr
    );
}
