//! `var` extension properties (`var Recv.name: T get() = … set(v) { … }`) lower to a static getter
//! `getName(Recv): T` and setter `setName(Recv, T)`; a read `x.name` → `getName(x)`, a write
//! `x.name = v` → `setName(x, v)`. No backing field. Round-tripped on the JVM.

mod common;

fn run(src: &str) -> Option<String> {
    let jh = common::java_home()?;
    let sl = common::stdlib_jar()?;
    let jdk = std::path::PathBuf::from(format!("{jh}/lib/modules"));
    common::compile_and_run_box(src, "Main", &[sl], Some(&jdk))
}

#[test]
fn var_extension_property_get_set() {
    const SRC: &str = "class Box { var backing = 0 }\n\
var Box.v: Int get() = backing\n\
    set(x) { backing = x }\n\
fun box(): String { val b = Box(); b.v = 42; return if (b.v == 42) \"OK\" else \"no\" }\n";
    assert_eq!(run(SRC).expect("var ext property compiles + runs"), "OK");
}

#[test]
fn var_extension_property_string() {
    const SRC: &str = "class Holder { var s = \"\" }\n\
var Holder.text: String get() = s\n\
    set(value) { s = value }\n\
fun box(): String { val h = Holder(); h.text = \"OK\"; return h.text }\n";
    assert_eq!(
        run(SRC).expect("string var ext property compiles + runs"),
        "OK"
    );
}
