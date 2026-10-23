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

const NOT_NULL: &str = "package org.jetbrains.annotations;\n\
    import java.lang.annotation.*;\n\
    @Retention(RetentionPolicy.CLASS)\n\
    @Target({ElementType.METHOD, ElementType.FIELD, ElementType.PARAMETER})\n\
    public @interface NotNull { String value() default \"\"; }\n";

const NULLABLE: &str = "package org.jetbrains.annotations;\n\
    import java.lang.annotation.*;\n\
    @Retention(RetentionPolicy.CLASS)\n\
    @Target({ElementType.METHOD, ElementType.FIELD, ElementType.PARAMETER})\n\
    public @interface Nullable { String value() default \"\"; }\n";

/// A nullability annotation on a raw occurrence moves the interval's faces: `@NotNull` makes the
/// upper face non-null, `@Nullable` the lower face nullable too.
const ANNOTATED: &str = "import org.jetbrains.annotations.*;\n\
    public class Annotated {\n\
    \x20   public static class Box<T extends CharSequence> { public T get() { return null; } }\n\
    \x20   @SuppressWarnings(\"rawtypes\") public static @NotNull Box notNull() { return new Box<String>(); }\n\
    \x20   @SuppressWarnings(\"rawtypes\") public static @Nullable Box nullable() { return null; }\n\
    }\n";

const ANNOTATED_REJECTED: &str = "fun notNull(): Int = Annotated.notNull()\n\
    fun nullable(): Int = Annotated.nullable()\n";

/// Each face's nullability is published in the inferred results' metadata.
const ANNOTATED_ACCEPTED: &str = "fun notNull() = Annotated.notNull()\n\
    fun nullable() = Annotated.nullable()\n\
    fun box(): String = if (notNull().get() == null && nullable() == null) \"OK\" else \"fail\"\n";

#[test]
fn an_annotated_java_raw_type_reports_its_faces_like_kotlinc() {
    let classes = annotated_classes();
    let result = common::compiler_diagnostics(
        &[("AnnotatedRaw.kt", ANNOTATED_REJECTED)],
        std::slice::from_ref(&classes),
    );
    let mismatch = |line, column, actual: &str| common::CompilerError {
        file: "AnnotatedRaw.kt".to_string(),
        line,
        column,
        message: format!("return type mismatch: expected 'Int', actual '{actual}'."),
    };
    let expected = vec![
        mismatch(1, 22, "(Annotated.Box<CharSequence!>..Annotated.Box<*>)"),
        mismatch(2, 23, "(Annotated.Box<CharSequence!>?..Annotated.Box<*>?)"),
    ];
    assert_eq!(
        common::compiler_errors(&result.reference_stderr),
        expected,
        "kotlinc's diagnostics changed",
    );
    let mut krusty = common::compiler_errors(&result.krusty_stderr);
    krusty.extend(common::compiler_errors(&result.krusty_stdout));
    assert_eq!(krusty, expected);
}

#[test]
fn an_annotated_java_raw_type_publishes_its_faces_like_kotlinc() {
    let classes = annotated_classes();
    common::assert_classes_identical_to_kotlinc_against_jdk(
        "AnnotatedJavaRawType",
        ANNOTATED_ACCEPTED,
        &["AnnotatedJavaRawTypeKt"],
        &[classes],
    );
}

fn annotated_classes() -> std::path::PathBuf {
    common::javac_compile(
        &[
            (
                "org/jetbrains/annotations/NotNull.java".to_string(),
                NOT_NULL.to_string(),
            ),
            (
                "org/jetbrains/annotations/Nullable.java".to_string(),
                NULLABLE.to_string(),
            ),
            ("Annotated.java".to_string(), ANNOTATED.to_string()),
        ],
        &[],
    )
    .expect("pooled javac compiles the annotated Java fixture")
    .0
}

/// A raw receiver's members are erased whole, after the receiver's own formals are substituted:
/// an inherited `List<P> nested()` is a raw `List` (so any element can be added), while the
/// inherited `put(P)` still takes `C`'s erasure. Method formals erase to their bounds and are no
/// longer inferred. An exact receiver and a static generic method keep ordinary inference.
const FAMILY: &str = "import java.util.*;\n\
    public class Family {\n\
    \x20   public static class Parent<P> {\n\
    \x20       private final List<P> items = new ArrayList<P>();\n\
    \x20       public List<P> nested() { return items; }\n\
    \x20       public void put(P p) { items.add(p); }\n\
    \x20   }\n\
    \x20   public static class Child<C extends CharSequence> extends Parent<C> {\n\
    \x20       public <U> U echo(U u) { return u; }\n\
    \x20       public <U extends CharSequence> U bounded(U u) { return u; }\n\
    \x20       public static <U> U staticEcho(U u) { return u; }\n\
    \x20       public Map<String, C> map() { return new HashMap<String, C>(); }\n\
    \x20       public Rec<?> rec() { return null; }\n\
    \x20   }\n\
    \x20   public static class Rec<T extends Comparable<T>> { public void set(T t) {} }\n\
    \x20   @SuppressWarnings(\"rawtypes\") public static Child child() { return new Child<String>(); }\n\
    \x20   @SuppressWarnings(\"rawtypes\") public static Rec rec() { return new Rec<String>(); }\n\
    }\n";

const FAMILY_REJECTED: &str = "fun inheritedPut() = Family.child().put(1)\n\
    fun echo(): String = Family.child().echo(\"x\")\n\
    fun bounded(): String = Family.child().bounded(\"x\")\n\
    fun explicit(): Int = Family.child().echo<String>(\"x\")\n\
    fun nested(): Int = Family.child().nested()\n\
    fun map(): Int = Family.child().map()\n\
    fun recursive() = Family.rec().set(Any())\n\
    fun projected(): Int = Family.child().rec()\n\
    fun exactNested(c: Family.Child<String>): Int = c.nested()\n\
    fun exactEcho(c: Family.Child<String>): Int = c.echo(\"x\")\n\
    fun staticEcho(): Int = Family.Child.staticEcho(\"x\")\n";

const FAMILY_ACCEPTED: &str = "fun box(): String {\n\
    \x20   val child = Family.child()\n\
    \x20   child.nested().add(1)\n\
    \x20   child.nested().add(\"s\")\n\
    \x20   child.put(\"p\")\n\
    \x20   val echoed: Any? = child.echo(\"e\")\n\
    \x20   val bounded: CharSequence = child.bounded(\"bb\")\n\
    \x20   val explicit: Any? = child.echo<String>(\"x\")\n\
    \x20   val exact: String = Family.Child<String>().echo(\"exact\")\n\
    \x20   val static: String = Family.Child.staticEcho(\"st\")\n\
    \x20   Family.rec().set(\"r\")\n\
    \x20   Family.rec().set(2)\n\
    \x20   val size = child.nested().size + child.map().size\n\
    \x20   return if (size == 3 && echoed == \"e\" && bounded.length == 2 && explicit == \"x\" &&\n\
    \x20       exact.length == 5 && static.length == 2) \"OK\" else \"size $size\"\n\
    }\n";

#[test]
fn a_java_raw_member_is_erased_like_kotlinc() {
    common::assert_errors_match_kotlinc(
        &[("Family.java", FAMILY), ("main.kt", FAMILY_REJECTED)],
        &[],
    );
}

#[test]
fn a_java_raw_member_compiles_like_kotlinc() {
    common::assert_classes_identical_to_kotlinc_against_jdk(
        "JavaRawMember",
        FAMILY_ACCEPTED,
        &["JavaRawMemberKt"],
        &[family_classes()],
    );
}

#[test]
fn a_java_raw_member_runs_like_kotlinc() {
    let krusty =
        common::java_interop_box("JavaRawMember", &[("Family.java", FAMILY)], FAMILY_ACCEPTED);
    assert_eq!(krusty, "OK");
    assert_eq!(
        common::kotlinc_box_files_result_with_classpath(
            &[("JavaRawMember.kt", FAMILY_ACCEPTED)],
            "JavaRawMemberKt",
            &[family_classes()],
        ),
        krusty
    );
}

fn family_classes() -> std::path::PathBuf {
    common::javac_compile(&[("Family.java".to_string(), FAMILY.to_string())], &[])
        .expect("pooled javac compiles the Java fixture")
        .0
}
