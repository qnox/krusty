//! `val` extension properties (`val Recv.name: T get() = …`) lower to a static getter `getName(Recv): T`
//! (like an extension function), with `this` = the receiver; a read `x.name` becomes `getName(x)`. No
//! backing field. `var` extension properties (custom setter) and extension-delegated properties skip
//! cleanly. Round-tripped on the JVM.

mod common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn extension_property_user_class_bare_member() {
    const SRC: &str = "class A(val n: Int)\n\
val A.doubled: Int get() = n * 2\n\
fun box(): String = if (A(21).doubled == 42) \"OK\" else \"no\"\n";
    assert_eq!(
        run(SRC).expect("ext property on user class compiles + runs"),
        "OK"
    );
}

#[test]
fn extension_property_on_primitive_this() {
    const SRC: &str = "val Int.sq: Int get() = this * this\n\
fun box(): String = if (5.sq == 25) \"OK\" else \"no\"\n";
    assert_eq!(run(SRC).expect("ext property on Int compiles + runs"), "OK");
}

#[test]
fn extension_property_on_string() {
    const SRC: &str = "val String.firstC: Char get() = this[0]\n\
fun box(): String = if (\"OK\".firstC == 'O') \"OK\" else \"no\"\n";
    assert_eq!(
        run(SRC).expect("ext property on String compiles + runs"),
        "OK"
    );
}
