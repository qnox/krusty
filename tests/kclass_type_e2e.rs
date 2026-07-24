//! `kotlin.reflect.KClass` as an ORDINARY type — a `KClass<T>` parameter/return, and a `::class`
//! expression flowing into one. krusty used to model a class literal as a bare `java.lang.Class` and
//! reject `KClass` as a type ("KClass is not available on this target"), so a registry-style
//! `fun <T : Any> get(type: KClass<T>)` could not compile. `KClass` is now just a classpath type
//! (resolved like any other), a class literal is typed as `KClass` and emitted as kotlinc does —
//! `Reflection.getOrCreateKotlinClass(X.class)` — and `.java` bridges back to `Class` through the real
//! `JvmClassMappingKt.getJavaClass` extension.

use super::common;

#[test]
fn kclass_parameter_and_class_literal_argument_run() {
    let src = "import kotlin.reflect.KClass\n\
        class Reg {\n\
        \x20   fun <T : Any> nameOf(type: KClass<T>): String = type.simpleName ?: \"?\"\n\
        \x20   fun <T : Any> javaNameOf(type: KClass<T>): String = type.java.name\n\
        }\n\
        class Widget\n\
        fun box(): String {\n\
        \x20   val r = Reg()\n\
        \x20   if (r.nameOf(Widget::class) != \"Widget\") return \"f1:\" + r.nameOf(Widget::class)\n\
        \x20   if (r.nameOf(String::class) != \"String\") return \"f2:\" + r.nameOf(String::class)\n\
        \x20   if (!r.javaNameOf(Widget::class).endsWith(\"Widget\")) return \"f3\"\n\
        \x20   return \"OK\"\n\
        }\n";
    assert_eq!(
        common::compile_and_run_with_stdlib(src, "Main").expect(
            "a KClass<T> parameter taking a class-literal argument compiles, verifies, runs"
        ),
        "OK"
    );
}

#[test]
fn kclass_returned_and_compared() {
    // A `KClass` VALUE round-trip: returned from a function, held in a local, compared for equality,
    // and bridged to `Class` via `.java` — all on the real `kotlin.reflect.KClass`.
    let src = "import kotlin.reflect.KClass\n\
        class Widget\n\
        fun widgetClass(): KClass<Widget> = Widget::class\n\
        fun box(): String {\n\
        \x20   val k: KClass<Widget> = widgetClass()\n\
        \x20   if (k != Widget::class) return \"f1\"\n\
        \x20   if (k.simpleName != \"Widget\") return \"f2\"\n\
        \x20   if (k.java != Widget::class.java) return \"f3\"\n\
        \x20   return \"OK\"\n\
        }\n";
    assert_eq!(
        common::compile_and_run_with_stdlib(src, "Main").expect(
            "a KClass value returned, compared and bridged to Class compiles, verifies, runs"
        ),
        "OK"
    );
}
