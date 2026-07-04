//! Member delegated properties `class A { val/var x by Del() }`: an instance `x$delegate` field
//! (initialized in `<init>`) + a static `x$kprop` (`PropertyReference1Impl`) + an instance `getX()`
//! calling `this.x$delegate.getValue(this, x$kprop)` (and `setX` via `setValue` for `var`). The
//! delegate's `getValue`/`setValue` here ignore the property argument. Round-tripped under `-Xverify:all`.

use super::common;

#[test]
fn member_delegated_val_runs() {
    // Exact shape of corpus inClassVal.kt.
    const SRC: &str = "import kotlin.reflect.KProperty\n\
class Delegate {\n\
    operator fun getValue(t: Any?, p: KProperty<*>): Int = 1\n\
}\n\
class A {\n\
    val prop: Int by Delegate()\n\
}\n\
fun box(): String = if (A().prop == 1) \"OK\" else \"fail\"\n";
    common::expect_box_ok_with_stdlib(SRC, "P");
}

#[test]
fn member_delegated_var_runs() {
    // Exact shape of corpus inClassVar.kt.
    const SRC: &str = "import kotlin.reflect.KProperty\n\
class Delegate {\n\
    var inner = 1\n\
    operator fun getValue(t: Any?, p: KProperty<*>): Int = inner\n\
    operator fun setValue(t: Any?, p: KProperty<*>, i: Int) { inner = i }\n\
}\n\
class A {\n\
    var prop: Int by Delegate()\n\
}\n\
fun box(): String {\n\
    val c = A()\n\
    if (c.prop != 1) return \"fail get\"\n\
    c.prop = 2\n\
    if (c.prop != 2) return \"fail set\"\n\
    return \"OK\"\n\
}\n";
    common::expect_box_ok_with_stdlib(SRC, "P");
}
