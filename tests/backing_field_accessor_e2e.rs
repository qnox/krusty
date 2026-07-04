//! A class property with a backing field AND a custom accessor that references `field` — the getter
//! computes from the stored value (`val x = "O"; get() = field + "K"`), a `var` setter writes through
//! `field`. Distinct from a computed property (no backing field) and a plain field (default accessors).
//! Round-tripped on the JVM.

mod common;

fn run(src: &str) -> Option<String> {
    let sl = common::stdlib_jar()?;
    let jdk = common::jdk_modules()?;
    common::compile_and_run_box(src, "P", &[sl], Some(&jdk))
}

fn toolchain_ready() -> bool {
    common::java_home().is_some() && common::stdlib_jar().is_some()
}

#[test]
fn val_backing_field_custom_getter() {
    if !toolchain_ready() {
        return;
    }
    const SRC: &str = "// WITH_STDLIB\n\
class My {\n\
    val my: String = \"O\"\n\
        get() = field + \"K\"\n\
}\n\
fun box(): String = My().my\n";
    assert_eq!(
        run(SRC).expect("val backing-field custom getter should compile + run"),
        "OK"
    );
}

#[test]
fn var_backing_field_custom_accessors() {
    if !toolchain_ready() {
        return;
    }
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
    assert_eq!(
        run(SRC).expect("var backing-field custom accessors should compile + run"),
        "OK"
    );
}

#[test]
fn internal_read_and_write_go_through_custom_accessors() {
    if !toolchain_ready() {
        return;
    }
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
    assert_eq!(
        run(SRC).expect("internal custom-accessor access should compile + run"),
        "OK"
    );
}

#[test]
fn incdec_on_custom_accessor_var_goes_through_accessors() {
    if !toolchain_ready() {
        return;
    }
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
    assert_eq!(
        run(SRC).expect("incdec on custom-accessor var should compile + run"),
        "OK"
    );
}
