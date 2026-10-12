//! An interface's `$DefaultImpls` holder takes the interface instance first. A generic interface
//! that republishes an inherited member declares its formals and types that receiver `I<T>` in the
//! forwarder's generic signature, as kotlinc does. Holder annotations follow kotlinc's nullability
//! rule for type parameters: `T?` is `@Nullable`, a `T` over a non-null bound is `@NotNull`, and a
//! `T` over a nullable bound carries no annotation.

use super::common;

const SOURCE: &str = "\
package store\n\
\n\
interface Greeter {\n\
    fun greet(name: String): String = name + \"!\"\n\
    fun names(all: List<String>): List<String> = all\n\
}\n\
\n\
interface Host<T> : Greeter\n\
\n\
open class Node {\n\
    val tag = \"OK\"\n\
}\n\
\n\
interface Walker<T : Node> {\n\
    fun T.label(): String = tag\n\
    fun visit(node: T): String = node.label()\n\
}\n\
\n\
interface Source<T> {\n\
    fun next(): T? = null\n\
    fun same(value: T): T = value\n\
}\n\
";

#[test]
fn default_impls_forwarders_match_kotlinc() {
    for class in [
        "store/Host$DefaultImpls",
        "store/Walker$DefaultImpls",
        "store/Source$DefaultImpls",
    ] {
        common::byte_diff_against_kotlinc_cp(
            "DefaultImplsInheritedSignature",
            SOURCE,
            class,
            &[common::stdlib_jar()],
        )
        .expect("reference kotlinc is provisioned")
        .unwrap_or_else(|diff| panic!("{class} differs from kotlinc: {diff}"));
    }
}
