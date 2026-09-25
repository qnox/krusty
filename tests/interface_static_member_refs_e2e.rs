//! Static members of an interface are named through an `InterfaceMethodref`, and members a class
//! declares through a `Methodref`, whatever path reaches them. The JVM checks the constant kind when
//! it links the call, so each case fails with `IncompatibleClassChangeError` if the kind is wrong.

use super::common;

#[test]
fn sibling_file_interface_default_call_names_interface_method() {
    // `pick$default` is a static method of the interface `Chooser`, declared in another file of the
    // module than the call that omits the argument.
    let call = "class Picker : Chooser<String>\n\
fun box(): String = Picker().pick()!!\n";
    let chooser = "interface Chooser<T> {\n\
    fun pick(choice: String = \"OK\"): T? = choice as T\n\
}\n";
    common::expect_box_ok_files_with_stdlib(&[("Call.kt", call), ("Chooser.kt", chooser)], "Call");
}

#[test]
fn interface_lambda_implementation_handle_names_interface_method() {
    // The lambda's implementation `render$lambda$0` is a private static method of `Panel`, and the
    // metafactory's implementation handle must name it through an `InterfaceMethodref`.
    const SRC: &str = "interface Panel {\n\
    fun render(): String = { caption() }.let { it() }\n\
    private fun caption(): String = \"OK\"\n\
}\n\
object Board : Panel\n\
fun box(): String = Board.render()\n";
    common::expect_box_ok_with_stdlib(SRC, "Panel");
}

#[test]
fn super_call_through_interface_to_class_member_names_class_method() {
    // `super<Marker>.hashCode()` selects the member `Marker` inherits from `Any`, which is declared
    // on a class and is therefore a `Methodref`, although the qualifier is an interface.
    const SRC: &str = "interface Marker\n\
class Tagged : Marker { fun code() = super<Marker>.hashCode() }\n\
fun box(): String {\n\
    val tagged = Tagged()\n\
    return if (tagged.code() == System.identityHashCode(tagged)) \"OK\" else \"fail\"\n\
}\n";
    common::expect_box_ok_with_stdlib(SRC, "Marker");
}
