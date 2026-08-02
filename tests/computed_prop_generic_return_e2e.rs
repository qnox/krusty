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

fn run_files(sources: &[(&str, &str)]) -> Option<String> {
    common::compile_and_run_files_with_stdlib(sources)
}

#[test]
fn cross_file_generic_member_read_uses_erased_accessor() {
    // `holder.a` in file B on `Holder` from file A: the sibling class has no classfile, so the
    // accessor descriptor must come from the DECLARATION (erased `()Ljava/lang/Object;` + a
    // checkcast to the substituted `A`), never from the read's logical type (`()LA;` →
    // NoSuchMethodError). Exercised through a computed getter and a plain function, in both file
    // orders.
    const FILE_A: &str = "class A(val x: Int)\nclass Holder<T>(val a: T)\n";
    const FILE_B: &str = "class B(val holder: Holder<A>) {\n\
    val a get() = holder.a\n\
}\n\
fun f(h: Holder<A>): Int = h.a.x\n\
fun box(): String {\n\
    val b = B(Holder(A(41)))\n\
    if (b.a.x + 1 != 42) return \"FAIL getter\"\n\
    if (f(Holder(A(6))) != 6) return \"FAIL fun\"\n\
    return \"OK\"\n\
}\n";
    assert_eq!(
        run_files(&[("fileA.kt", FILE_A), ("fileB.kt", FILE_B)]),
        Some("OK".to_string()),
        "declaration file first"
    );
    assert_eq!(
        run_files(&[("fileB.kt", FILE_B), ("fileA.kt", FILE_A)]),
        Some("OK".to_string()),
        "use site file first"
    );
}

#[test]
fn cross_file_primitive_property_safe_call_descriptor() {
    // A safe-call read of a cross-file PRIMITIVE property: the node's logical type is `Int?`
    // (`Ljava/lang/Integer;`) but the accessor is `()I` — the descriptor must come from the
    // declaration or the call is a `NoSuchMethodError`.
    const FILE_A: &str = "class C(val x: Int)\n";
    const FILE_B: &str = "fun box(): String {\n\
    val c: C? = C(41)\n\
    val x = c?.x ?: -1\n\
    return if (x + 1 == 42) \"OK\" else \"FAIL $x\"\n\
}\n";
    assert_eq!(
        run_files(&[("fileA.kt", FILE_A), ("fileB.kt", FILE_B)]),
        Some("OK".to_string())
    );
}

#[test]
fn cross_file_generic_member_write_roundtrip() {
    // The write path stamps the erased setter descriptor the same way.
    const FILE_A: &str = "class A(val x: Int)\nclass Box<T>(var v: T)\n";
    const FILE_B: &str = "fun box(): String {\n\
    val b = Box(A(1))\n\
    b.v = A(41)\n\
    return if (b.v.x + 1 == 42) \"OK\" else \"FAIL\"\n\
}\n";
    assert_eq!(
        run_files(&[("fileA.kt", FILE_A), ("fileB.kt", FILE_B)]),
        Some("OK".to_string())
    );
}

#[test]
fn computed_getter_retry_converges_same_class_chain() {
    // `a` reads `b`, `b` reads the later-collected `Holder`: the retry must see the sibling
    // resolved by an earlier round, in either file order.
    const FILE_A: &str = "class A(val x: Int)\nclass Holder<T>(val a: T)\n";
    const FILE_B: &str = "class C(val h: Holder<A>) {\n\
    val b get() = h.a\n\
    val a get() = b\n\
}\n\
fun box(): String = if (C(Holder(A(5))).a.x == 5) \"OK\" else \"FAIL\"\n";
    assert_eq!(
        run_files(&[("fileA.kt", FILE_A), ("fileB.kt", FILE_B)]),
        Some("OK".to_string()),
        "declaration file first"
    );
    assert_eq!(
        run_files(&[("fileB.kt", FILE_B), ("fileA.kt", FILE_A)]),
        Some("OK".to_string()),
        "use site file first"
    );
}
