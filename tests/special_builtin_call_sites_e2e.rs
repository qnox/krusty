//! Call-site spelling for members whose override chain reaches a special builtin with a
//! different JVM name (`Collection.size` → `size()I`, `Map.keys` → `keySet`,
//! `MutableList.removeAt` → `remove`, `CharSequence.get` → `charAt`, `Number.toByte` →
//! `byteValue`).
//!
//! kotlinc consumes that alternate name only for the exact selected dependency declaration. A
//! source override has its own JVM declaration and keeps that declaration's spelling
//! (`SmartSet.getSize`, `MyList.removeAt`), even when its override chain reaches such a builtin.
//! For dependency declarations the owner and opcode stay the selected call's own
//! (`MethodSignatureMapper.mapOverriddenSpecialBuiltinIfNeeded`,
//! compiler/ir/backend.jvm/.../mapping/MethodSignatureMapper.kt:411-422, backed by
//! `getOverriddenBuiltinWithDifferentJvmName`,
//! core/descriptors.jvm/.../specialBuiltinMembers.kt:88-102). Super calls and source writes keep
//! the declaration spelling as well.
use super::common;
use super::common::{
    assert_class_code_matches_kotlinc_jdk, assert_class_code_matches_kotlinc_jdk_cp,
};

fn run_boxed(src: &str) {
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let output =
        common::compile_and_run_box(src, "Main", &[stdlib, jdk.clone()], Some(jdk.as_path()));
    assert_eq!(output.as_deref(), Some("OK"), "fixture runs");
}

/// A user `AbstractSet` subclass overriding `var size`: reads and writes call its own
/// `getSize`/`setSize` declarations. The inherited dependency declaration remains `size()I` when
/// the receiver is typed as `AbstractSet`.
const SMART_SET: &str = r#"
class SmartSet<T>(private val data: MutableList<T>) : AbstractSet<T>() {
    override var size: Int
        get() = data.size
        set(value) { if (value > data.size) data.add(data.last()) }
    override fun iterator(): Iterator<T> = data.iterator()
}
fun readSize(s: SmartSet<String>): Int = s.size
fun writeSize(s: SmartSet<String>, value: Int) { s.size = value }
fun readAbstract(a: AbstractSet<String>): Int = a.size
fun main() {
    val s = SmartSet(mutableListOf("a", "b"))
    if (readSize(s) != 2) throw AssertionError("read")
    writeSize(s, 3)
    if (readSize(s) != 3) throw AssertionError("write")
    if (readAbstract(s) != 3) throw AssertionError("abstract")
}
"#;

#[test]
fn special_set_size_call_sites_match_kotlinc() {
    // The caller facade: `s.size` reads spell `SmartSet.getSize:()I`, the write spells
    // `SmartSet.setSize:(I)V`, and the abstract-typed read uses its exact dependency realization.
    assert_class_code_matches_kotlinc_jdk("SpecialSetSize", SMART_SET, "SpecialSetSizeKt");
    // The declaration keeps `getSize`/`setSize` and inherits the final `size()` bridge.
    assert_class_code_matches_kotlinc_jdk("SpecialSetSize", SMART_SET, "SmartSet");
}

#[test]
fn special_set_size_call_sites_run() {
    run_boxed(&format!("{SMART_SET}\nfun box(): String = \"OK\"\n"));
}

const MY_MAP: &str = r#"
class MyMap<K, V>(private val data: Map<K, V>) : AbstractMap<K, V>() {
    override val entries: Set<Map.Entry<K, V>> get() = data.entries
    override val keys: Set<K> get() = data.keys
    override val values: Collection<V> get() = data.values
}
fun readKeys(m: MyMap<String, Int>): Set<String> = m.keys
fun readValues(m: MyMap<String, Int>): Collection<Int> = m.values
fun readEntries(m: MyMap<String, Int>): Set<Map.Entry<String, Int>> = m.entries
fun main() {
    val m = MyMap(mapOf("a" to 1))
    if (readKeys(m) != setOf("a")) throw AssertionError("keys")
    if (readValues(m).toList() != listOf(1)) throw AssertionError("values")
    if (readEntries(m).size != 1) throw AssertionError("entries")
}
"#;

#[test]
fn special_map_property_call_sites_match_kotlinc() {
    // These reads call the source override accessors. Their inherited dependency declarations
    // have separate provider-owned physical realizations.
    assert_class_code_matches_kotlinc_jdk("SpecialMapProps", MY_MAP, "SpecialMapPropsKt");
    assert_class_code_matches_kotlinc_jdk("SpecialMapProps", MY_MAP, "MyMap");
}

#[test]
fn special_map_property_call_sites_run() {
    run_boxed(&format!("{MY_MAP}\nfun box(): String = \"OK\"\n"));
}

/// A covariant source `keys` override keeps its declared `HashSet` result and accessor spelling.
const COV_MAP: &str = r#"
class CovMap<K, V>(private val data: Map<K, V>) : AbstractMap<K, V>() {
    override val entries: Set<Map.Entry<K, V>> get() = data.entries
    override val keys: HashSet<K> get() = HashSet(data.keys)
}
fun readKeys(m: CovMap<String, Int>): HashSet<String> = m.keys
fun main() {
    if (readKeys(CovMap(mapOf("a" to 1))) != hashSetOf("a")) throw AssertionError("keys")
}
"#;

#[test]
fn special_covariant_property_call_site_match_kotlinc() {
    assert_class_code_matches_kotlinc_jdk(
        "SpecialCovariantKeys",
        COV_MAP,
        "SpecialCovariantKeysKt",
    );
}

const SPECIAL_FUNCTIONS: &str = r#"
class MyList<T>(private val data: MutableList<T>) : AbstractMutableList<T>() {
    override val size: Int get() = data.size
    override fun get(index: Int): T = data[index]
    override fun removeAt(index: Int): T = data.removeAt(index)
    override fun set(index: Int, element: T): T = element
    override fun add(index: Int, element: T) {}
}
class Cs(val s: String) : CharSequence {
    override val length: Int get() = s.length
    override fun get(index: Int): Char = s[index]
    override fun subSequence(startIndex: Int, endIndex: Int): CharSequence =
        s.subSequence(startIndex, endIndex)
}
class N(private val v: Int) : Number() {
    override fun toByte(): Byte = v.toByte()
    override fun toDouble(): Double = v.toDouble()
    override fun toFloat(): Float = v.toFloat()
    override fun toInt(): Int = v
    override fun toLong(): Long = v.toLong()
    override fun toShort(): Short = v.toShort()
}
enum class E { A, B }
fun callRemoveAt(ml: MyList<String>): String = ml.removeAt(0)
fun callCharAt(cs: Cs): Char = cs[0]
fun readLength(cs: Cs): Int = cs.length
fun callToByte(n: N): Byte = n.toByte()
fun callToLong(n: N): Long = n.toLong()
fun readName(e: E): String = e.name
fun readOrdinal(e: E): Int = e.ordinal
fun main() {
    val ml = MyList(mutableListOf("a", "b"))
    if (callRemoveAt(ml) != "a") throw AssertionError("removeAt")
    if (callCharAt(Cs("xy")) != 'x') throw AssertionError("charAt")
    if (readLength(Cs("xy")) != 2) throw AssertionError("length")
    if (callToByte(N(300)) != 44.toByte()) throw AssertionError("toByte")
    if (callToLong(N(7)) != 7L) throw AssertionError("toLong")
    if (readName(E.B) != "B") throw AssertionError("name")
    if (readOrdinal(E.B) != 1) throw AssertionError("ordinal")
}
"#;

#[test]
fn special_function_call_sites_match_kotlinc() {
    // Each call names the source override (`removeAt`, `get`, `getLength`, `toByte`/`toLong`).
    // The inherited builtins' alternate JVM spellings do not replace source declaration ABI.
    assert_class_code_matches_kotlinc_jdk(
        "SpecialFunctions",
        SPECIAL_FUNCTIONS,
        "SpecialFunctionsKt",
    );
}

#[test]
fn special_function_call_sites_run() {
    run_boxed(&format!(
        "{SPECIAL_FUNCTIONS}\nfun box(): String = \"OK\"\n"
    ));
}

/// A non-generic subclass whose `removeAt` override has a concrete return keeps that declaration's
/// own descriptor; it is not widened back to the inherited builtin's erased result.
const NARROW_REMOVE_AT: &str = r#"
class S(private val data: MutableList<String>) : AbstractMutableList<String>() {
    override val size: Int get() = data.size
    override fun get(index: Int): String = data[index]
    override fun removeAt(index: Int): String = data.removeAt(index)
    override fun set(index: Int, element: String): String = element
    override fun add(index: Int, element: String) {}
}
fun callRemoveAt(s: S): String = s.removeAt(0)
fun main() {
    if (callRemoveAt(S(mutableListOf("a"))) != "a") throw AssertionError("removeAt")
}
"#;

#[test]
fn special_narrowed_override_call_site_match_kotlinc() {
    assert_class_code_matches_kotlinc_jdk(
        "SpecialNarrowRemoveAt",
        NARROW_REMOVE_AT,
        "SpecialNarrowRemoveAtKt",
    );
}

/// Super calls and ordinary calls to source overrides keep the declaration spelling. A property
/// whose override chain reaches no special builtin (`Plain.size`) does as well.
const SUPER_AND_NEGATIVE: &str = r#"
open class Base : CharSequence {
    override val length: Int get() = 3
    override fun get(index: Int): Char = 'x'
    override fun subSequence(startIndex: Int, endIndex: Int): CharSequence = this
}
class Sub : Base() {
    override val length: Int get() = super.length + 1
    override fun get(index: Int): Char = if (index < 0) super.get(0) else 'y'
}
class Plain(val size: Int)
fun readLength(s: Sub): Int = s.length
fun readPlain(p: Plain): Int = p.size
fun main() {
    val s = Sub()
    if (readLength(s) != 4) throw AssertionError("super.length")
    if (s[-1] != 'x') throw AssertionError("super.get")
    if (readPlain(Plain(9)) != 9) throw AssertionError("plain")
}
"#;

#[test]
fn special_builtin_super_calls_keep_declaration_spelling() {
    assert_class_code_matches_kotlinc_jdk("SpecialSuper", SUPER_AND_NEGATIVE, "Sub");
    assert_class_code_matches_kotlinc_jdk("SpecialSuper", SUPER_AND_NEGATIVE, "SpecialSuperKt");
}

#[test]
fn special_builtin_super_calls_run() {
    run_boxed(&format!(
        "{SUPER_AND_NEGATIVE}\nfun box(): String = \"OK\"\n"
    ));
}

/// Classpath owners: the retarget does not need a user class in the chain — a member declared
/// by a Kotlin stdlib class (`AbstractCollection.size`, `AbstractMap.keys`, `ArrayDeque`) has
/// its own override chain into the builtin, and a user interface inherits it too.
const CLASSPATH_OWNERS: &str = r#"
interface MyListI : MutableList<String>
interface MyColl : Collection<String>
fun readSize(a: AbstractCollection<String>): Int = a.size
fun readKeys(m: AbstractMap<String, Int>): Set<String> = m.keys
fun readValues(m: AbstractMap<String, Int>): Collection<Int> = m.values
fun readEntries(m: AbstractMap<String, Int>): Set<Map.Entry<String, Int>> = m.entries
fun callRemoveAt(i: MyListI): String = i.removeAt(0)
fun readCollSize(c: MyColl): Int = c.size
fun dequeRemoveAt(ad: ArrayDeque<String>): String = ad.removeAt(0)
fun dequeSize(ad: ArrayDeque<String>): Int = ad.size
class ML : MyListI, ArrayList<String>()
class AC : AbstractCollection<String>() {
    override val size: Int get() = 2
    override fun iterator(): Iterator<String> = listOf("a", "b").iterator()
}
class AM : AbstractMap<String, Int>() {
    override val entries: Set<Map.Entry<String, Int>> get() = mapOf("a" to 1).entries
}
class Coll : MyColl {
    override val size: Int get() = 5
    override fun isEmpty(): Boolean = false
    override fun iterator(): Iterator<String> = throw UnsupportedOperationException()
    override fun containsAll(elements: Collection<String>): Boolean = false
    override fun contains(element: String): Boolean = false
}
fun main() {
    if (readSize(AC()) != 2) throw AssertionError("size")
    val m = AM()
    if (readKeys(m) != setOf("a")) throw AssertionError("keys")
    if (readValues(m).toList() != listOf(1)) throw AssertionError("values")
    if (readEntries(m).size != 1) throw AssertionError("entries")
    val ml = ML()
    ml.add("z")
    if (callRemoveAt(ml) != "z") throw AssertionError("removeAt")
    if (readCollSize(Coll()) != 5) throw AssertionError("coll size")
    val ad = ArrayDeque(listOf("q"))
    if (dequeRemoveAt(ad) != "q") throw AssertionError("deque removeAt")
    if (dequeSize(ad) != 0) throw AssertionError("deque size")
}
"#;

#[test]
fn special_builtin_classpath_owner_call_sites_match_kotlinc() {
    assert_class_code_matches_kotlinc_jdk(
        "SpecialClasspathOwners",
        CLASSPATH_OWNERS,
        "SpecialClasspathOwnersKt",
    );
}

#[test]
fn special_builtin_classpath_owner_call_sites_run() {
    run_boxed(&format!("{CLASSPATH_OWNERS}\nfun box(): String = \"OK\"\n"));
}

/// kotlinc's pre-filter matches only the SOURCE name (`getOverriddenBuiltinWithDifferentJvmName`,
/// core/descriptors.jvm/.../specialBuiltinMembers.kt:88-102): a getter spelling denotes a PLAIN
/// FUNCTION, which is never retargeted — so `NegColl.getSize:()Ljava/lang/String;` keeps its
/// declared spelling. A property row also requires an empty parameter list
/// (`hasBuiltinSpecialPropertyFqNameImpl`'s `valueParameters.isEmpty()`), so the same-named
/// `NegMap.entries:(I)I` is no accessor and keeps its spelling too. The genuine `size` read in the
/// same class still retargets.
const NEGATIVE_FUNCTIONS: &str = r#"
class NegColl : AbstractCollection<String>() {
    override val size: Int get() = 2
    override fun iterator(): Iterator<String> = listOf("a", "b").iterator()
    fun getSize(): String = "plain"
}
class NegMap : AbstractMap<String, Int>() {
    override val entries: Set<Map.Entry<String, Int>> get() = mapOf("a" to 1).entries
    fun entries(x: Int): Int = x + 1
}
fun callGetSize(c: NegColl): String = c.getSize()
fun callEntries(m: NegMap): Int = m.entries(3)
fun readSize(c: NegColl): Int = c.size
fun main() {
    if (callGetSize(NegColl()) != "plain") throw AssertionError("getSize")
    if (callEntries(NegMap()) != 4) throw AssertionError("entries")
    if (readSize(NegColl()) != 2) throw AssertionError("size")
}
"#;

#[test]
fn special_plain_functions_keep_declaration_spelling() {
    assert_class_code_matches_kotlinc_jdk(
        "SpecialNegativeFunctions",
        NEGATIVE_FUNCTIONS,
        "SpecialNegativeFunctionsKt",
    );
}

#[test]
fn special_plain_functions_run() {
    run_boxed(&format!(
        "{NEGATIVE_FUNCTIONS}\nfun box(): String = \"OK\"\n"
    ));
}

/// A Java bean getter is a plain function as far as the retarget is concerned: `j.getSize()`
/// keeps `JColl.getSize:()I` while the `size` PROPERTY read on the same receiver retargets to
/// `JColl.size:()I`.
const J_COLL: &str = r#"
package jl;

import java.util.AbstractCollection;
import java.util.Iterator;
import java.util.List;

public class JColl extends AbstractCollection<String> {
    public int getSize() { return 42; }
    @Override public int size() { return 2; }
    @Override public Iterator<String> iterator() { return List.of("a", "b").iterator(); }
}
"#;

const JAVA_GETTER: &str = r#"
import jl.JColl
fun callGetSize(j: JColl): Int = j.getSize()
fun readSize(j: JColl): Int = j.size
fun main() {
    if (callGetSize(JColl()) != 42) throw AssertionError("getSize")
    if (readSize(JColl()) != 2) throw AssertionError("size")
}
"#;

fn jcoll_dir() -> std::path::PathBuf {
    let java = vec![("jl/JColl.java".to_string(), J_COLL.to_string())];
    common::javac_compile(&java, &[])
        .map(|(dir, _)| dir)
        .expect("javac must compile the JColl fixture")
}

#[test]
fn special_java_bean_getter_keeps_declaration_spelling() {
    assert_class_code_matches_kotlinc_jdk_cp(
        "SpecialJavaGetter",
        JAVA_GETTER,
        "SpecialJavaGetterKt",
        std::slice::from_ref(&jcoll_dir()),
    );
}

#[test]
fn special_java_bean_getter_runs() {
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let jcoll = jcoll_dir();
    let output = common::compile_and_run_box(
        &format!("{JAVA_GETTER}\nfun box(): String = \"OK\"\n"),
        "Main",
        &[stdlib, jcoll, jdk.clone()],
        Some(jdk.as_path()),
    );
    assert_eq!(output.as_deref(), Some("OK"), "fixture runs");
}
