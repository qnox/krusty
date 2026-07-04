//! Local delegated properties `fun f() { val/var x by Del() }`: a synthesized `$delegate` local holds
//! the delegate; reads route to `getValue(null, propref)`, a `var`'s writes to `setValue`. The
//! delegate's getValue/setValue here ignore the property argument. Round-tripped under `-Xverify:all`.

use super::common;

#[test]
fn local_delegated_val_runs() {
    // Corpus local/localVal.kt + localValNoExplicitType.kt shapes.
    const SRC: &str = "import kotlin.reflect.KProperty\n\
class Delegate {\n\
    operator fun getValue(t: Any?, p: KProperty<*>): Int = 1\n\
}\n\
fun box(): String {\n\
    val prop: Int by Delegate()\n\
    val inferred by Delegate()\n\
    return if (prop == 1 && inferred == 1) \"OK\" else \"fail\"\n\
}\n";
    common::expect_box_ok_with_stdlib(SRC, "P");
}

#[test]
fn local_delegated_var_runs() {
    // Corpus local/localVar.kt shape.
    const SRC: &str = "import kotlin.reflect.KProperty\n\
class Delegate {\n\
    var inner = 1\n\
    operator fun getValue(t: Any?, p: KProperty<*>): Int = inner\n\
    operator fun setValue(t: Any?, p: KProperty<*>, i: Int) { inner = i }\n\
}\n\
fun box(): String {\n\
    var prop: Int by Delegate()\n\
    if (prop != 1) return \"fail get\"\n\
    prop = 2\n\
    if (prop != 2) return \"fail set\"\n\
    return \"OK\"\n\
}\n";
    common::expect_box_ok_with_stdlib(SRC, "P");
}
