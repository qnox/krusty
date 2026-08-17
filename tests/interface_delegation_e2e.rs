//! Interface delegation `class C(a: I) : I by a` where the delegate `a` is a NON-`val` constructor
//! parameter — kotlinc synthesizes a `private final $$delegate_N` field holding it, stored in the ctor,
//! and forwards each interface method through it. Round-tripped under `-Xverify:all`.

use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "C")
}

#[test]
fn nonval_param_single_delegation() {
    const SRC: &str = "interface T1 { fun foo(): String }\n\
class Impl : T1 { override fun foo() = \"OK\" }\n\
class C(a: T1) : T1 by a\n\
fun box(): String = C(Impl()).foo()\n";
    assert_eq!(run(SRC).expect("non-val delegation compiles + runs"), "OK");
}

#[test]
fn nonval_param_multiple_delegations() {
    const SRC: &str = "interface A { fun a(): String }\n\
interface B { fun b(): String }\n\
class IA : A { override fun a() = \"a\" }\n\
class IB : B { override fun b() = \"b\" }\n\
class C(x: A, y: B) : A by x, B by y\n\
fun box(): String { val c = C(IA(), IB()); return if (c.a() + c.b() == \"ab\") \"OK\" else \"fail\" }\n";
    assert_eq!(
        run(SRC).expect("multiple non-val delegations compile + run"),
        "OK"
    );
}

#[test]
fn delegation_forwards_same_name_overloads() {
    const SRC: &str = "interface A { fun foo(value: String): String }\n\
interface B { fun foo(value: Any): Any }\n\
class IA : A { override fun foo(value: String) = \"O\" }\n\
class IB : B { override fun foo(value: Any): Any = \"K\" }\n\
class C(val a: A, val b: B) : A by a, B by b\n\
fun box(): String { val c = C(IA(), IB()); return c.foo(\"\") + c.foo(1) }\n";
    assert_eq!(run(SRC).expect("delegated overloads compile + run"), "OK");
}

/// Delegation to an interface that EXTENDS another interface (`Second : First`, `C : Second by s`):
/// the forwarders must cover `First`'s inherited methods too, not just `Second`'s own — otherwise the
/// inherited method stays abstract (an `AbstractMethodError`).
#[test]
fn delegation_forwards_inherited_super_interface_methods() {
    const SRC: &str = "interface First { fun foo(): Int }\n\
interface Second : First { fun bar(): Int }\n\
class Impl : Second { override fun foo() = 1; override fun bar() = 2 }\n\
class Test(s: Second) : Second by s\n\
fun box(): String {\n\
    val t = Test(Impl())\n\
    if (t.foo() != 1) return \"f1\"\n\
    if (t.bar() != 2) return \"f2\"\n\
    if (t !is First) return \"f3\"\n\
    return \"OK\"\n\
}\n";
    assert_eq!(
        run(SRC).expect("delegation to a sub-interface forwards inherited methods"),
        "OK"
    );
}

/// Property accessors live in the semantic property table rather than the ordinary method table.
/// Delegation must therefore synthesize their forwarders explicitly; otherwise the class verifies but
/// throws `AbstractMethodError` when the interface getter is invoked. This is the minimized
/// multiplatform corpus regression: the delegated class is declared against the expect interface and
/// must still implement the actual interface's accessor ABI.
#[test]
fn delegation_forwards_interface_property_accessors() {
    const COMMON: &str = "// LANGUAGE: +MultiPlatformProjects\n\
expect interface Base { val s: String }\n\
class Delegated<T>(val base: Base) : Base by base\n";
    const PLATFORM: &str = "// LANGUAGE: +MultiPlatformProjects\n\
actual interface Base { actual val s: String }\n\
class Impl : Base { override val s: String = \"K\" }\n\
fun box(): String {\n\
    val delegated = Delegated<Int>(Impl())\n\
    return delegated.s\n\
}\n";
    assert_eq!(
        common::compile_and_run_files_with_stdlib(&[("Common", COMMON), ("Platform", PLATFORM),])
            .expect("delegation forwards interface property accessors"),
        "K"
    );
}

/// Delegation to a generic interface instantiated with a REFERENCE type argument (`A<String> by a`):
/// the interface method `foo(): T` erases to `Object`, so a raw forward through the synthesized field
/// is correct (the unbox/checkcast happens at the call site). The non-`val` param uses a `$$delegate_N`.
#[test]
fn generic_reference_arg_delegation_forwards() {
    const SRC: &str = "interface A<T> { fun foo(): T }\n\
class B : A<String> { override fun foo() = \"OK\" }\n\
class C(a: A<String>) : A<String> by a\n\
fun box(): String {\n\
    val c = C(B())\n\
    val a: A<String> = c\n\
    if (c.foo() != \"OK\") return \"f1 ${c.foo()}\"\n\
    if (a.foo() != \"OK\") return \"f2 ${a.foo()}\"\n\
    return \"OK\"\n\
}\n";
    assert_eq!(
        run(SRC).expect("generic reference-arg delegation compiles + runs"),
        "OK"
    );
}

/// Source for the forwarder-ORDER tests below: a delegated interface with more than one method, so a
/// hash-ordered walk of the interface's member table can observe more than one emission order.
const MULTI_MEMBER_SRC: &str = "interface Base {\n\
    fun foo(): String\n\
    fun bar(): String\n\
}\n\
class Impl : Base {\n\
    override fun foo() = \"O\"\n\
    override fun bar() = \"K\"\n\
}\n\
class DelegatedImpl(val d: Base) : Base by d\n\
fun box(): String {\n\
    val x = DelegatedImpl(Impl())\n\
    return x.foo() + x.bar()\n\
}\n";

/// The names of `DelegatedImpl`'s forwarders, in emission order, from one compile of
/// [`MULTI_MEMBER_SRC`].
fn forwarder_order() -> (Vec<String>, Vec<u8>) {
    let classes = common::expect_classes_with_stdlib(MULTI_MEMBER_SRC, "DelegOrder");
    let (_, bytes) = classes
        .iter()
        .find(|(n, _)| n.ends_with("DelegatedImpl"))
        .expect("DelegatedImpl emitted");
    let info = krusty::jvm::classreader::parse_class(bytes).expect("DelegatedImpl parses");
    let order = info
        .methods
        .iter()
        .filter(|m| m.name == "foo" || m.name == "bar")
        .map(|m| m.name.clone())
        .collect();
    (order, bytes.clone())
}

/// Delegation forwarders are emitted in the delegated interface's DECLARATION order — kotlinc's order,
/// and the only one that is stable across processes. Reading the interface's members out of a hash map
/// made the order (and with it constant-pool intern order and the emitted bytes) depend on the hash
/// seed.
#[test]
fn forwarders_follow_interface_declaration_order() {
    assert_eq!(
        forwarder_order().0,
        vec!["foo".to_string(), "bar".to_string()]
    );
}

/// Repeated compiles of the same source emit byte-identical classes. Each compile builds fresh symbol
/// tables, so a hash-ordered member walk shows up here as differing bytes within a single process.
#[test]
fn forwarder_emission_is_byte_deterministic() {
    let (first_order, first_bytes) = forwarder_order();
    for i in 1..16 {
        let (order, bytes) = forwarder_order();
        assert_eq!(order, first_order, "forwarder order differs on compile {i}");
        // A pure intern-order flip leaves the class the same SIZE, so name the first differing
        // offset — the lengths alone would carry no signal in exactly the case this test catches.
        if bytes != first_bytes {
            let at = bytes
                .iter()
                .zip(&first_bytes)
                .position(|(a, b)| a != b)
                .unwrap_or_else(|| first_bytes.len().min(bytes.len()));
            panic!(
                "DelegatedImpl bytes differ on compile {i}: first difference at offset {at} \
                 ({} vs {} bytes total)",
                bytes.len(),
                first_bytes.len()
            );
        }
    }
}

/// A `val`-param delegate to a PRIMITIVE-instantiated generic interface (`A<Long> by a`) forwards
/// through its own typed field and is handled correctly. (A non-`val`-param primitive instantiation is
/// skipped — the erased forwarder mis-boxes an `int` literal as `Integer` for a `Long` parameter.)
#[test]
fn generic_primitive_valparam_delegation_forwards() {
    const SRC: &str = "interface A<T> { fun foo(t: T): String }\n\
class B : A<Long> { override fun foo(t: Long) = if (t == 42L) \"OK\" else \"fail $t\" }\n\
class C(val a: A<Long>) : A<Long> by a\n\
fun box(): String = C(B()).foo(42L)\n";
    assert_eq!(
        run(SRC).expect("val-param primitive generic delegation compiles + runs"),
        "OK"
    );
}
