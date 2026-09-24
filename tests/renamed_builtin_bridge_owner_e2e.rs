//! Which class owns a renamed-builtin bridge (`size()` for `Collection.size`, `intValue()` for
//! `Number.toInt`).
//!
//! kotlinc emits the bridge `final` in the FIRST Kotlin class that overrides the mapped member —
//! `kotlin.collections.AbstractCollection.size()` in the standard library — and every subclass
//! inherits it. A subclass that redeclares it overrides a final method, and the JVM refuses to load
//! the class (`IncompatibleClassChangeError`). A Java superclass realizes the member under its JVM
//! name itself and owns no bridge, so the Kotlin subclass still needs one there; an unrelated
//! superclass property of the same name owns none either.
use super::common;

fn run(src: &str) -> String {
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    common::compile_and_run_box(src, "Main", &[stdlib, jdk.clone()], Some(jdk.as_path()))
        .expect("fixture compiles and runs")
}

/// The published methods — name, descriptor, access flags (`final`, `bridge`, `synthetic`) and
/// generic signature — of each listed class, as kotlinc and krusty emit them. Constant-pool layout,
/// debug tables and `@Metadata` are excluded: this compares the member and bridge SET.
fn assert_methods_match_kotlinc(tag: &str, src: &str, classes: &[&str]) {
    let Some(dir) = common::scratch_dir() else {
        eprintln!("skip: scratch directory unavailable for {tag}");
        return;
    };
    let reference = dir.join("reference");
    std::fs::create_dir_all(&reference).expect("create reference directory");
    let source = dir.join(format!("{tag}.kt"));
    std::fs::write(&source, src).expect("write fixture");
    let args = vec![
        "-d".to_string(),
        reference.to_string_lossy().into_owned(),
        source.to_string_lossy().into_owned(),
    ];
    let Some((status, stderr)) = common::kotlinc_compile(&args) else {
        eprintln!("skip: kotlinc unavailable for {tag}");
        return;
    };
    assert_eq!(status, 0, "{tag}: kotlinc failed: {stderr}");
    let jdk = common::jdk_modules();
    let ours = common::compile_in_process(src, tag, &[common::stdlib_jar()], Some(jdk.as_path()))
        .unwrap_or_else(|| panic!("{tag}: krusty failed to compile"));
    let methods = |bytes: &[u8], who: &str, class: &str| {
        krusty::jvm::classreader::parse_class(bytes)
            .unwrap_or_else(|_| panic!("{tag}: parse {who} {class}.class"))
            .methods
            .into_iter()
            .map(|method| {
                (
                    method.name,
                    method.descriptor,
                    method.access,
                    method.signature,
                )
            })
            .collect::<Vec<_>>()
    };
    for class in classes {
        let expected = std::fs::read(reference.join(format!("{class}.class")))
            .unwrap_or_else(|_| panic!("{tag}: kotlinc did not emit {class}"));
        let (_, actual) = ours
            .iter()
            .find(|(name, _)| name == class)
            .unwrap_or_else(|| panic!("{tag}: krusty did not emit {class}"));
        assert_eq!(
            methods(actual, "krusty", class),
            methods(&expected, "kotlinc", class),
            "{tag}: {class} methods differ from kotlinc"
        );
    }
    let _ = std::fs::remove_dir_all(dir);
}

const ABSTRACT_LIST_SIZE: &str = "class Pair2<T>(val a: T, val b: T) : AbstractList<T>() {\n\
    override val size: Int get() = 2\n\
    override fun get(index: Int): T = if (index == 0) a else b\n\
}\n\
fun box(): String {\n\
    val pair = Pair2(\"x\", \"y\")\n\
    val erased: Collection<String> = pair\n\
    return if (pair.joinToString() == \"x, y\" && erased.size == 2) \"OK\" else \"fail\"\n\
}\n";

/// `AbstractList` inherits `AbstractCollection`'s final `size()`, so the override publishes only
/// its open `getSize()`; a `size()` of its own made the class unloadable.
#[test]
fn a_stdlib_abstract_list_subclass_inherits_the_final_size_bridge() {
    assert_eq!(run(ABSTRACT_LIST_SIZE), "OK");
    assert_methods_match_kotlinc("AbstractListSize", ABSTRACT_LIST_SIZE, &["Pair2"]);
}

const SOURCE_COLLECTION_CHAIN: &str = "abstract class Base : Collection<String> {\n\
    override val size: Int get() = 0\n\
    override fun isEmpty(): Boolean = size == 0\n\
    override fun iterator(): Iterator<String> = emptyList<String>().iterator()\n\
    override fun containsAll(elements: Collection<String>): Boolean = false\n\
    override fun contains(element: String): Boolean = false\n\
}\n\
abstract class Middle : Base()\n\
class Leaf : Middle() { override val size: Int get() = 3 }\n\
fun box(): String {\n\
    val erased: Collection<String> = Leaf()\n\
    return if (erased.size == 3 && !erased.isEmpty()) \"OK\" else \"fail\"\n\
}\n";

/// A source superclass that overrides `Collection.size` owns the `size()` bridge; the subclass,
/// even through an intermediate class that declares nothing, publishes only `getSize()`.
#[test]
fn a_subclass_of_a_source_collection_does_not_redeclare_its_size_bridge() {
    assert_eq!(run(SOURCE_COLLECTION_CHAIN), "OK");
    assert_methods_match_kotlinc("SourceCollectionChain", SOURCE_COLLECTION_CHAIN, &["Leaf"]);
}

const NUMBER_CHAIN: &str = "abstract class Base : Number() {\n\
    override fun toInt(): Int = 1\n\
    override fun toLong(): Long = 1\n\
    override fun toDouble(): Double = 1.0\n\
    override fun toFloat(): Float = 1f\n\
    override fun toShort(): Short = 1\n\
    override fun toByte(): Byte = 1\n\
}\n\
class Two : Base() { override fun toInt(): Int = 2 }\n\
fun box(): String {\n\
    val erased: Number = Two()\n\
    return if (erased.toInt() == 2) \"OK\" else \"fail\"\n\
}\n";

/// The function half: `Number.toInt` is realized as `intValue()`, owned by the first Kotlin class
/// that overrides it.
#[test]
fn a_subclass_of_a_source_number_does_not_redeclare_its_int_value_bridge() {
    assert_eq!(run(NUMBER_CHAIN), "OK");
    assert_methods_match_kotlinc("NumberChain", NUMBER_CHAIN, &["Two"]);
}

/// A dependency compiled by kotlinc carries the bridge `final`: a redeclared `size()` in the
/// subclass is rejected when the class loads, not merely a different class file.
#[test]
fn a_subclass_of_a_kotlinc_compiled_collection_loads() {
    const LIB: &str = "package lib\n\
        abstract class Base : Collection<String> {\n\
        \x20   override val size: Int get() = 0\n\
        \x20   override fun isEmpty(): Boolean = size == 0\n\
        \x20   override fun iterator(): Iterator<String> = emptyList<String>().iterator()\n\
        \x20   override fun containsAll(elements: Collection<String>): Boolean = false\n\
        \x20   override fun contains(element: String): Boolean = false\n\
        }\n";
    const MAIN: &str = "import lib.Base\n\
        class Leaf : Base() { override val size: Int get() = 4 }\n\
        fun box(): String {\n\
        \x20   val erased: Collection<String> = Leaf()\n\
        \x20   return if (erased.size == 4) \"OK\" else \"fail: ${erased.size}\"\n\
        }\n";
    let Some(result) = common::expect_box_run_against_kotlinc(LIB, MAIN) else {
        eprintln!("skip: kotlinc unavailable");
        return;
    };
    assert_eq!(result, "OK");
}

/// A Java superclass realizes `Collection.size` directly under its JVM name rather than owning a
/// Kotlin renamed bridge. The Kotlin subclass must therefore still emit `size()` for its `size`
/// property; inheriting the Java implementation would return the stale superclass value.
#[test]
fn a_java_superclass_does_not_suppress_the_size_bridge() {
    let java = [(
        "lib/Base.java".to_string(),
        "package lib;\n\
         public class Base extends java.util.AbstractCollection<String> {\n\
         \x20   @Override public int size() { return 1; }\n\
         \x20   @Override public java.util.Iterator<String> iterator() {\n\
         \x20       return java.util.Collections.emptyIterator();\n\
         \x20   }\n\
         }\n"
        .to_string(),
    )];
    let Some((classes, _)) = common::javac_compile(&java, &[]) else {
        panic!("javac must compile the Java collection fixture");
    };
    let root = classes.parent().map(std::path::Path::to_path_buf);
    let jdk = common::jdk_modules();
    let classpath = vec![classes, common::stdlib_jar()];
    let source = "import lib.Base\n\
         class Leaf : Base() {\n\
         \x20   override val size: Int get() = 5\n\
         }\n\
         fun box(): String {\n\
         \x20   val erased: Collection<String> = Leaf()\n\
         \x20   return if (erased.size == 5) \"OK\" else \"fail: ${erased.size}\"\n\
         }\n";
    let result =
        common::compile_and_run_box(source, "JavaSuperclass", &classpath, Some(jdk.as_path()));
    if result.is_none() {
        panic!(
            "Java superclass fixture failed: {:?}",
            common::front_end_diagnostics(source, &classpath, Some(jdk.as_path()))
        );
    }
    if let Some(root) = root {
        let _ = std::fs::remove_dir_all(root);
    }
    assert_eq!(result.as_deref(), Some("OK"));
}

/// A superclass property that merely shares the name does not override `Collection.size`, so it
/// owns no bridge and the implementing class still needs its own `size()`.
#[test]
fn an_unrelated_superclass_property_leaves_the_size_bridge_to_the_implementation() {
    const SRC: &str = "open class Box { open val size: Int get() = 0 }\n\
        class Strings : Box(), Collection<String> {\n\
        \x20   override val size: Int get() = 5\n\
        \x20   override fun isEmpty(): Boolean = false\n\
        \x20   override fun iterator(): Iterator<String> = emptyList<String>().iterator()\n\
        \x20   override fun containsAll(elements: Collection<String>): Boolean = false\n\
        \x20   override fun contains(element: String): Boolean = false\n\
        }\n\
        fun box(): String {\n\
        \x20   val erased: Collection<String> = Strings()\n\
        \x20   return if (erased.size == 5) \"OK\" else \"fail\"\n\
        }\n";
    assert_eq!(run(SRC), "OK");
}
