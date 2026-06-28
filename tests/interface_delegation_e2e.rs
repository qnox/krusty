//! Interface delegation `class C(a: I) : I by a` where the delegate `a` is a NON-`val` constructor
//! parameter — kotlinc synthesizes a `private final $$delegate_N` field holding it, stored in the ctor,
//! and forwards each interface method through it. Round-tripped under `-Xverify:all`.

mod common;

fn run(src: &str) -> Option<String> {
    let jh = common::java_home()?;
    let sl = common::stdlib_jar()?;
    let jdk = std::path::PathBuf::from(format!("{jh}/lib/modules"));
    common::compile_and_run_box(src, "C", &[sl], Some(&jdk))
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
