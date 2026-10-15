//! A collection member's type-checked bridge under its erased root signature is kotlinc's
//! `BRIDGE_SPECIAL`: final and not synthetic. A subclass inherits that bridge instead of declaring
//! it again. An interface owns the same guarded body without the impossible final flag.
use super::common;

const SOURCE: &str = "\
open class Entity
class Issue : Entity()

open class Tracked<T : Entity> : HashSet<T>() {
    override fun remove(element: T): Boolean = super.remove(element)
    override fun contains(element: T): Boolean = super.contains(element)
}

class Issues : Tracked<Issue>() {
    override fun remove(element: Issue): Boolean = super.remove(element)
}

open class Lookup<V> : HashMap<String, V>() {
    override fun get(key: String): V? = super.get(key)
    override fun remove(key: String): V? = super.remove(key)
}

interface StringCollection : Collection<String> {
    override fun contains(element: String): Boolean
}
";

fn assert_identical(class: &str) {
    common::byte_diff_against_kotlinc_cp("SpecialBridges", SOURCE, class, &[common::stdlib_jar()])
        .expect("the reference kotlinc is provisioned")
        .unwrap_or_else(|why| panic!("{why}"));
}

#[test]
fn a_type_checked_collection_bridge_is_final_and_not_synthetic() {
    assert_identical("Tracked");
}

#[test]
fn a_null_result_collection_bridge_is_final_and_not_synthetic() {
    assert_identical("Lookup");
}

const GENERIC_INHERITED: &str = "\
class GenericLookup<V> : HashMap<String, V>()
";

#[test]
fn an_inherited_get_or_default_keeps_the_class_type_parameter() {
    common::byte_diff_against_kotlinc_cp(
        "GenericInheritedSpecialBridge",
        GENERIC_INHERITED,
        "GenericLookup",
        &[common::stdlib_jar()],
    )
    .expect("the reference kotlinc is provisioned")
    .unwrap_or_else(|why| panic!("{why}"));
}

#[test]
fn a_subclass_inherits_the_final_special_bridge() {
    assert_identical("Issues");
}

#[test]
fn an_interface_collection_bridge_stays_non_final() {
    assert_identical("StringCollection");
}

const DEPENDENCY: &str = "\
package lib

open class Entity

open class Tracked<T : Entity> : HashSet<T>() {
    override fun remove(element: T): Boolean = super.remove(element)
    override fun contains(element: T): Boolean = super.contains(element)
}
";

const DEPENDENT: &str = "\
import lib.*

class Issue : Entity()

class Issues : Tracked<Issue>() {
    override fun remove(element: Issue): Boolean = super.remove(element)
    override fun contains(element: Issue): Boolean = super.contains(element)
}

fun box(): String {
    val issues = Issues()
    val issue = Issue()
    issues.add(issue)
    val all: MutableSet<Any?> = issues as MutableSet<Any?>
    if (!all.contains(issue)) return \"contains\"
    if (all.contains(\"text\")) return \"barrier\"
    if (!all.remove(issue)) return \"remove\"
    return \"OK\"
}
";

#[test]
fn a_subclass_inherits_a_dependencys_final_special_bridge() {
    let output = common::expect_box_run_against_kotlinc(DEPENDENCY, DEPENDENT)
        .expect("the reference kotlinc is provisioned");
    assert_eq!(output, "OK");
}

const INTERFACE_PROPERTY: &str = "\
interface SizedStrings : Collection<String> {\n\
    override val size: Int get() = 56\n\
}\n\
class SizedStringsImpl : SizedStrings {\n\
    override fun isEmpty() = throw UnsupportedOperationException()\n\
    override fun contains(element: String) = throw UnsupportedOperationException()\n\
    override fun iterator() = throw UnsupportedOperationException()\n\
    override fun containsAll(elements: Collection<String>) = throw UnsupportedOperationException()\n\
}\n\
fun box(): String {\n\
    val strings: Collection<String> = SizedStringsImpl()\n\
    return if (strings.size == 56) \"OK\" else \"fail: ${strings.size}\"\n\
}\n";

#[test]
fn an_interface_special_property_bridge_is_non_final_and_runs() {
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    assert_eq!(
        common::expect_box_run(
            INTERFACE_PROPERTY,
            "SpecialInterfacePropertyBridge",
            &[stdlib],
            Some(jdk.as_path()),
        ),
        "OK"
    );
    common::byte_diff_against_kotlinc_cp(
        "SpecialInterfacePropertyBridge",
        INTERFACE_PROPERTY,
        "SizedStrings",
        &[common::stdlib_jar()],
    )
    .expect("the reference kotlinc is provisioned")
    .unwrap_or_else(|why| panic!("{why}"));
}
