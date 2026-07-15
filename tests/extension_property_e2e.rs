//! `val` extension properties (`val Recv.name: T get() = …`) lower to a static getter `getName(Recv): T`
//! (like an extension function), with `this` = the receiver; a read `x.name` becomes `getName(x)`. No
//! backing field. `var` extension properties (custom setter) and extension-delegated properties skip
//! cleanly. Round-tripped on the JVM.

use super::common;

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
fn extension_property_with_own_type_param() {
    // `val <T> Array<T>.length` declares a generic type parameter on the extension property; `T`
    // scopes over the receiver type. It erases like a function's — the getter reads `size`.
    const SRC: &str = "val <T> Array<T>.length: Int get() = this.size\n\
fun box(): String = if (arrayOfNulls<Int>(10).length == 10) \"OK\" else \"no\"\n";
    assert_eq!(
        run(SRC).expect("generic extension property compiles + runs"),
        "OK"
    );
}

#[test]
fn extension_property_type_param_bound_scopes_accessor() {
    const SRC: &str = "val <T: String> T.first: Char get() = this[0]\n\
fun box(): String = if (\"OK\".first == 'O') \"OK\" else \"no\"\n";
    assert_eq!(
        run(SRC).expect("bounded generic extension property compiles + runs"),
        "OK"
    );
}

#[test]
fn extension_property_on_bare_type_param_receiver() {
    // `val <T> T.tag` has a free type-parameter receiver; it erases to `Any` and applies to any
    // receiver (String, Int, …). Both reads resolve to the one static getter.
    const SRC: &str = "val <T> T.tag: String get() = \"K\"\n\
fun box(): String = if (\"x\".tag + 1.tag == \"KK\") \"OK\" else \"no\"\n";
    assert_eq!(
        run(SRC).expect("type-parameter-receiver extension property compiles + runs"),
        "OK"
    );
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
