//! Protected members of a class in another package, used from code the JVM does not let call them.
//!
//! Kotlin lets code written inside a subclass call its supertypes' protected members: an inner
//! class, and the carrier of a property or function reference, among them. The JVM grants a
//! protected member only to its own package and to a subclass calling it on its own kind of
//! receiver, so kotlinc gives the receiver's class one `public static final synthetic access$<name>`
//! per member — taking the receiver, the member's parameters as that class sees them, and
//! returning its result the same way — and routes every such use through it. The members
//! themselves stay `protected`.
use super::common;

const BASE: &str = "package base\n\
\n\
open class Base<K> {\n\
\x20   protected fun greet(): String = \"O\"\n\
\x20   protected operator fun get(key: K): String = \"K\"\n\
\x20   protected fun tail(key: K, suffix: String = \"-\"): String = suffix\n\
\x20   protected var mark: String = \"\"\n\
\x20   protected val seen: String get() = \"!\"\n\
\x20   var open: String = \"\"\n\
\x20       protected set\n\
}\n";

const MAIN: &str = "import base.Base\n\
\n\
class Derived : Base<Long>() {\n\
\x20   inner class Inner {\n\
\x20       fun read(): String = greet() + this@Derived[1L] + seen + tail(2L)\n\
\x20       fun write() {\n\
\x20           mark = \"x\"\n\
\x20           open = \"o\"\n\
\x20       }\n\
\x20   }\n\
\n\
\x20   fun references(): String {\n\
\x20       ::mark.set(\"y\")\n\
\x20       val seenOf = Derived::seen\n\
\x20       val greeting = ::greet\n\
\x20       return mark + seenOf.get(this) + greeting()\n\
\x20   }\n\
}\n\
\n\
fun box(): String {\n\
\x20   val derived = Derived()\n\
\x20   derived.Inner().write()\n\
\x20   val result = derived.Inner().read() + derived.references() + derived.open\n\
\x20   return if (result == \"OK!-y!Oo\") \"OK\" else result\n\
}\n";

const SOURCES: [(&str, &str); 2] = [("base/Base.kt", BASE), ("Main.kt", MAIN)];

/// `javap -p -c -l` of one class, constant-pool indices erased.
fn disassembly(class: &str, bytes: &[u8]) -> String {
    let work = common::scratch_dir().expect("a scratch directory for disassembly");
    let path = work.join(format!("{}.class", class.replace(['/', '$'], "_")));
    std::fs::write(&path, bytes).expect("write the class for disassembly");
    let text =
        common::javap(&["-p", "-c", "-l", &path.to_string_lossy()]).expect("javap is provisioned");
    let _ = std::fs::remove_dir_all(work);
    text
}

#[test]
fn protected_members_of_another_package_are_reached_through_the_receivers_accessors() {
    // The members keep kotlinc's `protected` flags, the protected setter included; a protected
    // member's `$default` stub stays public, and subclass code calls it without an accessor.
    let base = common::ModuleClassPair::compile(&SOURCES, "base/Base");
    assert_eq!(
        common::member_table(&base.krusty),
        common::member_table(&base.kotlinc),
        "base/Base: kotlinc's members"
    );

    // The receiver's class declares one accessor per member, after its own members, in the order
    // the file first uses them: generic parameters as `Base<Long>` sees them, unboxed.
    let derived = common::ModuleClassPair::compile(&SOURCES, "Derived");
    assert_eq!(
        common::member_table(&derived.krusty),
        common::member_table(&derived.kotlinc),
        "Derived: kotlinc's members"
    );
    let (reference, krusty) = (
        disassembly("Derived", &derived.kotlinc),
        disassembly("Derived", &derived.krusty),
    );
    for header in [
        "public static final java.lang.String access$greet(Derived);",
        "public static final java.lang.String access$get(Derived, long);",
        "public static final java.lang.String access$getSeen(Derived);",
        "public static final void access$setMark(Derived, java.lang.String);",
        "public static final void access$setOpen(Derived, java.lang.String);",
        "public static final java.lang.String access$getMark(Derived);",
    ] {
        let accessor = common::method_block(&reference, header);
        assert!(!accessor.is_empty(), "kotlinc declares {header}");
        assert_eq!(common::method_block(&krusty, header), accessor, "{header}");
    }

    // The inner class calls them, and the reference carriers read and write through them.
    let inner = common::ModuleClassPair::compile(&SOURCES, "Derived$Inner");
    for method in ["read", "write"] {
        let (reference, krusty) = inner.method_code("Derived$Inner", method);
        assert_eq!(krusty, reference, "Derived$Inner.{method}");
    }
    let mark = common::ModuleClassPair::compile(&SOURCES, "Derived$references$1");
    for method in ["get", "set"] {
        let (reference, krusty) = mark.method_code("Derived$references$1", method);
        assert_eq!(krusty, reference, "Derived$references$1.{method}");
    }
    let seen = common::ModuleClassPair::compile(&SOURCES, "Derived$references$seenOf$1");
    let (reference, krusty) = seen.method_code("Derived$references$seenOf$1", "get");
    assert_eq!(krusty, reference, "Derived$references$seenOf$1.get");

    let jdk = common::jdk_modules();
    let result =
        common::compile_and_run_box_files(&SOURCES, &[common::stdlib_jar()], Some(jdk.as_path()))
            .expect("krusty compiles and runs the fixture");
    assert_eq!(result, common::kotlinc_box_files_result(&SOURCES, "MainKt"));
    assert_eq!(result, "OK");
}
