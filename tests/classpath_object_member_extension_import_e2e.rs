//! `import kotlin.time.Duration.Companion.minutes` — importing a member EXTENSION declared inside an
//! `object` or `companion object` — did not resolve, so `10.minutes` was `unresolved reference`.
//!
//! Kotlin's rule: importing a member of an object brings that name into scope WITH the object as its
//! implicit dispatch receiver; for an extension member the use site supplies the extension receiver and
//! the singleton is the dispatch receiver. krusty's callable namespace is keyed by fully-qualified
//! name, and `symbols` only ever treated the parent of that name as a PACKAGE — so an
//! object or companion parent surfaced nothing. The one shape that worked, `import Obj.memberFun`, did
//! so through a separate special case, not the namespace.
//!
//! An object-like classifier is now a legal callable-namespace parent: its member extensions are
//! surfaced as extension callables carrying `LibraryCallable::singleton_dispatch`, and emit loads the
//! singleton (`Obj.INSTANCE` / `Outer.Companion`) and invokes on it instead of `invokestatic`.
//!
//! This is what the `runTest(timeout = 10.minutes)` chain bottomed out in: `minutes` is a member
//! extension property of `Duration.Companion`, so the argument was `Error` and no overload applied.
use super::common;

const LIB: &str = r#"
    package lib

    class Holder {
        companion object {
            val Int.tripled: Int get() = this * 3
            fun Int.quadrupled(): Int = this * 4
        }
    }

    object Tools {
        val Int.doubled: Int get() = this * 2
        fun Int.negated(): Int = -this
        // `Byte`/`Short` erase to `I` in a `Ty`, so a descriptor REBUILT from parameter types would
        // name `scaled(int, int)` — a method that does not exist. The verbatim descriptor must be
        // used; nothing catches that unless a signature carries a sub-`Int` primitive.
        fun Int.scaled(factor: Byte, offset: Short): Int = this * factor.toInt() + offset.toInt()
    }
"#;

#[test]
fn member_extensions_imported_from_an_object_or_companion_resolve_and_run() {
    let Some(libout) = common::compile_lib("object_member_extension_import", LIB) else {
        return;
    };
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let classpath = [libout, stdlib];
    // Both owners (plain `object` and `companion object`) and both member kinds (extension property,
    // extension function). The companion cases are the `Duration.Companion.minutes` shape.
    let main = "import lib.Holder.Companion.quadrupled\n\
        import lib.Holder.Companion.tripled\n\
        import lib.Tools.doubled\n\
        import lib.Tools.negated\n\
        import lib.Tools.scaled\n\
        fun box(): String {\n\
        \x20 if (5.doubled != 10) return \"object property: ${5.doubled}\"\n\
        \x20 if (5.negated() != -5) return \"object function: ${5.negated()}\"\n\
        \x20 if (5.tripled != 15) return \"companion property: ${5.tripled}\"\n\
        \x20 if (5.quadrupled() != 20) return \"companion function: ${5.quadrupled()}\"\n\
        \x20 val factor: Byte = 3\n\
        \x20 val offset: Short = 4\n\
        \x20 if (5.scaled(factor, offset) != 19) return \"sub-int params: ${5.scaled(factor, offset)}\"\n\
        \x20 return \"OK\"\n\
        }\n";
    let Some(out) = common::compile_and_run_box(main, "Main", &classpath, Some(jdk.as_path()))
    else {
        panic!(
            "compile/run returned None: {:?}",
            common::front_end_diagnostics(main, &classpath, Some(jdk.as_path()))
        );
    };
    assert_eq!(out, "OK");
}

#[test]
fn member_extension_imported_from_a_named_source_companion_uses_its_declared_field() {
    let source = "package lib\n\
        import lib.Holder.Factory.quadrupled\n\
        class Holder { companion object Factory { fun Int.quadrupled(): Int = this * 4 } }\n\
        fun box(): String = if (5.quadrupled() == 20) \"OK\" else \"fail\"\n";
    let result = common::compile_and_run_with_stdlib(source, "NamedCompanion");
    assert_eq!(
        result.unwrap_or_else(|| panic!(
            "named source companion import: {:?}",
            common::front_end_diagnostics(
                source,
                std::slice::from_ref(&common::stdlib_jar()),
                Some(common::jdk_modules().as_path()),
            )
        )),
        "OK"
    );
}

#[test]
fn a_stdlib_companion_extension_property_resolves_and_runs() {
    // The real-world instance, against the actual stdlib rather than a fixture. `Duration.Companion`
    // declares `val Int.minutes`, and its accessor is both value-class mangled (`getMinutes-UwyO8pc`)
    // AND `@InlineOnly` — `private` in the class file, so there is no call to emit and the body must be
    // SPLICED with the singleton bound as its receiver. Two different units are compared rather than
    // read through a `Duration` member: `Duration`'s own member properties are a separate gap.
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let classpath = [stdlib];
    let main = "import kotlin.time.Duration\n\
        import kotlin.time.Duration.Companion.minutes\n\
        import kotlin.time.Duration.Companion.seconds\n\
        fun box(): String {\n\
        \x20 val a: Duration = 10.minutes\n\
        \x20 val b: Duration = 600.seconds\n\
        \x20 if (a != b) return \"not equal: $a vs $b\"\n\
        \x20 if (a.toString() != \"10m\") return \"rendered: $a\"\n\
        \x20 return \"OK\"\n\
        }\n";
    let Some(out) = common::compile_and_run_box(main, "Main", &classpath, Some(jdk.as_path()))
    else {
        panic!(
            "compile/run returned None: {:?}",
            common::front_end_diagnostics(main, &classpath, Some(jdk.as_path()))
        );
    };
    assert_eq!(out, "OK");
}
