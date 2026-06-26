//! `object O : Base(args)` — an object (incl. a sealed-hierarchy `object A : S()`) extending a class.
//! `parse_object` now captures the base class + super-args (previously ignored); the general class
//! lowering computes the `superclass` + emits the `super(args)` call. This also makes a sealed hierarchy
//! of objects exhaustive in a `when (s) { is A -> … }` (the objects are now registered as subclasses of
//! the sealed base). An object with BOTH a base class and interfaces (qualified `super<T>`) skips.

mod common;

fn run(src: &str) -> Option<String> {
    let jh = common::java_home()?;
    let sl = common::stdlib_jar()?;
    let jdk = std::path::PathBuf::from(format!("{jh}/lib/modules"));
    common::compile_and_run_box(src, "Main", &[sl], Some(&jdk))
}

#[test]
fn object_extends_class_with_arg() {
    const SRC: &str = "open class Base(val n: Int)\n\
object O : Base(5)\n\
fun box(): String = if (O.n == 5) \"OK\" else \"no\"\n";
    assert_eq!(
        run(SRC).expect("object extends class compiles + runs"),
        "OK"
    );
}

#[test]
fn sealed_object_hierarchy_when_is() {
    const SRC: &str = "sealed class S\nobject A : S()\nobject B : S()\n\
fun f(s: S): String = when (s) { is A -> \"O\"; is B -> \"K\" }\n\
fun box(): String = f(A) + f(B)\n";
    assert_eq!(run(SRC).expect("sealed-object when compiles + runs"), "OK");
}

#[test]
fn object_extends_open_class_method() {
    const SRC: &str = "open class Base { open fun g(): String = \"no\" }\n\
object O : Base() { override fun g() = \"OK\" }\n\
fun box(): String { val b: Base = O; return b.g() }\n";
    assert_eq!(
        run(SRC).expect("object override via base compiles + runs"),
        "OK"
    );
}

#[test]
fn object_with_computed_property_override() {
    // An `object` implementing an interface with a COMPUTED property override
    // (`override val d: Int get() = …`) — the same `getX()` lowering a class uses. Previously an
    // object only admitted plain backing-field body properties; a computed getter bailed the file.
    // This is the shape of a custom-serializer `object : KSerializer<…>` whose `descriptor` is a
    // `get() = …`.
    const SRC: &str = "interface I { val d: Int; val e: String }\n\
object O : I {\n\
override val d: Int get() = 40 + 2\n\
override val e: String get() = \"x\"\n\
}\n\
fun box(): String { val i: I = O; return if (i.d == 42 && i.e == \"x\") \"OK\" else \"no\" }\n";
    assert_eq!(
        run(SRC).expect("object computed-property override compiles + runs"),
        "OK"
    );
}

#[test]
fn nested_object_accessed_via_outer() {
    // A NESTED `object Inner` inside a class is hoisted to `Outer$Inner` and accessed as the singleton
    // via `Outer.Inner` (resolver + `getstatic Outer$Inner.INSTANCE` lowering). Previously a nested
    // object was silently dropped (not emitted) and `Outer.Inner` was an unresolved reference.
    const SRC: &str = "class Outer { object Inner { val x = 5; fun greet() = \"hi\" } }\n\
fun box(): String {\n\
if (Outer.Inner.x != 5) return \"x\"\n\
return if (Outer.Inner.greet() == \"hi\") \"OK\" else \"g\"\n\
}\n";
    assert_eq!(
        run(SRC).expect("nested object access compiles + runs"),
        "OK"
    );
}
