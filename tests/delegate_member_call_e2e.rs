//! A generated interface-delegation forwarder calls the member of the delegate's own static type
//! that overrides the forwarded declaration, as kotlinc's does: `List<String> by ArrayList()`
//! forwards `get` to `ArrayList.get` with `invokevirtual`, and `Source by Impl()` forwards to
//! `Impl.text`. Such a Java member's result is flexible or enhanced to not-null, so when the
//! forwarder's declared result rejects `null` kotlinc's implicit not-null cast checks it before
//! returning, naming the call (`get(...)`). A forwarder whose result is a type parameter admitting
//! `null`, or whose delegate result is a Kotlin declaration, stays unchecked.
//!
//! A class delegate's property accessors dispatch virtually; a type-parameter delegate forwards
//! through its bound, even when that bound applies the interface with other type arguments; and a
//! delegate with no class type (a function type) keeps the interface declaration's call. A
//! delegate class whose member overrides the forwarded declaration through several supertypes
//! dispatches virtually to it, and a member a superclass already implements gets no forwarder.

use super::common;

const SOURCE: &str = r#"
import java.util.ArrayList
import java.util.HashSet

class Names : List<String> by ArrayList<String>()

class Elements(items: ArrayList<String>) : MutableList<String> by items

class Members : Set<String> by HashSet<String>()

class Walk : Iterable<String> by ArrayList<String>()

class Generic<T>(items: ArrayList<T>) : List<T> by items

interface Source {
    fun text(): String
    fun maybe(): String?
}

class Impl : Source {
    override fun text(): String = "OK"
    override fun maybe(): String? = null
}

class Forwarded : Source by Impl()

interface Labeled {
    val label: String
    var count: Int
}

class LabelImpl : Labeled {
    override val label: String = "L"
    override var count: Int = 0
}

class Labels : Labeled by LabelImpl()

class Bounded<D : Source>(d: D) : Source by d

interface Transform<out T, in K> {
    fun apply(a: K): T
}

class Echo : Transform<Echo, Echo> {
    override fun apply(a: Echo): Echo = a
}

class Chained<D : Transform<D, D>, A : D>(a: A) : Transform<D, A> by a

class Call(f: () -> String) : () -> String by f

interface Collected {
    fun first(): String
    val second: String
}

interface Listed : Collected {
    override fun first(): String
    override val second: String
}

interface Grown : Collected

interface GrownList : Listed, Grown

abstract class AbstractCollected protected constructor() : Collected {
    abstract override fun first(): String
    abstract override val second: String
}

class Arrayed : GrownList, AbstractCollected() {
    override fun first(): String = "O"
    override val second: String = "K"
}

class Overridden : Grown by Arrayed()

interface Slot {
    var x: String
}

open class LateSlot : Slot {
    override lateinit var x: String
}

interface Shadow : Slot

open class FixedSlot : Shadow {
    override var x: String
        get() = "OK"
        set(_) {}
}

class Hidden : LateSlot(), Shadow by FixedSlot()

fun box(): String {
    val items = ArrayList<String>()
    items.add("O")
    items.add("K")
    val elements = Elements(items)
    if (elements.get(0) != "O") return "get"
    if (!elements.iterator().hasNext()) return "iterator"
    if (elements.listIterator().next() != "O") return "listIterator"
    if (elements.listIterator(1).next() != "K") return "listIterator(index)"
    if (elements.subList(1, 2).get(0) != "K") return "subList"
    if (elements.set(1, "K") != "K") return "set"
    elements.add("X")
    if (elements.removeAt(2) != "X") return "removeAt"
    if (!Names().isEmpty()) return "isEmpty"
    if (Members().iterator().hasNext()) return "Members.iterator"
    if (Walk().iterator().hasNext()) return "Walk.iterator"
    val nullable = ArrayList<String?>()
    nullable.add(null)
    if (Generic(nullable).get(0) != null) return "Generic.get"
    if (Forwarded().maybe() != null) return "maybe"
    val labels = Labels()
    labels.count = 2
    if (labels.count != 2 || labels.label != "L") return "Labels"
    if (Bounded(Impl()).text() != "OK") return "Bounded"
    val echo = Echo()
    if (Chained<Echo, Echo>(echo).apply(echo) !== echo) return "Chained"
    if (Call { "OK" }() != "OK") return "Call"
    val overridden = Overridden()
    if (overridden.first() + overridden.second != "OK") return "Overridden"
    val hidden = Hidden()
    hidden.x = "OK"
    if (hidden.x != "OK") return "Hidden"
    return Forwarded().text()
}
"#;

#[test]
fn delegate_member_calls_match_kotlinc() {
    let cases: &[(&str, &[&str])] = &[
        (
            "Names",
            &[
                "get",
                "iterator",
                "listIterator",
                "subList",
                "isEmpty",
                "contains",
            ],
        ),
        ("Elements", &["set", "removeAt", "add", "get"]),
        ("Members", &["iterator", "size"]),
        ("Walk", &["iterator"]),
        ("Generic", &["get", "iterator"]),
        ("Forwarded", &["text", "maybe"]),
        ("Labels", &["getLabel", "getCount", "setCount"]),
        ("Bounded", &["text", "maybe"]),
        ("Chained", &["apply"]),
        ("Call", &["invoke"]),
        ("Overridden", &["first", "getSecond"]),
    ];
    for (class, methods) in cases {
        let pair = common::ModuleClassPair::compile(&[("DelegateMemberCall.kt", SOURCE)], class);
        for method in *methods {
            let (kotlinc, krusty) = pair.method_code(class, method);
            assert_eq!(krusty, kotlinc, "{class}.{method}");
        }
    }
}

#[test]
fn delegate_member_calls_run_like_kotlinc() {
    let jdk = common::jdk_modules();
    let output = common::compile_and_run_box(
        SOURCE,
        "DelegateMemberCall",
        &[common::stdlib_jar()],
        Some(jdk.as_path()),
    )
    .expect("krusty compiles and runs the box");
    assert_eq!(output, "OK");
}
