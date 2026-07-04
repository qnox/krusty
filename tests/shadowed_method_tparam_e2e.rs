//! A method type parameter that SHADOWS its class's (`class Box<T> { fun <T> echo(x: T): T }`) is
//! INDEPENDENT of the receiver's type argument. The classpath member-return substitution
//! (`resolve_instance_member`) binds the class's formals to the receiver's arguments and
//! substitutes the method's generic return under them; if it also substituted a method-declared
//! parameter of the same name, `Box<String>.echo(42)` would be typed `String` and the call site would
//! `checkcast String` an `Integer` → `ClassCastException`. The substitution now drops any class
//! binding the method re-declares, so the shadowing `T` erases to its bound. Verified on a real JVM
//! against a separately `javac`-compiled generic class (Kotlin warns on such shadowing, so it is
//! absent from the same-file box corpus — a `javac` dependency is the faithful reproduction).

use super::common;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.is_empty())
}

/// Compile a default-`lib`-package generic Java class into a fresh classes dir; `None` if no JDK.
fn build_lib(java_src: &str) -> Option<PathBuf> {
    let jh = env("KRUSTY_REF_JAVA_HOME").or_else(|| env("JAVA_HOME"))?;
    let javac = format!("{jh}/bin/javac");
    if !Path::new(&javac).exists() {
        return None;
    }
    let work = std::env::temp_dir().join(format!("krusty_shadow_tp_{}", std::process::id()));
    let _ = fs::remove_dir_all(&work);
    fs::create_dir_all(&work).ok()?;
    fs::write(work.join("Box.java"), java_src).ok()?;
    let out = Command::new(&javac)
        .args([
            "-d",
            work.to_str().unwrap(),
            work.join("Box.java").to_str().unwrap(),
        ])
        .output()
        .ok()?;
    assert!(
        out.status.success(),
        "javac: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    Some(work)
}

use std::path::Path;

#[test]
fn shadowing_method_type_param_is_independent() {
    let Some(jh) = env("KRUSTY_REF_JAVA_HOME").or_else(|| env("JAVA_HOME")) else {
        eprintln!("skipping: set JAVA_HOME");
        return;
    };
    let Some(sl) = common::stdlib_jar() else {
        eprintln!("skipping: no kotlin-stdlib jar");
        return;
    };
    let jdk = PathBuf::from(format!("{jh}/lib/modules"));
    // `echo`'s own `<T>` shadows the class `<T>`; it returns its argument unchanged.
    let java = "package lib;\n\
public class Box<T> {\n\
    private final T t;\n\
    public Box(T t) { this.t = t; }\n\
    public T get() { return t; }\n\
    public <T> T echo(T x) { return x; }\n\
}\n";
    let Some(libdir) = build_lib(java) else {
        eprintln!("skipping: no JDK javac");
        return;
    };
    let cp = vec![libdir.clone(), sl.clone()];
    // `b.echo(42)` must type as the (erased) method `T`, NOT the receiver's `String` — so comparing
    // it to an `Int` type-checks and the value is the `Int` 42 at runtime.
    let main = "import lib.Box\n\
fun box(): String {\n\
    val b = Box<String>(\"s\")\n\
    val r = b.echo(42)\n\
    return if (r == 42) \"OK\" else \"got=$r\"\n\
}\n";
    let classes = common::compile_in_process(main, "Main", &cp, Some(&jdk))
        .expect("a shadowing method type parameter must not bind to the receiver's argument");
    match common::run_box(&classes, "MainKt", &[libdir, sl]) {
        Some(o) => assert_eq!(o.trim(), "OK", "box() = {o:?}"),
        None => eprintln!("skipping: box runner unavailable"),
    }
}
