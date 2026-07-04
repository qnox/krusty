//! Top-level delegated property `val x: T by Del()` where `Del` is a user class with a member
//! `operator fun getValue(thisRef: Any?, property: KProperty<*>): T`. Modeled as `x$delegate` +
//! `x$kprop` (a `PropertyReference0Impl`) statics + a `getX()` calling `delegate.getValue(null, kprop)`.
//! Round-tripped under `-Xverify:all`.

use super::common;

#[test]
fn delegated_property_runs() {
    const SRC: &str = "import kotlin.reflect.KProperty\n\
class Del {\n\
    operator fun getValue(thisRef: Any?, property: KProperty<*>): String = \"hello\"\n\
}\n\
val greeting: String by Del()\n\
fun box(): String {\n\
if (greeting != \"hello\") return \"fail: \" + greeting\n\
return \"OK\"\n\
}\n";
    common::expect_box_ok_with_stdlib(SRC, "P");
}

#[test]
fn delegated_property_inferred_type_in_clinit() {
    // Exact shape of corpus accessTopLevelDelegatedPropertyInClinit.kt: inferred type + `val a = prop`.
    const SRC: &str = "import kotlin.reflect.KProperty\n\
class Delegate {\n\
    operator fun getValue(thisRef: Any?, prop: KProperty<*>): String {\n\
        return \"OK\"\n\
    }\n\
}\n\
val prop by Delegate()\n\
val a = prop\n\
fun box() = a\n";
    common::expect_box_ok_with_stdlib(SRC, "P");
}
