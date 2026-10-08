//! Special bridges a Kotlin class inherits from a Java collection superclass.
//!
//! kotlinc emits them on the first Kotlin class even when that class overrides nothing, and not
//! again on a subclass. Every named class is compared byte-for-byte with kotlinc.
use super::common;

fn run(src: &str) -> String {
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let classpath = [stdlib];
    match common::compile_and_run_box(src, "Main", &classpath, Some(jdk.as_path())) {
        Some(value) => value,
        None => {
            let diagnostics = common::compile_in_process_diagnostics(
                src,
                "Main",
                &classpath,
                Some(jdk.as_path()),
            );
            panic!("fixture did not run: {diagnostics:#?}");
        }
    }
}

const COLLECTIONS: &str = "\
class Bare : java.util.LinkedHashMap<String, Int>()\n\
class H : java.util.HashMap<String, Int>()\n\
class A : java.util.ArrayList<String>()\n\
class S : java.util.HashSet<String>()\n\
class C<T> : java.util.HashMap<T, T>()\n\
open class Parent : java.util.HashMap<String, Int>()\n\
class Kid : Parent()\n\
class Two : java.util.LinkedHashMap<String, Int>() {\n\
    fun extra(): Int = 1\n\
}\n\
fun box(): String {\n\
    val bare = Bare()\n\
    bare[\"a\"] = 1\n\
    val raw = bare as java.util.Map<Any, Any>\n\
    if (bare.size != 1 || bare[\"a\"] != 1 || raw.get(1) != null) return \"map\"\n\
    val list = A()\n\
    list.add(\"x\")\n\
    if (list.size != 1 || list[0] != \"x\") return \"list\"\n\
    val set = S()\n\
    set.add(\"z\")\n\
    if (set.size != 1 || !set.contains(\"z\")) return \"set\"\n\
    return \"OK\"\n\
}\n";

#[test]
fn a_java_collection_subclass_declares_the_inherited_special_bridges() {
    assert_eq!(run(COLLECTIONS), "OK");
    common::assert_classes_identical_to_kotlinc_jdk(
        "InheritedCollectionBridges",
        COLLECTIONS,
        &["Bare", "H", "A", "S", "C", "Parent", "Kid", "Two"],
    );
}

const EXISTING_OVERRIDE_BRIDGES: &str = "\
val seed = ArrayList<String>()\n\
interface Counted {\n\
    val size: Int\n\
}\n\
interface GenericCount<T> {\n\
    val size: T\n\
}\n\
class ConcreteCount : ArrayList<String>(seed), Counted\n\
class GenericCountImpl : ArrayList<String>(seed), GenericCount<Int>\n\
fun box(): String {\n\
    seed.add(\"x\")\n\
    if (ConcreteCount().size != 1) return \"concrete\"\n\
    if ((ConcreteCount() as Counted).size != 1) return \"concrete-interface\"\n\
    if (GenericCountImpl().size != 1) return \"generic\"\n\
    if ((GenericCountImpl() as GenericCount<Int>).size != 1) return \"generic-interface\"\n\
    return \"OK\"\n\
}\n";

#[test]
fn an_existing_override_bridge_is_not_replaced_by_an_inherited_pair() {
    assert_eq!(run(EXISTING_OVERRIDE_BRIDGES), "OK");
    common::assert_classes_identical_to_kotlinc_jdk(
        "InheritedCollectionExistingBridge",
        EXISTING_OVERRIDE_BRIDGES,
        &["ConcreteCount", "GenericCountImpl"],
    );
}
