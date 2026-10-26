//! A read-only face's member that the class effectively overrides gets no throwing stub, but the
//! mutable declaration's erased descriptor still needs a bridge when it differs from the
//! implementation's. kotlinc's bridge lowering then writes an `ACC_BRIDGE|ACC_SYNTHETIC` method at
//! that descriptor which casts (or unboxes) its arguments and calls the implementation, mapped to
//! the class declaration's line. A superclass that already writes the bridge is not repeated in
//! its subclass.

use super::common;

const ITERATOR_MEMBERS: &str = "    override fun hasNext() = false
    override fun next() = \"\"
    override fun hasPrevious() = false
    override fun previous() = \"\"
    override fun nextIndex() = 0
    override fun previousIndex() = -1
";

const INT_ITERATOR_MEMBERS: &str = "    override fun hasNext() = false
    override fun next() = 0
    override fun hasPrevious() = false
    override fun previous() = 0
    override fun nextIndex() = 0
    override fun previousIndex() = -1
";

/// The class's own members implement `set` and `add`.
fn own_members() -> String {
    format!(
        "class Walk : ListIterator<String> {{
{ITERATOR_MEMBERS}    var last = \"\"
    fun set(e: String) {{ last = \"set \" + e }}
    fun add(e: String) {{ last = \"add \" + e }}
}}
"
    )
}

/// A superclass with no collection face implements `set`; `add` keeps its throwing stub.
fn superclass_member() -> String {
    format!(
        "open class Base {{
    var last = \"\"
    fun set(e: String) {{ last = \"set \" + e }}
}}

class Walk : Base(), ListIterator<String> {{
{ITERATOR_MEMBERS}}}
"
    )
}

/// The implementation takes the unboxed `Int`, so the bridge unboxes its argument.
fn primitive_member() -> String {
    format!(
        "class Walk : ListIterator<Int> {{
{INT_ITERATOR_MEMBERS}    var last = 0
    fun set(e: Int) {{ last = e }}
}}
"
    )
}

/// The abstract superclass both has the face and implements `set`, so it writes the bridge and
/// its subclass writes nothing.
fn abstract_superclass_member() -> String {
    format!(
        "abstract class Base : ListIterator<String> {{
    var last = \"\"
    fun set(e: String) {{ last = \"set \" + e }}
}}

class Walk : Base() {{
{ITERATOR_MEMBERS}}}
"
    )
}

/// An interface default implements `set`.
fn interface_default() -> String {
    format!(
        "interface Setter {{
    fun set(e: String) {{}}
}}

class Walk : ListIterator<String>, Setter {{
{ITERATOR_MEMBERS}}}
"
    )
}

/// The class's own `set` is declared between two members with ordinary bridges, so its bridge sits
/// between theirs.
fn between_ordinary_bridges() -> String {
    format!(
        "class Walk : ListIterator<String>, Comparable<Walk>, java.util.function.Consumer<String> {{
{ITERATOR_MEMBERS}    override fun accept(t: String) {{}}
    var last = \"\"
    fun set(e: String) {{ last = \"set \" + e }}
    override fun compareTo(other: Walk) = 0
}}
"
    )
}

fn assert_classes_match_kotlinc(source: &str) {
    let classes = common::classes_against_kotlinc_module(&[(
        "Walk.kt",
        &format!("package bridges\n\n{source}"),
    )]);
    assert!(
        classes.reference.contains_key("bridges/Walk"),
        "kotlinc writes bridges/Walk"
    );
    assert_eq!(classes.differences(), Vec::<String>::new());
}

/// Call `name(Object)` the way a Java caller of `java.util.ListIterator` does, and report what it
/// did: the member's `last`, or the stub's exception message.
const CALL_ERASED: &str = r#"
fun callErased(walk: Any, name: String, argument: Any): String = try {
    walk.javaClass.getMethod(name, Any::class.java).invoke(walk, argument)
    "returned"
} catch (e: java.lang.reflect.InvocationTargetException) {
    val cause = e.cause
    if (cause is UnsupportedOperationException) "threw ${cause.message}" else "$cause"
}
"#;

const READ_ONLY: &str = "threw Operation is not supported for read-only collection";

#[test]
fn own_members_get_forwarding_bridges_like_kotlinc() {
    assert_classes_match_kotlinc(&own_members());
    let program = format!(
        "{}{CALL_ERASED}
fun box(): String {{
    val walk = Walk()
    if (callErased(walk, \"set\", \"x\") != \"returned\" || walk.last != \"set x\") return walk.last
    if (callErased(walk, \"add\", \"y\") != \"returned\" || walk.last != \"add y\") return walk.last
    return \"OK\"
}}
",
        own_members()
    );
    common::expect_box_same_as_kotlinc(&program, "read_only_own_member_bridges");
}

#[test]
fn a_superclass_member_gets_a_forwarding_bridge_beside_a_throwing_one_like_kotlinc() {
    assert_classes_match_kotlinc(&superclass_member());
    let program = format!(
        "{}{CALL_ERASED}
fun box(): String {{
    val walk = Walk()
    if (callErased(walk, \"set\", \"x\") != \"returned\" || walk.last != \"set x\") return walk.last
    val add = callErased(walk, \"add\", \"y\")
    if (add != \"{READ_ONLY}\") return add
    return \"OK\"
}}
",
        superclass_member()
    );
    common::expect_box_same_as_kotlinc(&program, "read_only_superclass_member_bridge");
}

#[test]
fn a_primitive_member_bridge_unboxes_its_argument_like_kotlinc() {
    assert_classes_match_kotlinc(&primitive_member());
    let program = format!(
        "{}{CALL_ERASED}
fun box(): String {{
    val walk = Walk()
    if (callErased(walk, \"set\", 7) != \"returned\" || walk.last != 7) return \"${{walk.last}}\"
    val add = callErased(walk, \"add\", 8)
    if (add != \"{READ_ONLY}\") return add
    return \"OK\"
}}
",
        primitive_member()
    );
    common::expect_box_same_as_kotlinc(&program, "read_only_primitive_member_bridge");
}

#[test]
fn an_abstract_superclass_writes_the_bridge_its_subclass_inherits_like_kotlinc() {
    assert_classes_match_kotlinc(&abstract_superclass_member());
    let program = format!(
        "{}{CALL_ERASED}
fun box(): String {{
    val walk = Walk()
    if (callErased(walk, \"set\", \"x\") != \"returned\" || walk.last != \"set x\") return walk.last
    return \"OK\"
}}
",
        abstract_superclass_member()
    );
    common::expect_box_same_as_kotlinc(&program, "read_only_abstract_superclass_bridge");
}

#[test]
fn an_interface_default_gets_a_forwarding_bridge_like_kotlinc() {
    assert_classes_match_kotlinc(&interface_default());
    let program = format!(
        "{}{CALL_ERASED}
fun box(): String {{
    val set = callErased(Walk(), \"set\", \"x\")
    if (set != \"returned\") return set
    return \"OK\"
}}
",
        interface_default()
    );
    common::expect_box_same_as_kotlinc(&program, "read_only_interface_default_bridge");
}

#[test]
fn an_own_member_bridge_takes_its_declaration_place_among_ordinary_bridges_like_kotlinc() {
    assert_classes_match_kotlinc(&between_ordinary_bridges());
}
