//! A Kotlin classifier mapped onto a JDK class (`kotlin.collections.Map` over `java.util.Map`) has
//! no static scope of its own: the JDK class's statics are named only through the Java classifier,
//! by an explicit import or a qualified name. kotlinc's `qualifierForStaticCase` diagnostics case
//! pins the star-import side: two star-imported `Map`s are ambiguous at their rung, so the root
//! falls to the next rung, `kotlin.collections.Map`, which has no `of`.

use super::common;

const SOME_MAP: &str = "\
package some

class Map {
    companion object {
        fun of() = 42
    }
}
";

const OTHER_MAP: &str = "\
package other

class Map
";

#[test]
fn ambiguous_star_imported_classifiers_leave_the_mapped_kotlin_classifier_without_statics() {
    let sources = [
        ("some/Map.kt", SOME_MAP),
        ("other/Map.kt", OTHER_MAP),
        (
            "test.kt",
            "import java.util.*\nimport some.*\n\nfun test() = Map.of()\n",
        ),
        (
            "test2.kt",
            "import java.util.*\nimport other.*\n\nfun test2() = Map.of()\n",
        ),
    ];
    common::assert_error_blocks_match_kotlinc(&sources, &[]);
}

#[test]
fn a_mapped_kotlin_classifier_does_not_expose_the_statics_of_its_jdk_realization() {
    let sources = [(
        "test.kt",
        "fun mapOf() = Map.of<String, String>()\nfun listOf() = List.of<String>()\n",
    )];
    common::assert_error_blocks_match_kotlinc(&sources, &[]);
}

#[test]
fn the_java_classifier_keeps_its_statics_by_explicit_import_or_qualified_name() {
    let sources = [
        ("some/Map.kt", SOME_MAP),
        ("test3.kt", "import java.util.Map\n\nfun test3() = Map.of<String, String>()\n"),
        ("test4.kt", "import some.Map\n\nfun test4() = Map.of()\n"),
        (
            "test5.kt",
            "fun test5() = java.util.Map.of<String, String>()\nfun test6() = java.util.List.of<String>()\n",
        ),
    ];
    common::assert_sources_accepted_like_kotlinc(&sources, &[]);
}
