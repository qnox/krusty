use super::common;

#[test]
fn classpath_generic_superclass_override_dispatches_through_erased_descriptor() {
    // The superclass is intentionally compiled first and supplied only through the classpath. Its
    // generic declaration is physically `choose(Object): Object`; the source override is
    // `choose(String): String`. Calling through `Base<String>` therefore exercises both halves of the
    // bridge descriptor. If bridge derivation looks only at same-file IR classes, the inherited base
    // implementation runs instead of `Child.choose`, which is a silent virtual-dispatch miscompile.
    common::expect_box_ok_against(
        "classpath_superclass_bridge",
        r#"
package lib

open class Base<T> {
    open fun choose(value: T): T = value
}
"#,
        r#"
import lib.Base

class Child : Base<String>() {
    override fun choose(value: String): String = "OK"
}

fun box(): String {
    val base: Base<String> = Child()
    return base.choose("ignored")
}
"#,
    );
}

#[test]
fn classpath_generic_property_override_bridges_both_accessors() {
    // Getter and setter obligations are two descriptors of the SAME semantic property. Keeping
    // classpath properties in the shared property-shape walk ensures a mutable override receives both
    // bridges; deriving only the getter makes this write through `Base<String>` silently miss `Child`.
    common::expect_box_ok_against(
        "classpath_superclass_property_bridges",
        r#"
package lib

open class Base<T> {
    open var value: T? = null
}
"#,
        r#"
import lib.Base

class Child : Base<String>() {
    override var value: String? = null
}

fun box(): String {
    val base: Base<String> = Child()
    base.value = "OK"
    return base.value ?: "fail"
}
"#,
    );
}

#[test]
fn overloaded_superclass_selects_the_overridden_descriptor_not_declaration_order() {
    // The Int overload deliberately precedes the generic declaration and has the same arity. Bridge
    // selection must use semantic signature compatibility: taking the first same-named method would
    // synthesize `choose(Int)` -> `choose(String)` and still omit the required `choose(Object)` bridge
    // for the actual override. Dispatch through the generic base then falls back to `Base.choose`.
    let source = r#"
open class Base<T> {
    fun choose(value: Int): String = "I$value"
    open fun choose(value: T): T = value
}

class Child : Base<String>() {
    override fun choose(value: String): String = "S$value"
}

fun box(): String {
    val generic: Base<String> = Child()
    return generic.choose("x")
}
"#;
    assert_eq!(
        common::compile_and_run_with_stdlib(source, "Main").as_deref(),
        Some("Sx")
    );
}

#[test]
fn synthesized_data_class_any_overrides_tolerate_semantic_aliases() {
    // Library symbols can expose the same physical `Any` member through Kotlin and Java semantic
    // aliases. A data class's synthesized `equals`/`hashCode`/`toString` implementations must see one
    // erased obligation per descriptor; treating identical aliases as overloaded declarations makes
    // the bridge pass reject an otherwise ordinary data class.
    let source = r#"
data class Token(val active: Boolean)

fun box(): String = if (Token(true).active) "OK" else "fail"
"#;
    assert_eq!(
        common::compile_and_run_with_stdlib(source, "Main").as_deref(),
        Some("OK")
    );
}
