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

/// A delegated extension property's `KProperty` names the property's container as its owner: the
/// file facade of a top-level property and the enclosing class of a member. The extension
/// receiver's classifier is not a container, and `IntArray` has no class to load.
const PRIMITIVE_ARRAY_RECEIVER: &str = "class Slot(val index: Int) {\n\
    \x20   operator fun getValue(thisRef: IntArray, property: Any?): Int = thisRef[index]\n\
    }\n\
    val IntArray.second by Slot(1)\n\
    class Scaled(val factor: Int) {\n\
    \x20   operator fun Long.getValue(thisRef: IntArray, property: Any?): Int = thisRef[toInt()] * factor\n\
    \x20   val IntArray.first by 0L\n\
    \x20   fun read(values: IntArray) = values.first\n\
    }\n\
    fun box(): String {\n\
    \x20   val values = IntArray(2)\n\
    \x20   values[0] = 3\n\
    \x20   values[1] = 4\n\
    \x20   if (values.second != 4) return \"top-level\"\n\
    \x20   if (Scaled(10).read(values) != 30) return \"member\"\n\
    \x20   return \"OK\"\n\
    }\n";

#[test]
fn a_delegated_extension_property_reference_is_owned_by_its_container() {
    common::expect_box_same_as_kotlinc(PRIMITIVE_ARRAY_RECEIVER, "PrimitiveArrayReceiverDelegate");
}
