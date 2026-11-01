//! A non-final backing field signs its type the way kotlinc maps a value parameter, with
//! declaration-site variance as wildcards; a final field signs it like a return type.

use crate::common;

const SOURCE: &str = r#"
var recompose: (String, Int) -> Unit = { _, _ -> }
val fixed: (String, Int) -> Unit = { _, _ -> }
var names: List<CharSequence> = emptyList()
val frozen: List<CharSequence> = emptyList()
var order: Comparable<CharSequence>? = null
private var hidden: List<CharSequence> = emptyList()
lateinit var late: List<CharSequence>
var plain: List<String> = emptyList()
var delegated: List<CharSequence> by lazyVar()

fun lazyVar(): Slot = Slot()

class Slot {
    private var value: List<CharSequence> = emptyList()
    operator fun getValue(owner: Any?, property: Any?): List<CharSequence> = value
    operator fun setValue(owner: Any?, property: Any?, next: List<CharSequence>) {
        value = next
    }
}

class Members(var given: List<CharSequence>) {
    var names: List<CharSequence> = emptyList()
    val frozen: List<CharSequence> = emptyList()
    private var hidden: List<CharSequence> = emptyList()
    @JvmField var exposed: List<CharSequence> = emptyList()
    lateinit var late: List<CharSequence>

    fun touch(): Int {
        hidden = hidden + "x"
        return hidden.size
    }

    companion object {
        var shared: List<CharSequence> = emptyList()
        @JvmField var published: List<CharSequence> = emptyList()
    }
}

object Registry {
    var names: List<CharSequence> = emptyList()
    @JvmField var exposed: List<CharSequence> = emptyList()
}

enum class Mode {
    ON, OFF;

    var listeners: List<CharSequence> = emptyList()
}

fun touch(): Int {
    hidden = hidden + "x"
    return hidden.size
}

fun box(): String {
    recompose("a", 1)
    names = listOf("a")
    delegated = names
    val members = Members(names)
    members.late = names
    Registry.names = names
    Mode.ON.listeners = names
    Members.shared = names
    if (touch() != 1 || members.touch() != 1) return "touch"
    if (delegated.size != 1 || members.late.size != 1) return "values"
    return "OK"
}
"#;

#[test]
fn mutable_fields_sign_declaration_site_variance_like_kotlinc() {
    common::assert_classes_identical_to_kotlinc(
        "MutableFieldSignature",
        SOURCE,
        &[
            "MutableFieldSignatureKt",
            "Slot",
            "Members",
            "Members$Companion",
            "Registry",
            "Mode",
        ],
    );
}

#[test]
fn mutable_fields_keep_their_semantics() {
    common::expect_box_ok_with_stdlib(SOURCE, "MutableFieldSignature");
}
