use super::common;

const LIB: &str = "package lib\n\
    class Holder<T>(val v: T)\n\
    object Make { fun str(): Holder<String> = Holder(\"hi\") }\n";

fn run(main: &str) -> Option<String> {
    let jdk = common::jdk_modules()?;
    let sl = common::stdlib_jar()?;
    let lo = common::compile_lib("computed_generic", LIB)?;
    common::compile_and_run_box(main, "Main", &[lo, sl], Some(&jdk))
}

#[test]
fn computed_property_keeps_classpath_generic_return_arg() {
    const MAIN: &str = "import lib.Make\n\
        class C { val h get() = Make.str() }\n\
        fun box(): String = if (C().h.v.length == 2) \"OK\" else \"F:\" + C().h.v.length\n";
    assert_eq!(run(MAIN).expect("computed property generic return"), "OK");
}

fn run_source(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

fn assert_source_ok(src: &str) {
    let stdlib = common::stdlib_jar().expect("stdlib jar");
    let jdk = common::jdk_modules();
    let diagnostics = common::front_end_diagnostics(src, &[stdlib], jdk.as_deref());
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    assert_eq!(run_source(src), Some("OK".to_string()));
}

#[test]
fn computed_getter_substitutes_source_generic_member() {
    // `holder.a` is `T` on the DECLARATION but `A` at this receiver (`Holder<A>`): the inferred
    // getter type must substitute the receiver's type arguments — else it collects as the erased
    // `T` (`Any`) and every member read on it fails ("unresolved reference 'x'").
    const SRC: &str = "class A(val x: Int)\n\
class Holder<T>(val a: T)\n\
class B(val holder: Holder<A>) {\n\
    val a get() = holder.a\n\
}\n\
fun box(): String {\n\
    val b = B(Holder(A(41)))\n\
    if (b.a.x + 1 != 42) return \"FAIL x\"\n\
    return \"OK\"\n\
}\n";
    assert_source_ok(SRC);
}

#[test]
fn computed_getter_substitutes_source_generic_member_direct_use() {
    // Direct member use of the substituted type (`String.length`): fails unless the inferred
    // getter type is exactly `String`, not the erased `T`.
    const SRC: &str = "class Holder<T>(val v: T)\n\nclass B(val holder: Holder<String>) {\n\
    val s get() = holder.v\n\
}\n\
fun box(): String = if (B(Holder(\"abcde\")).s.length == 5) \"OK\" else \"FAIL\"\n";
    assert_source_ok(SRC);
}

#[test]
fn computed_getter_substitutes_nested_generic_property_shapes() {
    // `Holder<T>.cell` is not a direct `T` slot: its declared shape is `Cell<T>`. Inference must
    // substitute that complete shape to `Cell<String>`, then apply the same operation again to
    // `Cell<String>.value`. This guards the generic-function-property branch, not only the simpler
    // positional `val value: T` branch.
    const SRC: &str = "class Cell<T>(val value: T)\n\
class Holder<T>(val cell: Cell<T>)\n\
class B(val holder: Holder<String>) {\n\
    val value get() = holder.cell.value\n\
}\n\
fun box(): String = if (B(Holder(Cell(\"shape\"))).value.length == 5) \"OK\" else \"FAIL\"\n";
    assert_source_ok(SRC);
}

#[test]
fn computed_getter_keeps_direct_nullable_scalar_tparam_unsupported() {
    // Specializing a directly stored `T?` to `Int?` requires a new erased-reference ↔ boxed-scalar
    // storage boundary. The generic shape machinery must not silently claim that representation is
    // supported; retain the conservative declaration type until that boxing path exists.
    const SRC: &str = "class Holder<T>(val value: T?)\n\
class B(val holder: Holder<Int>) {\n\
    val value get() = holder.value\n\
}\n\
fun read(): Int {\n\
    val b = B(Holder<Int>(1))\n\
    val n: Int = b.value\n\
    return n\n\
}\n";
    let diagnostics = common::front_end_diagnostics(SRC, &[], None);
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.contains("type mismatch")),
        "direct nullable type-parameter specialization must remain a checked skip; got {diagnostics:?}"
    );
}

#[test]
fn computed_getter_substitutes_inherited_generic_member() {
    // The property is declared on a generic SUPERCLASS: substitution must flow through the
    // hierarchy (`Leaf : Mid<String>` binds `T = String` on `Mid`).
    const SRC: &str = "open class Mid<T>(val v: T)\n\
class Leaf : Mid<String>(\"xyz\")\n\
class B(val leaf: Leaf) {\n\
    val s get() = leaf.v\n\
}\n\
fun box(): String = if (B(Leaf()).s.length == 3) \"OK\" else \"FAIL\"\n";
    assert_source_ok(SRC);
}
