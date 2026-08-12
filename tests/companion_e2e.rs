//! `companion object` methods — compiled (like kotlinc) to a `C$Companion` class holding the methods,
//! a `public static final Companion` field on the outer class built in its `<clinit>`, and
//! `C.foo()` → `getstatic C.Companion; invokevirtual`. Round-tripped under `-Xverify:all`.

use super::common;

#[test]
fn companion_methods_run() {
    let src = "class C {\n\
    companion object {\n\
        fun answer(): Int = 42\n\
        fun greet(s: String): String = \"hi \" + s\n\
    }\n\
}\n\
fun box(): String {\n\
if (C.answer() != 42) return \"f1\"\n\
if (C.greet(\"x\") != \"hi x\") return \"f2\"\n\
return \"OK\"\n\
}\n";
    common::expect_box_ok_with_stdlib(src, "C");
}

#[test]
fn companion_property_custom_accessors_run() {
    // A `companion object` property with a custom accessor IS its accessor: kotlinc emits no static
    // field for it, just `getZERO()`/`getLEVEL()`/`setLEVEL(int)` on `C$Companion`, and `C.ZERO`
    // compiles to `getstatic C.Companion; invokevirtual`. A plain companion property (`backing`)
    // still hoists to a static field on the OUTER class, so the accessors read and write it there.
    // (The accessor bodies name the plain `backing` QUALIFIED: reading a property that HAS a static
    // field unqualified from inside the companion is a separate, still-unsupported shape.)
    let src = "class C {\n\
    companion object {\n\
        var backing = 10\n\
        val ZERO: Int get() = 0\n\
        val DERIVED: Int get() = C.backing * 2\n\
        var LEVEL: Int\n\
            get() = C.backing\n\
            set(v) { C.backing = v + 1 }\n\
    }\n\
}\n\
fun box(): String {\n\
    if (C.ZERO != 0) return \"f1\"\n\
    if (C.DERIVED != 20) return \"f2\"\n\
    if (C.LEVEL != 10) return \"f3\"\n\
    C.LEVEL = 41\n\
    if (C.LEVEL != 42) return \"f4\"\n\
    if (C.DERIVED != 84) return \"f5\"\n\
    return \"OK\"\n\
}\n";
    common::expect_box_ok_with_stdlib(src, "C");
}

#[test]
fn computed_companion_property_reads_outside_a_qualified_receiver() {
    // A field-less custom-accessor companion property has NO static field, so EVERY read of it must
    // go through the accessor — not just the qualified `C.X` form. An unqualified read from an
    // instance method, from a companion method, and from a member initializer each used to emit
    // `getstatic C.X` for a field that is never emitted (`NoSuchFieldError` at run time).
    let src = "class C {\n\
    val fromInit: Int = ZERO\n\
    companion object {\n\
        val ZERO: Int get() = 5\n\
        fun insideCompanion(): Int = ZERO\n\
    }\n\
    fun fromInstance(): Int = ZERO\n\
}\n\
fun box(): String {\n\
    val c = C()\n\
    if (C.ZERO != 5) return \"f1\"\n\
    if (c.fromInstance() != 5) return \"f2\"\n\
    if (C.insideCompanion() != 5) return \"f3\"\n\
    if (c.fromInit != 5) return \"f4\"\n\
    return \"OK\"\n\
}\n";
    common::expect_box_ok_with_stdlib(src, "C");
}

#[test]
fn companion_method_shares_name_with_instance_method_of_different_arity() {
    // Companion methods live on `C$Companion`, so a companion method may share a NAME with an
    // instance member when their accepted ARITY RANGES do not overlap — kotlinc accepts this (the
    // instance member wins on a dispatch receiver, the companion member on `C.`). Raw parameter
    // counts are insufficient here: defaults and varargs change which calls a declaration accepts.
    // The unqualified calls exercise the checker path that previously selected the companion by name
    // before the ordinary implicit-instance receiver could see the argument count.
    let src = "open class C {\n\
    open fun requestFocus(value: Boolean, suffix: String = \"\"): String = \"inst\" + value + suffix\n\
    fun describe(): String = requestFocus(true)\n\
    fun collect(prefix: Int, vararg values: String): String = \"$prefix:${values[0]}\"\n\
    fun describeVararg(): String = collect(7, \"x\")\n\
    companion object {\n\
        fun requestFocus(): Int = 42\n\
        fun collect(): String = \"comp\"\n\
        fun describeStatic(): String = \"comp\" + requestFocus() + collect()\n\
    }\n\
}\n\
fun box(): String {\n\
    if (C().requestFocus(true) != \"insttrue\") return \"f1\"\n\
    if (C().describe() != \"insttrue\") return \"f2\"\n\
    if (C().describeVararg() != \"7:x\") return \"f3\"\n\
    if (C.requestFocus() != 42) return \"f4\"\n\
    if (C.collect() != \"comp\") return \"f5\"\n\
    if (C.describeStatic() != \"comp42comp\") return \"f6\"\n\
    return \"OK\"\n\
}\n";
    common::expect_box_ok_with_stdlib(src, "C");
}

#[test]
fn property_inferred_from_generic_companion_method() {
    // A property initialized by a same-file class's generic companion method (`val c =
    // C.create<String>()`) infers its type from the companion method's (inferred) return type.
    let src = "class C() {\n\
    companion object {\n\
        private fun <T> create() = C()\n\
    }\n\
    class ZZZ { val c = C.create<String>() }\n\
}\n\
fun box(): String { C.ZZZ().c; return \"OK\" }\n";
    common::expect_box_ok_with_stdlib(src, "C");
}

#[test]
fn instance_members_outrank_same_shaped_companion_members() {
    let src = r#"
class Exact {
    fun f(): Int = 1
    companion object { fun f(): Int = 2 }
    fun selected(): Int = f()
}

class Defaulted {
    fun f(value: Int = 1): Int = value
    companion object { fun f(): Int = 2 }
    fun selected(): Int = f()
}

class Variadic {
    fun f(prefix: Int = 1, vararg value: Int): Int = prefix + value.size
    companion object { fun f(): Int = 2 }
    fun selected(): Int = f()
}

fun box(): String =
    if (Exact().selected() == 1 && Exact.f() == 2 &&
        Defaulted().selected() == 1 && Defaulted.f() == 2 &&
        Variadic().selected() == 1 && Variadic.f() == 2) "OK" else "FAIL"
"#;

    // Pin the language decision, not the old conservative rejection: kotlinc accepts all three
    // declaration pairs, and the nearer instance receiver wins only for the unqualified call.
    if common::compile_lib("CompanionInstanceOverlapKotlinc", src).is_none() {
        return;
    }
    common::expect_box_ok_with_stdlib(src, "CompanionInstanceOverlap");
}

#[test]
fn companion_reaches_the_outer_class_private_var() {
    // A companion is a SEPARATE class file, so it can neither call a private property's accessor
    // (kotlinc synthesizes none) nor `putfield` the private backing field. kotlinc routes the write
    // through a synthetic `access$setX$p(Outer, T)` bridge; emitting a plain `setX` call is a
    // `NoSuchMethodError` at run time. Mirrors box `classes/kt504.kt`, which the default gate
    // does not run.
    let src = "class Identifier() {\n\
    private var myNullable: Boolean = true\n\
    fun read(): Boolean = myNullable\n\
    companion object {\n\
        fun init(isNullable: Boolean): Identifier {\n\
            val id = Identifier()\n\
            id.myNullable = isNullable\n\
            return id\n\
        }\n\
    }\n\
}\n\
fun box(): String = if (!Identifier.init(false).read()) \"OK\" else \"FAIL\"\n";
    common::expect_box_ok_with_stdlib(src, "Identifier");
}

#[test]
fn a_private_property_keeps_its_source_written_setter() {
    // The accessor a private property does NOT get is the SYNTHESIZED one. A source-written
    // accessor is user code: dropping it turns `set(l) { /* ignore */ }` into a plain field store,
    // so the write below would take effect. Mirrors box `properties/kt3551.kt`.
    let src = "class Identifier() {\n\
    private var myNullable: Boolean = false\n\
        set(l: Boolean) {\n\
        }\n\
    fun read(): Boolean = myNullable\n\
    companion object {\n\
        fun init(isNullable: Boolean): Identifier {\n\
            val id = Identifier()\n\
            id.myNullable = isNullable\n\
            return id\n\
        }\n\
    }\n\
}\n\
fun box(): String = if (!Identifier.init(true).read()) \"OK\" else \"FAIL\"\n";
    common::expect_box_ok_with_stdlib(src, "Identifier2");
}

#[test]
fn a_nested_class_reaches_the_outer_class_private_member() {
    // Kotlin's private visibility is LEXICAL: a nested (non-`inner`) class has no outer receiver at
    // all, yet it sits inside the outer class's body and reaches its privates — including the
    // companion's. Walking the receiver chain alone reported this inaccessible.
    let src = "class Outer {\n\
    private val secret: Int = 7\n\
    private fun twice(): Int = secret * 2\n\
    class Nested {\n\
        fun read(): Int = Outer().twice()\n\
    }\n\
}\n\
fun box(): String = if (Outer.Nested().read() == 14) \"OK\" else \"FAIL\"\n";
    common::expect_box_ok_with_stdlib(src, "Outer");
}

#[test]
fn a_private_member_of_an_unrelated_class_stays_inaccessible() {
    // The lexical rule must not become "everything in the file is accessible".
    let src = "class A { private fun secret(): Int = 1 }\n\
class B { fun read(): Int = A().secret() }\n";
    let Some(diagnostics) = common::checker_diags_with_stdlib(src) else {
        return;
    };
    assert!(
        diagnostics.iter().any(|d| d.contains("secret")),
        "expected a private-access diagnostic, got: {diagnostics:?}"
    );
}
