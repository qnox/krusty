//! The generic `Signature` of an interface-delegation forwarder. A forwarder in a generic class
//! signs the delegated member as that class sees it: `class Pass<D>(b: Source<D>) : Source<D> by b`
//! forwards `take(TD;)TD;`. A member whose positions name no type parameter keeps its descriptor
//! alone, and a member's own type parameters are declared on the forwarder as on the member.

use super::common;

const FORWARDERS: &str = "interface Source<T> {\n\
    fun take(item: T): T\n\
    val first: T\n\
    fun <R> pair(item: T, other: R): R\n\
}\n\
interface Sink<in T> { fun put(item: T): String }\n\
interface Maker<out T> { fun make(): T }\n\
class Fixed(val label: String) : Source<String> {\n\
    override fun take(item: String): String = item + label\n\
    override val first: String get() = label\n\
    override fun <R> pair(item: String, other: R): R = other\n\
}\n\
class Pass<D>(val inner: Source<D>) : Source<D> by inner\n\
class Bounded<D : CharSequence>(val inner: Source<D>) : Source<D> by inner\n\
class Nested<A>(val inner: Source<Source<A>>) : Source<Source<A>> by inner\n\
class Plain(val inner: Source<String>) : Source<String> by inner\n\
class Writer<W>(val inner: Sink<W>) : Sink<W> by inner\n\
class Builder<M>(val inner: Maker<M>) : Maker<M> by inner\n\
fun box(): String {\n\
    val fixed = Fixed(\"K\")\n\
    val sink = object : Sink<String> { override fun put(item: String): String = item }\n\
    val maker = object : Maker<String> { override fun make(): String = \"O\" }\n\
    val passed = Pass(fixed).take(\"O\") + Bounded(fixed).first + Plain(fixed).pair(\"\", \"!\")\n\
    val built = Builder(maker).make() + Writer(sink).put(\"K\")\n\
    return if (passed == \"OKK!\") built else passed\n\
}\n";

const CLASSES: [&str; 6] = ["Pass", "Bounded", "Nested", "Plain", "Writer", "Builder"];

#[test]
fn delegation_forwarders_run() {
    common::expect_box_ok_with_stdlib(FORWARDERS, "DelegationForwarders");
}

#[test]
fn delegation_forwarders_sign_as_kotlinc() {
    let krusty = common::expect_classes_with_stdlib(FORWARDERS, "DelegationForwarders");
    let dir = common::scratch_dir().expect("scratch directory");
    let path = dir.join("DelegationForwarders.kt");
    std::fs::write(&path, FORWARDERS).expect("write source");
    let out = dir.join("ref");
    let (code, stderr) = common::kotlinc_compile(&[
        "-d".to_string(),
        out.to_string_lossy().into_owned(),
        path.to_string_lossy().into_owned(),
    ])
    .expect("reference kotlinc is provisioned");
    assert_eq!(code, 0, "kotlinc failed: {stderr}");
    for class in CLASSES {
        let reference = std::fs::read(out.join(format!("{class}.class")))
            .unwrap_or_else(|_| panic!("kotlinc did not emit {class}"));
        let (_, emitted) = krusty
            .iter()
            .find(|(emitted, _)| emitted == class)
            .unwrap_or_else(|| panic!("krusty did not emit {class}"));
        // Bridge order is not this test's subject: each of kotlinc's methods, in its order, is
        // looked up by name and descriptor and must exist with the same `Signature`.
        let emitted = methods(emitted);
        let signed = methods(&reference)
            .into_iter()
            .map(|(name, descriptor, _)| {
                let found = emitted
                    .iter()
                    .find(|(n, d, _)| *n == name && *d == descriptor)
                    .map(|(_, _, signature)| signature.clone());
                (name, descriptor, found)
            })
            .collect::<Vec<_>>();
        let expected = methods(&reference)
            .into_iter()
            .map(|(name, descriptor, signature)| (name, descriptor, Some(signature)))
            .collect::<Vec<_>>();
        assert_eq!(signed, expected, "{class}: methods");
        assert_eq!(emitted.len(), expected.len(), "{class}: method count");
    }
}

/// Every method's name, descriptor and generic `Signature`, in classfile order.
fn methods(bytes: &[u8]) -> Vec<(String, String, Option<String>)> {
    let class = krusty::jvm::classreader::parse_class(bytes).expect("parse class");
    class
        .methods
        .iter()
        .map(|method| {
            (
                method.name.clone(),
                method.descriptor.clone(),
                method.signature.clone(),
            )
        })
        .collect()
}
