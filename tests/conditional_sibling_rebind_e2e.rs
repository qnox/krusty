//! An under-constrained call takes type arguments from a sibling whose classifier is a supertype
//! of the call's result.
//!
//! `hashSetOf<T>()` beside `linkedSetOf()` is the shape in
//! `core/util.runtime`'s `Collection<T>.closure`: the empty `linkedSetOf()` has no argument to fix
//! `T`, and `LinkedHashSet<T>` is a `HashSet<T>`. The same reading applies to any result that is a
//! subtype of its sibling (`arrayListOf()` beside `mutableListOf<String>()`), in either branch,
//! including a block, a `when` arm, and an elvis fallback. A formal that an argument already fixed
//! stays fixed.

use super::common;

const SOURCE: &str = "\
open class Bag<T>\n\
class LinkedBag<T> : Bag<T>()\n\
fun <T> bag(): Bag<T> = Bag()\n\
fun <T> linkedBag(): LinkedBag<T> = LinkedBag()\n\
fun <T> bagOf(value: T): Bag<T> = Bag()\n\
fun <T> closure(flag: Boolean, value: T): Boolean {\n\
    val toAdd = if (flag) linkedSetOf() else hashSetOf<T>()\n\
    toAdd.add(value)\n\
    return toAdd.contains(value)\n\
}\n\
fun <T> closureReversed(flag: Boolean, value: T): Boolean {\n\
    val toAdd = if (flag) hashSetOf<T>() else linkedSetOf()\n\
    toAdd.add(value)\n\
    return toAdd.contains(value)\n\
}\n\
fun <T> fromArgument(flag: Boolean, value: T): Boolean {\n\
    val toAdd = if (flag) linkedSetOf(value) else hashSetOf()\n\
    return toAdd.contains(value)\n\
}\n\
fun <T> explicitSibling(flag: Boolean, value: T): T {\n\
    val toAdd = if (flag) linkedSetOf<T>() else hashSetOf()\n\
    toAdd.add(value)\n\
    return toAdd.first()\n\
}\n\
fun lists(flag: Boolean): Boolean {\n\
    val items = if (flag) mutableListOf<String>() else arrayListOf()\n\
    items.add(\"a\")\n\
    return items[0] == \"a\"\n\
}\n\
fun blocked(flag: Boolean): Boolean {\n\
    val toAdd = if (flag) { linkedSetOf() } else { hashSetOf(\"a\") }\n\
    toAdd.add(\"b\")\n\
    return toAdd.contains(\"a\") || toAdd.contains(\"b\")\n\
}\n\
fun whenSet(flag: Boolean): Boolean {\n\
    val toAdd = when {\n\
        flag -> linkedSetOf()\n\
        else -> hashSetOf(\"a\")\n\
    }\n\
    toAdd.add(\"b\")\n\
    return toAdd.contains(\"b\")\n\
}\n\
fun elvisSet(): Boolean {\n\
    val toAdd = linkedSetOf() ?: hashSetOf(\"a\")\n\
    toAdd.add(\"b\")\n\
    return toAdd.contains(\"a\") || toAdd.contains(\"b\")\n\
}\n\
fun <T : CharSequence> bounded(flag: Boolean, value: T): Int {\n\
    val toAdd = if (flag) linkedSetOf() else hashSetOf<T>()\n\
    toAdd.add(value)\n\
    return toAdd.first().length\n\
}\n\
fun <T> ownHierarchy(flag: Boolean): Bag<T> {\n\
    val value = if (flag) linkedBag() else bag<T>()\n\
    return value\n\
}\n\
fun box(): String {\n\
    if (!closure(false, \"z\")) return \"closure\"\n\
    if (!closure(true, \"z\")) return \"closure-then\"\n\
    if (!closureReversed(true, \"z\")) return \"reversed\"\n\
    if (!fromArgument(true, \"z\")) return \"argument\"\n\
    if (explicitSibling(false, \"z\") != \"z\") return \"explicit\"\n\
    if (!lists(false)) return \"lists\"\n\
    if (!lists(true)) return \"lists-then\"\n\
    if (!blocked(false)) return \"block\"\n\
    if (!whenSet(false)) return \"when\"\n\
    if (!elvisSet()) return \"elvis\"\n\
    if (bounded(false, \"abcd\") != 4) return \"bounded\"\n\
    val bag = ownHierarchy<String>(true)\n\
    if (bag !is LinkedBag<String>) return \"hierarchy\"\n\
    return \"OK\"\n\
}\n\
";

#[test]
fn a_more_specific_call_rebinds_from_its_sibling() {
    common::expect_box_same_as_kotlinc(SOURCE, "conditional-sibling-rebind");
}

#[test]
fn an_argument_constraint_survives_a_sibling_branch() {
    const CLASH: &str = "\
        open class Bag<T>\n\
        class LinkedBag<T> : Bag<T>()\n\
        fun <T> bagOf(value: T): Bag<T> = Bag()\n\
        fun <T> linkedBag(): LinkedBag<T> = LinkedBag()\n\
        fun clash(flag: Boolean): Bag<String> =\n\
            if (flag) bagOf(1) else linkedBag<String>()\n\
        ";
    let result = common::compiler_diagnostics(
        &[("Clash.kt", CLASH)],
        &[common::stdlib_jar(), common::jdk_modules()],
    );
    let reference = vec![common::CompilerError {
        file: "Clash.kt".to_string(),
        line: 6,
        column: 1,
        message: "return type mismatch: expected 'Bag<String>', actual 'Bag<out Comparable<*> & \
                  Serializable>'."
            .to_string(),
    }];
    let krusty = vec![common::CompilerError {
        file: "Clash.kt".to_string(),
        line: 6,
        column: 1,
        message: "return type mismatch: expected 'Bag<String>', actual 'Bag<out Any>'.".to_string(),
    }];
    assert_eq!(
        common::compiler_errors(&result.reference_stderr),
        reference,
        "kotlinc"
    );
    assert_eq!(
        common::compiler_errors(&result.krusty_stderr),
        krusty,
        "krusty"
    );
    assert_eq!(result.reference_code, 1);
    assert_eq!(result.krusty_code, 1);
}
