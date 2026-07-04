//! `class B : A()` / `object O : A()` where the base `A(val x: Int = 5)` has all-defaulted constructor
//! parameters and the subtype supplies NO explicit base args. krusty previously bailed (the `super(…)`
//! arity didn't match the base's primary ctor). It now fills the base ctor's default-value exprs into the
//! `super(…)` call — the same defaults a `new A()` construction fills at the call site (krusty has no
//! synthetic `$default` ctor). Round-tripped on the JVM.

use super::common;

use std::path::PathBuf;

fn run(src: &str) -> Option<String> {
    let jh = common::java_home()?;
    let sl = common::stdlib_jar()?;
    let jdk = PathBuf::from(format!("{jh}/lib/modules"));
    common::compile_and_run_box(src, "Main", &[sl], Some(&jdk))
}

#[test]
fn class_extends_all_default_base() {
    const SRC: &str = "open class A(val x: Int = 5)\n\
class B : A()\n\
fun box(): String = if (B().x == 5) \"OK\" else \"no\"\n";
    assert_eq!(
        run(SRC).expect("class extends all-default base compiles + runs"),
        "OK"
    );
}

#[test]
fn object_extends_all_default_base() {
    const SRC: &str = "open class A(val tag: String = \"OK\")\n\
object O : A()\n\
fun box(): String = O.tag\n";
    assert_eq!(
        run(SRC).expect("object extends all-default base compiles + runs"),
        "OK"
    );
}
