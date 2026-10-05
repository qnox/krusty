//! Parameter entry guards on the members a value class realizes as statics.
//!
//! kotlinc guards a parameter of a non-private, non-synthetic function when its type, with every
//! value class unwrapped to its carrier, is non-null and its JVM type is not primitive. The static
//! `constructor-impl` replacing a value class's constructor is such a function, so it guards a
//! non-null reference carrier (`Tag(val text: String)`), as does each secondary constructor's
//! `constructor-impl` for its own parameters. A computed accessor realized as a static over the
//! carrier guards its declared value parameter, never the carrier, and the box's interface entry
//! repeats that guard. A regular class's secondary constructor taking a value class is private in
//! the class file behind a marker overload, but its declaration is public, so the private
//! constructor still guards its parameters. A primitive carrier (`Count`) and one that admits null
//! (`Maybe`) are never guarded. A write through a computed setter calls that static with the
//! carrier as its receiver.

use super::common;

const SOURCE: &str = "@JvmInline value class Tag(val text: String)\n\
                      @JvmInline value class Count(val n: Int)\n\
                      @JvmInline value class Maybe(val text: String?)\n\
                      @JvmInline value class Nested(val tag: Tag)\n\
                      interface Labelled { var label: Tag }\n\
                      @JvmInline value class Name(val text: String) : Labelled {\n\
                      \x20   constructor(prefix: String, count: Count) : this(prefix + count.n)\n\
                      \x20   var title: String\n\
                      \x20       get() = text\n\
                      \x20       set(value) { sink = value + text }\n\
                      \x20   var tag: Tag\n\
                      \x20       get() = Tag(text)\n\
                      \x20       set(value) { sink = value.text }\n\
                      \x20   var count: Count\n\
                      \x20       get() = Count(text.length)\n\
                      \x20       set(value) { sink = text + value.n }\n\
                      \x20   override var label: Tag\n\
                      \x20       get() = Tag(text)\n\
                      \x20       set(value) { sink = value.text + text }\n\
                      }\n\
                      @JvmInline value class Label(val text: String) {\n\
                      \x20   constructor(tag: Tag, maybe: Maybe, suffix: String?) : this(tag.text)\n\
                      }\n\
                      class Holder(val first: Int) {\n\
                      \x20   constructor(text: String, tag: Tag) : this(text.length + tag.text.length)\n\
                      \x20   constructor(maybe: Maybe, count: Count, text: String) : this(text.length)\n\
                      }\n\
                      var sink = \"\"\n\
                      @Suppress(\"UNCHECKED_CAST\")\n\
                      fun <T> smuggle(value: Any?): T = value as T\n\
                      fun rejected(block: () -> Unit): String =\n\
                      \x20   try { block(); \"accepted\" } catch (e: NullPointerException) { e.message ?: \"no message\" }\n\
                      fun box(): String {\n\
                      \x20   val name = Name(\"n\")\n\
                      \x20   name.title = \"t\"\n\
                      \x20   if (sink != \"tn\") return \"title $sink\"\n\
                      \x20   name.tag = Tag(\"g\")\n\
                      \x20   if (sink != \"g\") return \"tag $sink\"\n\
                      \x20   name.count = Count(2)\n\
                      \x20   if (sink != \"n2\") return \"count $sink\"\n\
                      \x20   val labelled: Labelled = name\n\
                      \x20   labelled.label = Tag(\"l\")\n\
                      \x20   if (sink != \"ln\") return \"label $sink\"\n\
                      \x20   if (Name(\"p\", Count(3)).text != \"p3\") return \"secondary\"\n\
                      \x20   if (Label(Tag(\"q\"), Maybe(null), null).text != \"q\") return \"secondary tag\"\n\
                      \x20   if (Nested(Tag(\"x\")).tag.text != \"x\") return \"nested\"\n\
                      \x20   if (Holder(\"ab\", Tag(\"c\")).first != 3) return \"holder\"\n\
                      \x20   return rejected { Tag(smuggle(null)) } + \"\\n\" +\n\
                      \x20       rejected { name.title = smuggle(null) } + \"\\n\" +\n\
                      \x20       rejected { Name(smuggle<String>(null), Count(1)) } + \"\\n\" +\n\
                      \x20       rejected { Holder(smuggle<String>(null), Tag(\"t\")) } + \"\\n\" +\n\
                      \x20       rejected { Maybe(smuggle(null)) }\n\
                      }\n";

/// Each value class declares kotlinc's members with kotlinc's guards, labels and positions.
#[test]
fn value_class_statics_guard_their_parameters_like_kotlinc() {
    for class in ["Tag", "Count", "Name"] {
        common::assert_class_code_matches_kotlinc_jdk("ValueClassParameterGuards", SOURCE, class);
    }
}

/// A nested carrier's `constructor-impl` and a regular class's hidden secondary constructors carry
/// kotlinc's guards (and, for the hidden ones, kotlinc's absent generic `Signature`).
#[test]
fn nested_and_hidden_constructors_guard_like_kotlinc() {
    // Only `tag` is guarded: `Maybe`'s carrier admits null and `suffix` is nullable. (The
    // annotations kotlinc writes on this `constructor-impl` follow the declared value-class types,
    // a separate realization, so the instructions are compared here.)
    let label = common::compare_with_kotlinc_plugin_jdk(
        "ValueClassParameterGuards",
        SOURCE,
        "Label",
        "17",
        &[],
    )
    .expect("reference kotlinc and javap are provisioned");
    let marker = "constructor-impl(java.lang.String, java.lang.String, java.lang.String);";
    let reference = common::method_instructions(&label.reference, marker);
    assert!(!reference.is_empty(), "kotlinc declares Label.{marker}");
    assert_eq!(
        common::method_instructions(&label.krusty, marker),
        reference,
        "Label: kotlinc's {marker}"
    );
    let members = [
        ("Nested", "public static java.lang.String constructor-impl(java.lang.String);"),
        ("Holder", "private Holder(java.lang.String, java.lang.String);"),
        ("Holder", "private Holder(java.lang.String, int, java.lang.String);"),
        (
            "Holder",
            "public Holder(java.lang.String, java.lang.String, kotlin.jvm.internal.DefaultConstructorMarker);",
        ),
    ];
    for (class, header) in members {
        let comparison = common::compare_with_kotlinc_plugin_jdk(
            "ValueClassParameterGuards",
            SOURCE,
            class,
            "17",
            &[],
        )
        .expect("reference kotlinc and javap are provisioned");
        let reference = common::method_block(&comparison.reference, header);
        assert!(!reference.is_empty(), "kotlinc declares {class}.{header}");
        assert_eq!(
            common::method_block(&comparison.krusty, header),
            reference,
            "{class}: kotlinc's {header}"
        );
    }
}

/// A null smuggled past the caller's types reaches each guard and fails with kotlinc's message;
/// a carrier that admits null accepts it.
#[test]
fn value_class_statics_reject_a_smuggled_null() {
    let output = common::compile_and_run_box(
        SOURCE,
        "ValueClassParameterGuards",
        &[common::stdlib_jar()],
        Some(common::jdk_modules().as_path()),
    )
    .expect("krusty compiles and the JVM runs the box function");
    assert_eq!(
        output,
        "Parameter specified as non-null is null: method Tag.constructor-impl, parameter text\n\
         Parameter specified as non-null is null: method Name.setTitle-impl, parameter value\n\
         Parameter specified as non-null is null: method Name.constructor-impl, parameter prefix\n\
         Parameter specified as non-null is null: method Holder.<init>, parameter text\n\
         accepted"
    );
}
