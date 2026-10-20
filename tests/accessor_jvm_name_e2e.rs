//! `@JvmName` on a compiler-default accessor (`@get:JvmName`, `@set:JvmName`, or written on a
//! bodiless `get`/`set`) is that accessor's whole JVM name: the declaration, its callers, its
//! `$annotations` marker and its metadata signature all use it, and neither the internal module
//! suffix nor value-class mangling is applied to it.

use super::common;

/// Both compilers build `sources` as one module and every class they write is byte-identical.
fn assert_module_matches_kotlinc(sources: &[(&str, &str)]) {
    let classes = common::classes_against_kotlinc_module(sources);
    let differences = classes.differences();
    assert!(
        differences.is_empty(),
        "classes differ from kotlinc's:\n\n{}",
        differences.join("\n\n")
    );
}

#[test]
fn a_default_accessor_takes_its_jvm_name_like_kotlinc() {
    assert_module_matches_kotlinc(&[(
        "Names.kt",
        "package named\n\
         \n\
         class Member {\n\
         \x20   @get:JvmName(\"isReady\") val ready: Boolean = true\n\
         \x20   val written: Int = 1\n\
         \x20       @JvmName(\"readWritten\") get\n\
         \x20   @set:JvmName(\"assign\") var value: Int = 1\n\
         }\n\
         \n\
         object Single {\n\
         \x20   @get:JvmName(\"readName\") val name = \"o\"\n\
         \x20   @set:JvmName(\"storeCount\") var count = 1\n\
         }\n\
         \n\
         @get:JvmName(\"topName\") val top = 3\n\
         @set:JvmName(\"storeTop\") var mutableTop = 1\n\
         \n\
         fun use(member: Member): Int {\n\
         \x20   member.value = 2\n\
         \x20   Single.count = 3\n\
         \x20   mutableTop = 4\n\
         \x20   return if (member.ready) member.written + member.value + Single.count + top else 0\n\
         }\n",
    )]);
}

#[test]
fn an_internal_or_value_class_accessor_keeps_its_exact_jvm_name() {
    assert_module_matches_kotlinc(&[(
        "Exact.kt",
        "package named\n\
         \n\
         @JvmInline value class Label(val text: String)\n\
         \n\
         class Exact {\n\
         \x20   @JvmName(\"compute\") internal fun f() = 1\n\
         \x20   internal val custom: Int @JvmName(\"readCustom\") get() = 2\n\
         \x20   internal val plain: Int = 3\n\
         \x20       @JvmName(\"readPlain\") get\n\
         \x20   internal var stored: Int = 4\n\
         \x20       @JvmName(\"store\") set\n\
         \x20   @get:JvmName(\"readLabel\") val label: Label = Label(\"a\")\n\
         \x20   fun use(): Int { stored = 5; return f() + custom + plain + stored + label.text.length }\n\
         }\n",
    )]);
}

#[test]
fn a_companion_default_accessor_keeps_its_written_annotations_and_jvm_name() {
    assert_module_matches_kotlinc(&[(
        "Companion.kt",
        "package named\n\
         \n\
         annotation class Marked\n\
         \n\
         class Holder {\n\
         \x20   companion object {\n\
         \x20       @get:JvmName(\"readName\") val name = \"c\"\n\
         \x20       @set:JvmName(\"storeCount\") var count = 1\n\
         \x20       @Marked @get:JvmName(\"readMarked\") val marked = 2\n\
         \x20       @get:Deprecated(\"old\") val old = 3\n\
         \x20   }\n\
         \x20   class Nested {\n\
         \x20       fun use(): Int { count = 2; return name.length + count + marked }\n\
         \x20   }\n\
         }\n",
    )]);
}

#[test]
fn a_property_reference_reads_through_the_written_accessor_name() {
    assert_module_matches_kotlinc(&[(
        "References.kt",
        "package named\n\
         \n\
         class Referenced {\n\
         \x20   @get:JvmName(\"readP\") val p: Int = 1\n\
         \x20   @set:JvmName(\"storeQ\") var q: Int = 2\n\
         \x20   @get:JvmName(\"readHidden\") internal val hidden: Int = 5\n\
         \x20   @set:JvmName(\"storeKept\") internal var kept: Int = 6\n\
         }\n\
         \n\
         @get:JvmName(\"readTop\") val top = 3\n\
         \n\
         fun use(referenced: Referenced): Int {\n\
         \x20   val p = Referenced::p\n\
         \x20   val q = referenced::q\n\
         \x20   q.set(4)\n\
         \x20   val t = ::top\n\
         \x20   val hidden = Referenced::hidden\n\
         \x20   val kept = referenced::kept\n\
         \x20   kept.set(7)\n\
         \x20   return p.get(referenced) + q.get() + t.get() + hidden.get(referenced) + kept.get()\n\
         }\n",
    )]);
}

#[test]
fn a_top_level_suppressed_source_marker_is_named_after_the_written_getter() {
    assert_module_matches_kotlinc(&[(
        "Markers.kt",
        "package named\n\
         \n\
         @Suppress(\"VOLATILE_ON_VALUE\") @Volatile @get:JvmName(\"readTop\") val top = 3\n",
    )]);
}

#[test]
fn a_sibling_file_calls_a_member_through_its_written_jvm_name() {
    assert_module_matches_kotlinc(&[
        (
            "Holder.kt",
            "package named\n\
             \n\
             class Holder {\n\
             \x20   @get:JvmName(\"readP\") val p: Int = 1\n\
             \x20   @set:JvmName(\"storeQ\") var q: Int = 2\n\
             \x20   val declared: Int @JvmName(\"readDeclared\") get() = 3\n\
             \x20   @get:JvmName(\"readHidden\") internal val hidden: Int = 4\n\
             \x20   @JvmName(\"compute\") fun f(): Int = 5\n\
             }\n\
             \n\
             object Single {\n\
             \x20   @get:JvmName(\"readName\") val name = \"o\"\n\
             }\n\
             \n\
             class Pair(@get:JvmName(\"readFirst\") val first: Int, @set:JvmName(\"storeSecond\") var second: Int)\n\
             \n\
             @get:JvmName(\"readTop\") val top: Int = 7\n",
        ),
        (
            "Use.kt",
            "package named\n\
             \n\
             fun use(holder: Holder, pair: Pair): Int {\n\
             \x20   holder.q = 6\n\
             \x20   pair.second = pair.first\n\
             \x20   val reference = Holder::p\n\
             \x20   return holder.p + holder.q + holder.declared + holder.hidden + holder.f() +\n\
             \x20       Single.name.length + reference.get(holder) + top\n\
             }\n",
        ),
    ]);
}

/// A constructor property whose annotation list mixes a bound `@get:JvmName` with an
/// unresolvable annotation: publication folds only the bound one, so the module reports exactly
/// kotlinc's single unresolved reference, once.
#[test]
fn a_constructor_property_with_an_unresolved_annotation_reports_it_once() {
    let result = common::compiler_diagnostics(
        &[
            (
                "Pair.kt",
                "package named\n\
                 \n\
                 class Pair(@get:JvmName(\"readFirst\") @Missing val first: Int)\n",
            ),
            (
                "Use.kt",
                "package named\n\
                 \n\
                 fun use(pair: Pair): Int = pair.first\n",
            ),
        ],
        &[],
    );
    common::expect_identical_rejection(&result, "mixed_constructor_property_annotations");
}
