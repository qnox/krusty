//! A Java member typed by its class's formal carries that formal's declared bound. For
//! `class Box<T extends CharSequence>`, `T get()` is a `CharSequence` wherever no type argument
//! replaces `T`, so `.length` resolves to `CharSequence.length`. krusty bounded every class-formal
//! occurrence by the parser's `Any` placeholder: through a raw `Box` (a Java method returning the
//! raw type) it rejected the access. Through `Box<*>` the capture already reads the classifier's
//! declared bound; those accesses pin that agreement. The bound reaches method returns, fields,
//! generic arguments (`List<T>`), and method formal bounds (`<U extends T>`); a method formal of
//! the same name (`<T> T shadow(T)`) shadows the class formal. A chain of formals
//! (`Chain<A extends CharSequence, B extends A, C extends B>`) carries the complete chain, so a
//! `C` read through a raw `Chain` is a `CharSequence` too. A non-static member class reads its
//! enclosing class's formals the same way (`Outer<T extends CharSequence>.Inner.get()`). The diagnostics read the fixture as
//! Java source (through its signature stub); the classes and the run read javac's class files, so
//! both Java providers are covered.

use super::common;

const BOX: &str = "public class Box<T extends CharSequence> {\n\
    \x20   public T value;\n\
    \x20   private final T held;\n\
    \x20   public Box(T held) { this.held = held; this.value = held; }\n\
    \x20   public T get() { return held; }\n\
    \x20   public java.util.List<T> all() { return java.util.Collections.singletonList(held); }\n\
    \x20   @SuppressWarnings(\"unchecked\") public <U extends T> U pick() { return (U) held; }\n\
    \x20   public <T> T shadow(T t) { return t; }\n\
    \x20   @SuppressWarnings(\"rawtypes\") public static Box raw() { return new Box<String>(\"raw\"); }\n\
    }\n";

const CHAIN: &str = "public class Chain<A extends CharSequence, B extends A, C extends B> {\n\
    \x20   public C value;\n\
    \x20   public Chain(C value) { this.value = value; }\n\
    \x20   public C get() { return value; }\n\
    \x20   @SuppressWarnings(\"unchecked\") public <D extends C> D pick() { return (D) value; }\n\
    \x20   @SuppressWarnings(\"rawtypes\") public static Chain raw() { return new Chain<String, String, String>(\"chain\"); }\n\
    }\n";

const OUTER: &str = "public class Outer<T extends CharSequence> {\n\
    \x20   private final T held;\n\
    \x20   public Outer(T held) { this.held = held; }\n\
    \x20   public class Inner {\n\
    \x20       public T value = held;\n\
    \x20       public T get() { return held; }\n\
    \x20   }\n\
    \x20   @SuppressWarnings(\"rawtypes\") public static Outer.Inner raw() { return new Outer<String>(\"inner\").new Inner(); }\n\
    }\n";

const ACCESS: &str = "fun returned(x: Box<*>): Int = x.get().length\n\
    fun field(x: Box<*>): Int = x.value.length\n\
    fun element(x: Box<*>): Int = x.all()[0].length\n\
    fun methodBound(x: Box<*>): CharSequence = x.pick()\n\
    fun rawReturned(): Int = Box.raw().get().length\n\
    fun rawField(): Int = Box.raw().value.length\n\
    fun rawMethodBound(): CharSequence = Box.raw().pick()\n\
    fun rawChained(): Int = Chain.raw().get().length\n\
    fun rawChainedField(): Int = Chain.raw().value.length\n\
    fun rawChainedMethodBound(): CharSequence = Chain.raw().pick()\n\
    fun inner(x: Outer<*>.Inner): Int = x.get().length\n\
    fun innerField(x: Outer<*>.Inner): Int = x.value.length\n\
    fun rawInner(): Int = Outer.raw().get().length\n\
    fun rawInnerField(): Int = Outer.raw().value.length\n\
    fun box(): String {\n\
    \x20   val x: Box<*> = Box(\"abc\")\n\
    \x20   val lengths = returned(x) + field(x) + element(x) + methodBound(x).length\n\
    \x20   val raw = rawReturned() + rawField() + rawMethodBound().length\n\
    \x20   val chain = rawChained() + rawChainedField() +\n\
    \x20       rawChainedMethodBound().length\n\
    \x20   val inners = inner(Outer.raw()) + innerField(Outer.raw()) + rawInner() + rawInnerField()\n\
    \x20   return if (lengths == 12 && raw == 9 && chain == 15 && inners == 20) \"OK\"\n\
    \x20   else \"lengths $lengths raw $raw chain $chain inners $inners\"\n\
    }\n";

const SHADOWED: &str = "fun returned(x: Box<*>): Int = x.get().length\n\
    fun field(x: Box<*>): Int = x.value.length\n\
    fun element(x: Box<*>): Int = x.all()[0].length\n\
    fun methodBound(x: Box<*>): CharSequence = x.pick()\n\
    fun shadowed(x: Box<*>): Int = x.shadow(1).length\n\
    fun rawReturned(): Int = Box.raw().get().length\n\
    fun rawField(): Int = Box.raw().value.length\n\
    fun rawMethodBound(): CharSequence = Box.raw().pick()\n\
    fun rawChained(): Int = Chain.raw().get().length\n\
    fun rawChainedField(): Int = Chain.raw().value.length\n\
    fun rawChainedMethodBound(): CharSequence = Chain.raw().pick()\n\
    fun inner(x: Outer<*>.Inner): Int = x.get().length\n\
    fun innerField(x: Outer<*>.Inner): Int = x.value.length\n\
    fun rawInner(): Int = Outer.raw().get().length\n\
    fun rawInnerField(): Int = Outer.raw().value.length\n";

fn javac_classes() -> std::path::PathBuf {
    common::javac_compile(
        &[
            ("Box.java".to_string(), BOX.to_string()),
            ("Chain.java".to_string(), CHAIN.to_string()),
            ("Outer.java".to_string(), OUTER.to_string()),
        ],
        &[],
    )
    .expect("pooled javac compiles the Java fixture")
    .0
}

#[test]
fn a_java_source_class_formal_keeps_its_declared_bound_and_a_method_formal_shadows_it() {
    common::assert_errors_match_kotlinc(
        &[
            ("Box.java", BOX),
            ("Chain.java", CHAIN),
            ("Outer.java", OUTER),
            ("main.kt", SHADOWED),
        ],
        &[],
    );
}

#[test]
fn a_java_class_formal_member_is_accessed_through_its_declared_bound_like_kotlinc() {
    common::assert_classes_identical_to_kotlinc_against_jdk(
        "JavaClassFormalBound",
        ACCESS,
        &["JavaClassFormalBoundKt"],
        &[javac_classes()],
    );
}

#[test]
fn a_java_class_formal_member_runs_through_its_declared_bound_like_kotlinc() {
    let krusty = common::java_interop_box(
        "JavaClassFormalBound",
        &[
            ("Box.java", BOX),
            ("Chain.java", CHAIN),
            ("Outer.java", OUTER),
        ],
        ACCESS,
    );
    assert_eq!(krusty, "OK");
    assert_eq!(
        common::kotlinc_box_files_result_with_classpath(
            &[("JavaClassFormalBound.kt", ACCESS)],
            "JavaClassFormalBoundKt",
            &[javac_classes()],
        ),
        krusty
    );
}
