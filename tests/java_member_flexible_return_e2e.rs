//! A Java member's return type is flexible (`V!`). When the receiver is a Java generic class
//! applied to a caller's own type parameters (`Shelf<X, Y>` inside `fun <X, Y>`), the member's
//! receiver-specialized return must keep that caller identity: `shelf[probe]` is `Y!`, not the
//! `Any!` upper bound of an erased formal. The stdlib shape that exposed it is
//! `LinkedHashMap(map)[key]` inside a generic function, where the constructor call itself already
//! inferred `LinkedHashMap<K, V>` correctly.

use super::common;

const CARRIER_JAVA: &str = "package fixtures;\n\
public interface Carrier<A, B> {\n\
    A key();\n\
    B value();\n\
}\n";

const SHELF_JAVA: &str = "package fixtures;\n\
import java.util.ArrayList;\n\
import java.util.List;\n\
public class Shelf<A, B> {\n\
    private final A key;\n\
    private final B value;\n\
    public Shelf(Carrier<? extends A, ? extends B> carrier) {\n\
        this.key = carrier.key();\n\
        this.value = carrier.value();\n\
    }\n\
    public B get(A probe) { return key.equals(probe) ? value : null; }\n\
    public A firstKey() { return key; }\n\
    public List<B> values() {\n\
        List<B> out = new ArrayList<>();\n\
        out.add(value);\n\
        return out;\n\
    }\n\
}\n";

/// The generic callers, kept in their own file so their facade class can be compared byte for
/// byte with kotlinc's.
const SHELVING: &str = "import fixtures.Carrier
import fixtures.Shelf

fun <X, Y> lookup(carrier: Carrier<X, Y>, probe: X): Y? {
    val shelf = Shelf(carrier)
    return shelf[probe]
}

fun <X, Y> onlyValue(carrier: Carrier<X, Y>): Y = Shelf(carrier).values()[0]

fun <X, Y> reshelve(carrier: Carrier<X, Y>): Shelf<X, Y> = Shelf(carrier)
";

const BOX: &str = "
class Entry<X, Y>(private val k: X, private val v: Y) : Carrier<X, Y> {
    override fun key(): X = k
    override fun value(): Y = v
}

fun box(): String {
    val entry = Entry(\"k\", 7)
    if (lookup(entry, \"k\") != 7) return \"lookup\"
    if (lookup(entry, \"z\") != null) return \"miss\"
    if (onlyValue(entry) != 7) return \"values\"
    val shelf: Shelf<String, Int> = reshelve(entry)
    return if (shelf.firstKey() == \"k\") \"OK\" else \"key\"
}
";

fn fixture_classes() -> std::path::PathBuf {
    let sources = [
        ("Carrier.java".to_string(), CARRIER_JAVA.to_string()),
        ("Shelf.java".to_string(), SHELF_JAVA.to_string()),
    ];
    common::javac_compile(&sources, &[])
        .expect("javac compiles the Java fixture")
        .0
}

#[test]
fn java_member_return_keeps_the_callers_type_arguments() {
    let java = fixture_classes();
    let source = format!("{SHELVING}{BOX}");
    let reference = common::kotlinc_box_result_with_classpath(&source, std::slice::from_ref(&java));
    assert_eq!(reference, "OK", "kotlinc must accept and run the fixture");
    let jdk = common::jdk_modules();
    let classpath = [java, common::stdlib_jar()];
    let classes = common::compile_in_process(&source, "Main", &classpath, Some(jdk.as_path()))
        .unwrap_or_else(|| {
            panic!(
                "{:?}",
                common::front_end_diagnostics(&source, &classpath, Some(jdk.as_path()))
            )
        });
    let output = common::run_box(&classes, "MainKt", &classpath).expect("run krusty box");
    assert_eq!(output, reference);
}

#[test]
fn java_member_return_callers_match_kotlinc_bytes() {
    let java = fixture_classes();
    let scratch = common::scratch_dir().expect("scratch directory");
    let reference = scratch.join("reference");
    std::fs::create_dir_all(&reference).expect("reference output directory");
    let source = scratch.join("Shelving.kt");
    std::fs::write(&source, SHELVING).expect("write the Kotlin fixture");
    let Some((status, stderr)) = common::kotlinc_compile(&[
        "-d".to_string(),
        reference.to_string_lossy().into_owned(),
        "-cp".to_string(),
        java.to_string_lossy().into_owned(),
        source.to_string_lossy().into_owned(),
    ]) else {
        panic!("reference kotlinc unavailable");
    };
    assert_eq!(status, 0, "kotlinc rejected the fixture: {stderr}");
    let expected = std::fs::read(reference.join("ShelvingKt.class")).expect("kotlinc emitted it");
    let classes = common::compile_in_process_metadata_cp_module_target(
        SHELVING,
        "Shelving",
        &[java, common::stdlib_jar(), common::jdk_modules()],
        "main",
        None,
    )
    .expect("krusty compiles the generic callers");
    let actual = classes
        .iter()
        .find(|(name, _)| name == "ShelvingKt")
        .map(|(_, bytes)| bytes)
        .expect("krusty emitted ShelvingKt");
    let _ = std::fs::remove_dir_all(&scratch);
    assert!(
        actual == &expected,
        "ShelvingKt.class differs from kotlinc ({} vs {} bytes)",
        actual.len(),
        expected.len()
    );
}

/// The motivating JDK shapes: an indexed read on a JDK collection constructed from, or typed by,
/// a caller's own type parameters.
#[test]
fn jdk_collection_index_keeps_the_callers_type_arguments() {
    common::expect_box_same_as_kotlinc(
        "fun <K, V> copied(a: Map<K, V>): V? { val out = LinkedHashMap(a); return out[a.keys.first()] }
fun <K, V> hashed(a: Map<K, V>, key: K): V? { val out = HashMap(a); return out[key] }
fun <K, V> sorted(a: Map<K, V>, key: K): V? { val out = java.util.TreeMap(a); return out[key] }
fun <T> head(list: java.util.ArrayList<T>): T { val first = list[0]; return first }
fun box(): String {
    val source = mapOf(\"a\" to \"O\", \"b\" to \"K\")
    if (copied(source) != \"O\") return \"copied\"
    if (hashed(source, \"b\") != \"K\") return \"hashed\"
    if (sorted(source, \"z\") != null) return \"sorted\"
    return head(arrayListOf(\"O\")) + hashed(source, \"b\")
}
",
        "jdk_collection_index_caller_type_arguments",
    );
}
