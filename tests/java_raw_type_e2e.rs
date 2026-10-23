//! A Java raw type occurrence (`Box` for `class Box<T extends CharSequence>`) is the flexible
//! interval `(Box<CharSequence!>..Box<*>?)`: every argument is the formal's erasure below and a
//! star above. Its members read each formal through that erasure (`Box.raw().get()` is a
//! `CharSequence!`), it accepts any application of its classifier, and as a source it supplies
//! only its lower bound, so it is not a `List<String>`. A raw non-static member class captures its
//! enclosing class's formals the same way, and an inferred raw result is published in metadata as
//! kotlinc's raw flexible type. The diagnostics read the fixture as Java source; the
//! classes and the run read javac's class files, so both Java providers are covered.

use super::common;

const RAW: &str = "import java.util.*;\n\
    public class Raw {\n\
    \x20   public static class Box<T extends CharSequence> {\n\
    \x20       private T held;\n\
    \x20       public T get() { return held; }\n\
    \x20       public void set(T t) { held = t; }\n\
    \x20   }\n\
    \x20   public static class Plain<T> { public T value; }\n\
    \x20   public static class Rec<T extends Comparable<T>> {\n\
    \x20       public T value;\n\
    \x20       public void set(T t) { value = t; }\n\
    \x20   }\n\
    \x20   @SuppressWarnings(\"rawtypes\") public static Box box() { return new Box<String>(); }\n\
    \x20   @SuppressWarnings(\"rawtypes\") public static Plain plain() { return new Plain<String>(); }\n\
    \x20   @SuppressWarnings(\"rawtypes\") public static Rec rec() { return new Rec<String>(); }\n\
    \x20   @SuppressWarnings(\"rawtypes\") public static List list() { return new ArrayList(Arrays.asList(\"a\", \"bc\")); }\n\
    \x20   @SuppressWarnings(\"rawtypes\") public static int size(Plain p) { return p.value == null ? 0 : 1; }\n\
    \x20   public static int exact(Plain<Object> p) { return 2; }\n\
    }\n";

/// Each raw shape kotlinc reports, and the reads, writes and arguments it rejects.
const REJECTED: &str = "fun box(): Int = Raw.box()\n\
    fun plain(): Int = Raw.plain()\n\
    fun recursive(): Int = Raw.rec()\n\
    fun list(): Int = Raw.list()\n\
    fun read(): Int = Raw.box().get()\n\
    fun field(): Int = Raw.plain().value\n\
    fun element(): Int = Raw.list()[0]\n\
    fun write() = Raw.box().set(1)\n\
    val strings: List<String> = Raw.list()\n\
    fun exactArgument() = Raw.exact(Raw.Plain<String>())\n\
    fun applied(): Raw.Plain<String> = Raw.plain()\n";

/// A raw type accepts every application of its classifier and reads through its erasures. An
/// inferred public result publishes the raw interval in metadata.
const ACCEPTED: &str = "fun rawBox() = Raw.box()\n\
    fun rawList() = Raw.list()\n\
    fun box(): String {\n\
    \x20   val p = Raw.Plain<String>()\n\
    \x20   p.value = \"v\"\n\
    \x20   val sizes = Raw.size(p) + Raw.size(Raw.Plain<Int>()) + Raw.size(Raw.plain())\n\
    \x20   val mixed = Raw.plain()\n\
    \x20   mixed.value = 1\n\
    \x20   mixed.value = \"s\"\n\
    \x20   val r = Raw.rec()\n\
    \x20   r.set(\"c\")\n\
    \x20   r.set(2)\n\
    \x20   val writes = Raw.list()\n\
    \x20   writes.add(3)\n\
    \x20   writes.add(\"d\")\n\
    \x20   val b = Raw.box()\n\
    \x20   b.set(StringBuilder(\"sb\"))\n\
    \x20   b.set(null)\n\
    \x20   b.set(\"held\")\n\
    \x20   val read: CharSequence = b.get()\n\
    \x20   val star: Raw.Plain<*> = Raw.plain()\n\
    \x20   val any: Any? = star.value\n\
    \x20   val list = Raw.list()\n\
    \x20   val first: Any? = list[0]\n\
    \x20   val chars = list.size + (first as String).length\n\
    \x20   val wrote = mixed.value == \"s\" && r.value == 2 && writes.size == 4\n\
    \x20   return if (sizes == 1 && read.length == 4 && any == null && chars == 3 && wrote) \"OK\"\n\
    \x20   else \"sizes $sizes read $read any $any chars $chars\"\n\
    }\n";

#[test]
fn a_java_raw_type_reads_and_rejects_like_kotlinc() {
    common::assert_errors_match_kotlinc(&[("Raw.java", RAW), ("main.kt", REJECTED)], &[]);
}

#[test]
fn a_java_raw_type_compiles_like_kotlinc() {
    common::assert_classes_identical_to_kotlinc_against_jdk(
        "JavaRawType",
        ACCEPTED,
        &["JavaRawTypeKt"],
        &[javac_classes()],
    );
}

#[test]
fn a_java_raw_type_runs_like_kotlinc() {
    let krusty = common::java_interop_box("JavaRawType", &[("Raw.java", RAW)], ACCEPTED);
    assert_eq!(krusty, "OK");
    assert_eq!(
        common::kotlinc_box_files_result_with_classpath(
            &[("JavaRawType.kt", ACCEPTED)],
            "JavaRawTypeKt",
            &[javac_classes()],
        ),
        krusty
    );
}

fn javac_classes() -> std::path::PathBuf {
    common::javac_compile(&[("Raw.java".to_string(), RAW.to_string())], &[])
        .expect("pooled javac compiles the Java fixture")
        .0
}
