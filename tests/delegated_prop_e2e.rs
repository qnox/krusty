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

/// `delegatedProperty/delegateToConstVal.kt`: a `const val` property reference is a `getValue`
/// delegate. Reading the object's reference runs its initializer; both reads return the constants.
#[test]
fn a_const_property_reference_is_a_delegate() {
    const SRC: &str = "\
var sideEffect = \"Fail\"\n\
object Property {\n\
    init { sideEffect = \"OK\" }\n\
    const val PROPERTY_VALUE: String = \"O\"\n\
}\n\
const val TOP_LEVEL_PROPERTY_VALUE: String = \"K\"\n\
val value1: String by Property::PROPERTY_VALUE\n\
val value2: String by ::TOP_LEVEL_PROPERTY_VALUE\n\
fun box(): String {\n\
    if (sideEffect != \"OK\") return \"Side effect wasn't executed\"\n\
    return value1 + value2\n\
}\n";
    common::expect_box_same_as_kotlinc(SRC, "DelegateToConstVal");
}

/// A private constant still has no declared getter. Its property-reference carrier returns the
/// selected declaration's compile-time payload directly.
#[test]
fn a_private_top_level_const_reference_returns_its_constant() {
    const SRC: &str = "private const val VALUE: String = \"OK\"\n\
val result: String by ::VALUE\n\
fun box(): String = result\n";
    common::expect_box_same_as_kotlinc(SRC, "PrivateTopLevelConstPropertyReference");
}

/// A bound companion-constant reference retains its semantic receiver while `get()` returns the
/// selected declaration's compile-time payload directly.
#[test]
fn a_private_companion_const_reference_returns_its_constant() {
    const SRC: &str = "class ConstantOwner {\n\
    companion object {\n\
        private const val MEMBER_VALUE: String = \"OK\"\n\
        fun memberReference() = ::MEMBER_VALUE\n\
    }\n\
}\n\
fun box(): String = ConstantOwner.memberReference().get()\n";
    common::expect_box_same_as_kotlinc(SRC, "PrivateCompanionConstPropertyReference");
}

fn assert_reference_get_same_as_kotlinc(src: &str, stem: &str, class: &str) {
    common::expect_box_same_as_kotlinc(src, stem);
    let cp = [common::stdlib_jar()];
    let built = common::compare_with_kotlinc_plugin(stem, src, class, &cp, "17", &[])
        .unwrap_or_else(|| panic!("{class} did not compile"));
    assert_eq!(
        common::method_instructions(&built.krusty, "get();"),
        common::method_instructions(&built.reference, "get();"),
        "{class}.get must emit kotlinc's exact compile-time constant"
    );
}

#[test]
fn a_visible_interface_companion_const_reference_emits_the_constant() {
    const SRC: &str = "interface PublicContract {\n\
    companion object {\n\
        const val VALUE: String = \"OK\"\n\
        fun reference() = ::VALUE\n\
    }\n\
}\n\
fun box(): String = PublicContract.reference().get()\n";
    assert_reference_get_same_as_kotlinc(
        SRC,
        "PublicInterfaceCompanionConstReference",
        "PublicContract$Companion$reference$1",
    );
}

#[test]
fn a_private_interface_companion_const_reference_emits_the_constant() {
    const SRC: &str = "interface PrivateContract {\n\
    companion object {\n\
        private const val VALUE: String = \"OK\"\n\
        fun reference() = ::VALUE\n\
    }\n\
}\n\
fun box(): String = PrivateContract.reference().get()\n";
    assert_reference_get_same_as_kotlinc(
        SRC,
        "PrivateInterfaceCompanionConstReference",
        "PrivateContract$Companion$reference$1",
    );
}

#[test]
fn a_dependency_const_reference_uses_the_published_constant() {
    let library = "package dependency\nconst val VALUE: String = \"OK\"\n";
    let main =
        "import dependency.VALUE\nval result: String by ::VALUE\nfun box(): String = result\n";
    let result = common::expect_box_run_against_kotlinc(library, main)
        .expect("kotlinc dependency and krusty caller");
    assert_eq!(result.trim(), "OK");
}
