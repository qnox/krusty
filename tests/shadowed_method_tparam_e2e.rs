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

#[test]
fn shadowing_method_type_param_is_independent() {
    let jdk = common::jdk_modules();
    let sl = common::stdlib_jar();
    // `echo`'s own `<T>` shadows the class `<T>`; it returns its argument unchanged.
    let java = "package lib;\n\
public class Box<T> {\n\
    private final T t;\n\
    public Box(T t) { this.t = t; }\n\
    public T get() { return t; }\n\
    public <T> T echo(T x) { return x; }\n\
}\n";
    let (libdir, _) = common::javac_compile(&[("Box.java".to_string(), java.to_string())], &[])
        .expect("pooled javac compiles the shadowing generic class");
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
    let out =
        common::run_box(&classes, "MainKt", &[libdir, sl]).expect("pooled box runner unavailable");
    assert_eq!(out.trim(), "OK", "box() = {out:?}");
}
