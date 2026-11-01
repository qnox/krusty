//! An override of a dependency's member whose signature mentions a value class implements it under
//! the member's own mangled JVM name, exactly as an override of a same-module member does: the
//! dependency publishes that name (`hash-<hash>`), the override mangles to the same name, and no
//! bridge stands beside it. Covered: a value-class result, a nullable one, a value-class
//! parameter, a primitive-underlying result, an abstract class member, a suspend member, and a
//! member that narrows a generic supertype's result to a value class.

use super::common;

const LIBRARY: &str = "\
package lib\n\
\n\
@JvmInline\n\
value class Hash(val value: String)\n\
\n\
@JvmInline\n\
value class Count(val value: Int)\n\
\n\
interface Hasher {\n\
    fun hash(raw: String): Hash\n\
    fun maybe(raw: String): Hash?\n\
    fun accept(hash: Hash): String\n\
    fun count(): Count\n\
    fun name(): String\n\
}\n\
\n\
abstract class Base {\n\
    abstract fun hash(raw: String): Hash\n\
    open fun count(): Count = Count(0)\n\
}\n\
\n\
interface Resolver {\n\
    suspend fun resolve(slug: Hash): Count?\n\
}\n\
\n\
interface Source<T> {\n\
    fun get(): T\n\
}\n\
\n\
interface HashSource : Source<Hash> {\n\
    override fun get(): Hash\n\
}\n\
";

const CONSUMER: &str = "\
package app\n\
\n\
import lib.Base\n\
import lib.Count\n\
import lib.Hash\n\
import lib.HashSource\n\
import lib.Hasher\n\
import lib.Resolver\n\
\n\
class Impl : Hasher {\n\
    override fun hash(raw: String): Hash = Hash(raw.reversed())\n\
    override fun maybe(raw: String): Hash? = if (raw.isEmpty()) null else Hash(raw)\n\
    override fun accept(hash: Hash): String = hash.value\n\
    override fun count(): Count = Count(1)\n\
    override fun name(): String = \"impl\"\n\
}\n\
\n\
class Derived : Base() {\n\
    override fun hash(raw: String): Hash = Hash(raw)\n\
    override fun count(): Count = Count(2)\n\
}\n\
\n\
class Cached : Resolver {\n\
    override suspend fun resolve(slug: Hash): Count? = Count(slug.value.length)\n\
}\n\
\n\
class Fixed : HashSource {\n\
    override fun get(): Hash = Hash(\"fixed\")\n\
}\n\
\n\
fun box(): String {\n\
    val impl: Hasher = Impl()\n\
    if (impl.hash(\"KO\").value != \"OK\") return \"hash\"\n\
    if (impl.maybe(\"\") != null) return \"maybe\"\n\
    if (impl.accept(Hash(\"x\")) != \"x\") return \"accept\"\n\
    if (impl.count().value != 1) return \"count\"\n\
    val derived: Base = Derived()\n\
    if (derived.hash(\"y\").value != \"y\" || derived.count().value != 2) return \"derived\"\n\
    val source: lib.Source<Hash> = Fixed()\n\
    if (source.get().value != \"fixed\") return \"source\"\n\
    return \"OK\"\n\
}\n\
";

#[test]
fn an_override_of_a_dependency_value_class_member_is_called_through_its_mangled_name() {
    assert_eq!(
        common::expect_box_run_against("DependencyValueClassOverrideLibrary", LIBRARY, CONSUMER)
            .as_deref(),
        Some("OK")
    );
}

#[test]
fn an_override_of_a_dependency_value_class_member_declares_no_bridge() {
    let library = common::compile_libs_build(
        "DependencyValueClassOverrideLibrary",
        &[("Lib.kt", LIBRARY)],
    )
    .expect("krusty builds the dependency");
    let reference = library
        .reference_out()
        .expect("reference kotlinc builds the dependency")
        .to_path_buf();
    common::assert_classes_identical_to_kotlinc_over_own_libraries(
        "DependencyValueClassOverride",
        CONSUMER,
        &[
            "app/Impl",
            "app/Derived",
            "app/Cached",
            "app/Fixed",
            "app/DependencyValueClassOverrideKt",
        ],
        &[reference],
        &[library.krusty_out().to_path_buf()],
    );
}
