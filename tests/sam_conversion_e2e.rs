//! SAM conversion: a lambda passed where a (simple, non-generic) `fun interface` is expected becomes an
//! instance of that interface whose single abstract method runs the lambda. Round-tripped on the JVM.

mod common;

fn run(src: &str) -> Option<String> {
    let jh = common::java_home()?;
    let sl = common::stdlib_jar()?;
    let jdk = std::path::PathBuf::from(format!("{jh}/lib/modules"));
    common::compile_and_run_box(src, "Main", &[sl], Some(&jdk))
}

#[test]
fn lambda_to_fun_interface_argument() {
    const SRC: &str = "fun interface Foo { fun get(): String }\n\
fun call(f: Foo): String = f.get()\n\
fun box(): String = call { \"OK\" }\n";
    assert_eq!(run(SRC).expect("lambda -> fun interface arg"), "OK");
}

#[test]
fn actual_interface_instance_still_passes() {
    // A real implementing class passed where the fun interface is expected must NOT be SAM-converted.
    const SRC: &str = "fun interface Foo { fun get(): String }\n\
class A : Foo { override fun get() = \"OK\" }\n\
fun call(f: Foo): String = f.get()\n\
fun box(): String = call(A())\n";
    assert_eq!(run(SRC).expect("interface instance passes through"), "OK");
}
