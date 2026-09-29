//! A class property with a backing field AND a custom accessor that references `field` — the getter
//! computes from the stored value (`val x = "O"; get() = field + "K"`), a `var` setter writes through
//! `field`. Distinct from a computed property (no backing field) and a plain field (default accessors).
//! Round-tripped on the JVM.

use super::common;

#[test]
fn val_backing_field_custom_getter() {
    const SRC: &str = "// WITH_STDLIB\n\
class My {\n\
    val my: String = \"O\"\n\
        get() = field + \"K\"\n\
}\n\
fun box(): String = My().my\n";
    common::expect_box_ok_with_stdlib(SRC, "P");
}

#[test]
fn var_backing_field_custom_accessors() {
    const SRC: &str = "// WITH_STDLIB\n\
class My {\n\
    var v: Int = 1\n\
        get() = field + 10\n\
        set(value) { field = value * 2 }\n\
}\n\
fun box(): String {\n\
    val m = My()\n\
    if (m.v != 11) return \"fail get: ${m.v}\"\n\
    m.v = 5\n\
    if (m.v != 20) return \"fail set: ${m.v}\"\n\
    return \"OK\"\n\
}\n";
    common::expect_box_ok_with_stdlib(SRC, "P");
}

#[test]
fn internal_read_and_write_go_through_custom_accessors() {
    // An IN-CLASS read/write of a custom-accessor property must call `getX`/`setX` — NOT read/write
    // the backing field directly (which would bypass the custom logic).
    const SRC: &str = "// WITH_STDLIB\n\
class My {\n\
    val my: String = \"O\"\n\
        get() = field + \"K\"\n\
    var v: Int = 1\n\
        get() = field + 10\n\
        set(value) { field = value * 2 }\n\
    fun selfTest(): String {\n\
        if (my != \"OK\") return \"fail read val: $my\"\n\
        if (v != 11) return \"fail read var: $v\"\n\
        v = 5\n\
        if (v != 20) return \"fail write var: $v\"\n\
        return \"OK\"\n\
    }\n\
}\n\
fun box(): String = My().selfTest()\n";
    common::expect_box_ok_with_stdlib(SRC, "P");
}

#[test]
fn incdec_on_custom_accessor_var_goes_through_accessors() {
    // `v++` on a custom-accessor `var` is `v = v + 1` = `setV(getV() + 1)` — it must run both
    // accessors, NOT increment the raw field. v0=1: getV()=11, +1=12, setV(12) → field=24; getV()=34.
    const SRC: &str = "// WITH_STDLIB\n\
class My {\n\
    var v: Int = 1\n\
        get() = field + 10\n\
        set(value) { field = value * 2 }\n\
    fun selfTest(): Int { v++; return v }\n\
}\n\
fun box(): String = if (My().selfTest() == 34) \"OK\" else \"fail: ${My().selfTest()}\"\n";
    common::expect_box_ok_with_stdlib(SRC, "P");
}

/// The `inline` modifier on a property ACCESSOR (`inline get()`, `private inline set(…)`) — accepted
/// (erased; krusty emits an ordinary accessor) rather than misparsed as an unterminated declaration.
#[test]
fn inline_accessor_modifier_parses_and_runs() {
    const SRC: &str = "// WITH_STDLIB
\
class C(val b: Int) {
\
    val p: Int
\
        inline get() = b + 1
\
    private val q: Int
\
        private inline get() = b + 2
\
    fun sum(): Int = p + q
\
}
\
fun box(): String = if (C(5).sum() == 13) \"OK\" else \"no\"
";
    common::expect_box_ok_with_stdlib(SRC, "C");
}

#[test]
fn private_computed_property_uses_its_checked_accessors() {
    const SRC: &str = "class Secret {\n\
        private var storage: Int = 0\n\
        private var computed: Int\n\
            get() = storage + 1\n\
            set(value) { storage = value * 2 }\n\
        fun verify(): Int { computed = 6; return computed }\n\
    }\n\
    fun box(): String = if (Secret().verify() == 13) \"OK\" else \"fail\"\n";
    common::expect_box_ok_with_stdlib(SRC, "PrivateComputed");
}

#[test]
fn a_top_level_property_with_one_declared_accessor_publishes_the_default_other() {
    // Only one accessor is declared; the facade still publishes the other as the default method,
    // so another module can both read and write the property.
    const LIB: &str = "package lib\n\
var written = 0\n\
    set(value) { field = value + 1 }\n\
var read = 0\n\
    get() = field * 2\n";
    const MAIN: &str = "import lib.*\n\
fun box(): String {\n\
    written = 1\n\
    read = 3\n\
    return if (written == 2 && read == 6) \"OK\" else \"fail $written $read\"\n\
}\n";
    common::expect_box_ok_against("DefaultOtherAccessor", LIB, MAIN);
}

/// `properties/fieldInsideField.kt`: a nested accessor's `field` is that accessor's property, not
/// the accessor this object was created inside.
#[test]
fn an_inner_accessor_reads_its_own_backing_field() {
    const SRC: &str = "abstract class Your {\n\
    abstract val your: String\n\
    fun foo() = your\n\
}\n\
val my: String = \"O\"\n\
    get() = field + object: Your() {\n\
        override val your = \"K\"\n\
            get() = field\n\
    }.foo()\n\
fun box() = my\n";
    common::expect_box_ok_with_stdlib(SRC, "FieldInsideField");
}

/// `properties/fieldInsideNested.kt`: an anonymous object created in a top-level getter reads that
/// getter's backing field.
#[test]
fn a_nested_object_reads_the_enclosing_top_level_backing_field() {
    const SRC: &str = "abstract class Your {\n\
    abstract val your: String\n\
    fun foo() = your\n\
}\n\
val my: String = \"O\"\n\
    get() = object: Your() {\n\
        override val your = field\n\
    }.foo() + \"K\"\n\
fun box() = my\n";
    common::expect_box_ok_with_stdlib(SRC, "FieldInsideNested");
}

/// `properties/classFieldInsideNested.kt`: the same read, on an instance property.
#[test]
fn a_nested_object_reads_the_enclosing_instance_backing_field() {
    const SRC: &str = "abstract class Your {\n\
    abstract val your: String\n\
    fun foo() = your\n\
}\n\
class My {\n\
    val my: String = \"O\"\n\
        get() = object : Your() {\n\
            override val your = field\n\
        }.foo() + \"K\"\n\
}\n\
fun box() = My().my\n";
    common::expect_box_ok_with_stdlib(SRC, "ClassFieldInsideNested");
}

/// `properties/classFieldInsideLocalInSetter.kt`: a local class in a setter writes the property's
/// backing field.
#[test]
fn a_local_class_in_a_setter_writes_the_backing_field() {
    const SRC: &str = "fun <T> eval(fn: () -> T) = fn()\n\
class My {\n\
    var my: String = \"U\"\n\
        get() = eval { field }\n\
        set(arg) {\n\
            class Local {\n\
                fun foo() { field = arg + \"K\" }\n\
            }\n\
            Local().foo()\n\
        }\n\
}\n\
fun box(): String {\n\
    val m = My()\n\
    m.my = \"O\"\n\
    return m.my\n\
}\n";
    common::expect_box_ok_with_stdlib(SRC, "ClassFieldInsideLocalInSetter");
}

/// A compound backing-field update from a local class must use the same bridge as separate reads
/// and writes; the JVM's direct-field subtraction fast path is legal only in the owning class.
#[test]
fn a_local_class_compound_update_uses_the_backing_field_bridge() {
    const SRC: &str = "class Counter {\n\
    var value: Int = 5\n\
        set(amount) {\n\
            class Local {\n\
                fun subtract() { field -= amount }\n\
            }\n\
            Local().subtract()\n\
        }\n\
    fun read() = value\n\
}\n\
fun box(): String {\n\
    val counter = Counter()\n\
    counter.value = 2\n\
    return if (counter.read() == 3) \"OK\" else \"FAIL: ${counter.read()}\"\n\
}\n";
    common::expect_box_ok_with_stdlib(SRC, "NestedCompoundBackingFieldUpdate");
}

/// A private property's reference must call its declared accessors while nested code in those
/// accessors reads and writes the backing field. The two operations have distinct JVM bridges.
#[test]
fn nested_field_bridges_do_not_replace_property_reference_bridges() {
    const SRC: &str = "fun <T> eval(fn: () -> T) = fn()\n\
class My {\n\
    private var my: String = \"U\"\n\
        get() = eval { field }\n\
        set(arg) {\n\
            class Local {\n\
                fun write() { field = arg + \"K\" }\n\
            }\n\
            Local().write()\n\
        }\n\
    fun reference() = this::my\n\
}\n\
fun box(): String {\n\
    val reference = My().reference()\n\
    reference.set(\"O\")\n\
    return reference.get()\n\
}\n";
    common::expect_box_ok_with_stdlib(SRC, "NestedFieldAndPropertyReferenceBridges");
}
