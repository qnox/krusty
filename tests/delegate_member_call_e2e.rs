//! A generated interface-delegation forwarder calls the member of the delegate's own static type
//! that overrides the forwarded declaration, as kotlinc's does: `List<String> by ArrayList()`
//! forwards `get` to `ArrayList.get` with `invokevirtual`, and `Source by Impl()` forwards to
//! `Impl.text`. Such a Java member's result is flexible or enhanced to not-null, so when the
//! forwarder's declared result rejects `null` kotlinc's implicit not-null cast checks it before
//! returning, naming the call (`get(...)`). A forwarder whose result is a type parameter admitting
//! `null`, or whose delegate result is a Kotlin declaration, stays unchecked.

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
