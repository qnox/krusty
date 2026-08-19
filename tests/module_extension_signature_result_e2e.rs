//! The RESULT of calling a same-module extension, as seen by signature inference.
//!
//! Selection finds a module-declared extension and picks it as the overload, but the facet that
//! carried a call's result was an EMIT handle — a library callable — and a same-module extension
//! emits through the module path instead, so it was absent. Asking "what does this name return on
//! this receiver" then answered nothing, and a member property initialized through such a call could
//! not be typed at all: "cannot infer the type of property". The same call in a LOCAL val was fine,
//! since the full checker types those through a different route.
use super::common;

#[test]
fn a_property_initialized_by_a_module_extension_is_typed() {
    const MAIN: &str = "package repro\n\
        class ItemDto(val id: String)\n\
        class Item(val id: String)\n\
        fun ItemDto.toDomain(): Item = Item(id)\n\
        class Repo {\n\
        \x20   private val direct = ItemDto(\"a\").toDomain()\n\
        \x20   fun value(): String = direct.id\n\
        }\n\
        fun box(): String = if (Repo().value() == \"a\") \"OK\" else \"fail\"\n";
    common::expect_box_ok_files_with_stdlib(&[("Main.kt", MAIN)], "a module extension's result");
}

#[test]
fn the_extension_may_be_declared_in_another_file() {
    // The declaration and the use are collected separately, so this pins that the answer comes from
    // the module's own symbols rather than from anything file-local.
    const DOMAIN: &str = "package repro\n\
        class ItemDto(val id: String)\n\
        class Item(val id: String)\n";
    const MAPPER: &str = "package repro\n\
        fun ItemDto.toDomain(): Item = Item(id)\n";
    const REPO: &str = "package repro\n\
        class Repo {\n\
        \x20   private val direct = ItemDto(\"b\").toDomain()\n\
        \x20   fun value(): String = direct.id\n\
        }\n\
        fun box(): String = if (Repo().value() == \"b\") \"OK\" else \"fail\"\n";
    common::expect_box_ok_files_with_stdlib(
        &[
            ("Domain.kt", DOMAIN),
            ("Mapper.kt", MAPPER),
            ("Repo.kt", REPO),
        ],
        "a module extension from another file",
    );
}

#[test]
fn a_generic_module_extension_binds_its_result() {
    // The result is bound from the receiver and the arguments, exactly as the emit handle binds it —
    // reporting the declared return unbound would type the property as the type variable itself.
    const MAIN: &str = "package repro\n\
        fun <T> List<T>.secondOrNull(): T? = if (size > 1) this[1] else null\n\
        fun <T> List<T>.pairedWith(other: T): List<T> = this + other\n\
        class Repo {\n\
        \x20   private val second = listOf(\"a\", \"bb\").secondOrNull()\n\
        \x20   private val paired = listOf(\"a\").pairedWith(\"cc\")\n\
        \x20   fun value(): String =\n\
        \x20       (second?.length ?: 0).toString() + \"/\" + paired.joinToString(\"-\") { it.uppercase() }\n\
        }\n\
        fun box(): String = if (Repo().value() == \"2/A-CC\") \"OK\" else \"fail: \" + Repo().value()\n";
    common::expect_box_ok_files_with_stdlib(&[("Main.kt", MAIN)], "a generic module extension");
}

#[test]
fn a_classpath_extension_still_answers_the_same_way() {
    // The must-not-touch side: the emit handle is unchanged and still takes precedence, so a
    // classpath extension — including one with defaults and one that is `inline` — keeps whatever it
    // resolved to before.
    const MAIN: &str = "package repro\n\
        class Repo {\n\
        \x20   private val joined = listOf(\"a\", \"bb\").joinToString(\"-\")\n\
        \x20   private val trimmed = \"ab..!!\".trimEnd('!', '.')\n\
        \x20   private val firstLong = listOf(\"a\", \"bb\").first { it.length > 1 }\n\
        \x20   fun value(): String = joined + \"/\" + trimmed + \"/\" + firstLong\n\
        }\n\
        fun box(): String = if (Repo().value() == \"a-bb/ab/bb\") \"OK\" else \"fail: \" + Repo().value()\n";
    common::expect_box_ok_files_with_stdlib(&[("Main.kt", MAIN)], "classpath extensions");
}

#[test]
fn a_generic_extension_result_is_the_one_kotlinc_writes() {
    // These shapes USED to decline. The signature pre-pass aligned a `vararg` call against the
    // ARRAY parameter, so `T` bound from nothing and specialized to its bound — reporting `Any`,
    // or `CharSequence` for a bounded variable, where kotlinc writes `String`. Declining was the
    // recoverable answer to a question that pass could not answer.
    //
    // With one typer there is no second pass to disagree, and the checker determines all of them.
    // The assertion is therefore the emitted FIELD DESCRIPTOR against the reference compiler's, not
    // a diagnostic: a decline here would now be a regression, and so would agreeing with neither.
    for (label, descriptor, source) in [
        (
            "vararg",
            "java.lang.String",
            "package repro\n\
             class Src\n\
             fun <T> Src.firstOf(vararg xs: T): T = xs[0]\n\
             class Holder { val v = Src().firstOf(\"a\", \"bb\") }\n\
             fun box(): String = \"OK\"\n",
        ),
        (
            // The element type survives: `Array<String>`, whose JVM signature is `[Ljava/lang/String;`
            // — spelling it `Lkotlin/Array<Ljava/lang/String;>;` is not a signature a consumer reads.
            "array argument to a vararg",
            "java.lang.String[]",
            "package repro\n\
             class Src\n\
             fun <T> Src.of(vararg xs: T): T = xs[0]\n\
             class Holder { val v = Src().of(arrayOf(\"a\", \"bb\")) }\n\
             fun box(): String = \"OK\"\n",
        ),
        (
            "type-variable receiver",
            "java.lang.String",
            "package repro\n\
             fun <T> T?.orDef(d: T): T = this ?: d\n\
             class Holder { val s: String? = null; val v = s.orDef(\"zz\") }\n\
             fun box(): String = \"OK\"\n",
        ),
        (
            // A variable "bound" to ITSELF is not bound, and an unbound one erases to its OWN bound:
            // this emitted `CharSequence` where kotlinc writes `String`.
            "type-variable receiver under a bound",
            "java.lang.String",
            "package repro\n\
             fun <T : CharSequence> T?.orDef(d: T): T = this ?: d\n\
             class Holder { val s: String? = null; val v = s.orDef(\"zz\") }\n\
             fun box(): String = \"OK\"\n",
        ),
        (
            // One formal reached from two disagreeing arguments joins rather than keeping the first.
            "one formal from two disagreeing arguments",
            "java.lang.Object",
            "package repro\n\
             class Src\n\
             fun <T> Src.pick(a: T, b: T): T = a\n\
             class Holder { val v = Src().pick(\"a\", 1) }\n\
             fun box(): String = \"OK\"\n",
        ),
    ] {
        let classes = common::expect_classes_with_stdlib(source, "Main");
        let holder = classes
            .iter()
            .find(|(name, _)| name.ends_with("Holder"))
            .map(|(_, bytes)| bytes.clone())
            .unwrap_or_else(|| panic!("{label}: repro.Holder was not emitted"));
        let scratch = common::scratch_dir().expect("scratch filesystem unavailable");
        let path = scratch.join(format!("gen-ext-{}.class", label.replace(' ', "-")));
        std::fs::write(&path, holder).expect("write class file");
        let text = common::javap(&["-p", path.to_str().expect("utf-8 path")])
            .expect("pooled javap unavailable");
        let declared = text
            .lines()
            .find(|line| line.trim_end().ends_with(" v;"))
            .unwrap_or_default()
            .trim()
            .to_string();
        assert!(
            declared.contains(descriptor),
            "{label}: expected the property to be {descriptor}, got {declared:?}"
        );
    }
}

#[test]
fn a_bounded_generic_extension_still_binds_from_its_arguments() {
    // The other side of the determinacy rule: refusing must be narrow. A bounded formal that the
    // receiver and argument really do pin keeps its inferred type, byte-for-byte as kotlinc emits
    // it — declining here would trade a wrong answer for a needless one.
    const MAIN: &str = "package repro\n\
        fun <T : CharSequence> T.twice(d: T): T = d\n\
        fun <T> List<T>.firstOr(d: T): T = firstOrNull() ?: d\n\
        class Holder {\n\
        \x20   private val doubled = \"a\".twice(\"bb\")\n\
        \x20   private val first = listOf(\"x\").firstOr(\"y\")\n\
        \x20   fun value(): String = doubled + \"/\" + first + \"/\" + doubled.length\n\
        }\n\
        fun box(): String = if (Holder().value() == \"bb/x/2\") \"OK\" else \"fail: \" + Holder().value()\n";
    common::expect_box_ok_files_with_stdlib(&[("Main.kt", MAIN)], "a bounded generic extension");
}
