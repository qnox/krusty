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

#[test]
fn classpath_interface_delegation_map() {
    // Delegation to a CLASSPATH interface: `class Dto(val data: Map<…>) : Map<…> by data`. krusty
    // forwards each of `java/util/Map`'s abstract methods (erased descriptors) to the `data` field, and a
    // `Dto`-typed PROPERTY access inherited from the interface (`d.size`) dispatches through it.
    const SRC: &str = "class Dto(val data: Map<Int, Int>) : Map<Int, Int> by data\n\
fun box(): String {\n\
val d = Dto(mapOf(1 to 2, 3 to 4))\n\
return if (d.size == 2 && d.data.size == 2) \"OK\" else \"size ${d.size}\"\n\
}\n";
    assert_eq!(
        run(SRC).expect("classpath interface delegation compiles + runs"),
        "OK"
    );
}
