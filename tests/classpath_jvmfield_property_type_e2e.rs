//! A `@JvmField` property of a dependency class is a Kotlin declaration: its type is the one its
//! `@Metadata` declares, type arguments and nullability included, not the field's erased classifier.
//! A top-level one has no accessor at all; its package-facade field realizes it.
use super::common;

const LIB: &str = "package dep\n\
    \n\
    class Element(\n\
    \x20   @JvmField val attributes: Map<String, String>,\n\
    \x20   @JvmField val content: String?,\n\
    )\n\
    class Box<T>(@JvmField val value: T)\n\
    class Counter { @JvmField var counts: MutableList<Int> = mutableListOf() }\n";

const MAIN: &str = "import dep.*\n\
    \n\
    fun box(): String {\n\
    \x20   val element = Element(mapOf(\"class\" to \"C\"), null)\n\
    \x20   if (element.attributes[\"class\"].isNullOrEmpty()) return \"attributes\"\n\
    \x20   if (element.attributes.get(\"id\").toBoolean()) return \"get\"\n\
    \x20   if ((element.content ?: \"none\") != \"none\") return \"content\"\n\
    \x20   if (Box(\"abc\").value.length != 3) return \"box\"\n\
    \x20   val counter = Counter()\n\
    \x20   counter.counts.add(4)\n\
    \x20   counter.counts = mutableListOf(counter.counts.sum() + 1)\n\
    \x20   return if (counter.counts.single() == 5) \"OK\" else \"counts\"\n\
    }\n";

#[test]
fn a_jvmfield_property_keeps_its_declared_type() {
    if let Some(output) = common::expect_box_run_against_kotlinc(LIB, MAIN) {
        assert_eq!(output, "OK");
    }
}

#[test]
fn a_nullable_jvmfield_property_stays_nullable() {
    let Some(library) = common::kotlinc_library(LIB) else {
        return;
    };
    const SOURCE: &str = "import dep.Element\n\
        \n\
        fun bad(element: Element): String {\n\
        \x20   val content: String = element.content\n\
        \x20   return content\n\
        }\n";
    let result = common::compiler_diagnostics(
        &[("Main.kt", SOURCE)],
        &[library, common::stdlib_jar(), common::jdk_modules()],
    );
    let expected = [common::CompilerError {
        file: "Main.kt".to_string(),
        line: 4,
        column: 25,
        message: "initializer type mismatch: expected 'String', actual 'String?'.".to_string(),
    }];
    let mut krusty = common::compiler_errors(&result.krusty_stdout);
    krusty.extend(common::compiler_errors(&result.krusty_stderr));
    assert_eq!(result.reference_code, 1, "kotlinc unexpectedly accepted");
    assert_eq!(result.krusty_code, 1, "krusty unexpectedly accepted");
    assert_eq!(common::compiler_errors(&result.reference_stderr), expected);
    assert_eq!(krusty, expected);
}

const TOP_LEVEL_LIB: &str = "package dep\n\
    \n\
    @JvmField val names: List<String> = listOf(\"a\")\n\
    @JvmField var count: Int = 1\n";

const TOP_LEVEL_MAIN: &str = "import dep.*\n\
    \n\
    fun box(): String {\n\
    \x20   count = count + names.size\n\
    \x20   return if (count == 2 && names.single() == \"a\") \"OK\" else \"fail\"\n\
    }\n";

#[test]
fn a_top_level_jvmfield_property_is_read_and_written() {
    if let Some(output) = common::expect_box_run_against_kotlinc(TOP_LEVEL_LIB, TOP_LEVEL_MAIN) {
        assert_eq!(output, "OK");
    }
    if let Some(result) = common::class_bytes_diff_against_kotlinc(
        "TopLevelJvmField",
        &[("Lib.kt", TOP_LEVEL_LIB)],
        TOP_LEVEL_MAIN,
        "TopLevelJvmFieldKt",
        "box",
    ) {
        result.unwrap_or_else(|difference| panic!("{difference}"));
    }
}
