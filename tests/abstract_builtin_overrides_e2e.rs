//! An abstract class that leaves a mapped builtin's renamed or type-safe member abstract gets
//! kotlinc's materialization of it: an abstract method under the member's Kotlin JVM signature with
//! the class's substituted types, then the final special bridge Java reaches it through.
//!
//! A renamed member's bridge takes the JVM name (`abstract int getSize()` and `final int size()`,
//! `abstract int toInt()` and `final int intValue()`, `abstract char get(int)` and `final char
//! charAt(int)`), with the member's generic signature (`final Set<String> keySet()`) and, where the
//! declaration's erased descriptor differs, an ordinary bridge there (`Object remove(int)` beside
//! `String removeAt(int)`). A scalar result over that generic return slot is the wrapper
//! (`Integer removeAt(int)` of `MutableList<Int>`): the final `remove(int)` bridge returns the
//! scalar and unboxes the call, and the erased bridge returns the wrapper. A value-class result
//! finalizes the carrier (`final int remove(int)` for `Token`) and boxes only the erased bridge.
//! Its element members are mangled: `contains`, `indexOf`, and `lastIndexOf` take the carrier,
//! and `remove` of an `Int` carrier stays the box. A subclass that implements the method inherits
//! that final bridge and publishes only `Object remove(int)`, calling its own `removeAt`.
//! `MutableCollection.remove` of `Int` is `remove(Integer)`. A
//! type-safe member's bridge tests its argument (`final boolean contains(Object)`), as two methods
//! when the member's result is a narrower reference (`Object
//! get(Object)`, then `Integer get(Object)`). A member whose type parameter erases like the
//! declaration's needs nothing (`containsKey(K)`), a member the class declares or inherits a
//! default for keeps its own bridges, and a subclass of a class that materialized the members
//! inherits them, its own implementations included. A Java superclass's abstract member stays
//! abstract in its specialization (`AbstractList.size()` makes `abstract int getSize()`). Each
//! class is compared byte for byte with kotlinc's.

use super::common;

const COLLECTIONS: &str = "abstract class Bag : Collection<String>
abstract class Ints : Collection<Int>
abstract class Row : MutableList<String>
abstract class Names : MutableSet<String>
abstract class Generic<T> : List<T>
abstract class Sub : Bag()
";

#[test]
fn abstract_collection_members_are_materialized_like_kotlinc() {
    common::assert_classes_identical_to_kotlinc(
        "AbstractCollections",
        COLLECTIONS,
        &["Bag", "Ints", "Row", "Names", "Generic", "Sub"],
    );
}

const MAPS: &str = "abstract class Dict : Map<String, Int>
abstract class Edits : MutableMap<String, Int>
abstract class Maybe : Map<String?, Int?>
abstract class Keyed<K> : Map<K, Int>
";

#[test]
fn abstract_map_members_are_materialized_like_kotlinc() {
    common::assert_classes_identical_to_kotlinc(
        "AbstractMaps",
        MAPS,
        &["Dict", "Edits", "Maybe", "Keyed"],
    );
}

const NUMBERS_AND_TEXT: &str = "abstract class Amount : Number()
abstract class Ranked : Number(), Comparable<Ranked>
abstract class Text : CharSequence
";

#[test]
fn abstract_number_and_char_sequence_members_are_materialized_like_kotlinc() {
    common::assert_classes_identical_to_kotlinc(
        "AbstractNumbers",
        NUMBERS_AND_TEXT,
        &["Amount", "Ranked", "Text"],
    );
}

const PARTLY_IMPLEMENTED: &str = "abstract class Declared : Collection<String> {
    override fun contains(element: String) = false
}
abstract class Sized : Collection<String> {
    override val size: Int get() = 0
}
interface Abstracted : Collection<String> {
    override fun contains(element: String): Boolean
}
abstract class ViaAbstracted : Abstracted
interface Defaulted : Collection<String> {
    override fun contains(element: String): Boolean = false
}
abstract class ViaDefaulted : Defaulted
abstract class Mixed : List<String> {
    override fun get(index: Int): String = \"a\"
    fun own() {}
}
";

#[test]
fn only_the_members_left_abstract_are_materialized_like_kotlinc() {
    common::assert_classes_identical_to_kotlinc(
        "PartlyImplemented",
        PARTLY_IMPLEMENTED,
        &[
            "Declared",
            "Sized",
            "Abstracted",
            "ViaAbstracted",
            "Defaulted",
            "ViaDefaulted",
            "Mixed",
        ],
    );
}

const IMPLEMENTING_SUBCLASSES: &str = "abstract class Bag : Collection<String>
abstract class Two : Bag() {
    override val size: Int get() = 2
    override fun contains(element: String) = element == \"a\"
}
abstract class Dict : Map<String, Int>
abstract class One : Dict() {
    override fun get(key: String): Int? = null
    override val keys: Set<String> get() = emptySet()
}
";

/// A subclass that implements a materialized member inherits the final bridges to it.
#[test]
fn a_subclass_implementing_a_materialized_member_inherits_its_bridges_like_kotlinc() {
    common::assert_classes_identical_to_kotlinc(
        "ImplementingSubclasses",
        IMPLEMENTING_SUBCLASSES,
        &["Bag", "Two", "Dict", "One"],
    );
}

#[test]
fn a_java_superclass_abstract_member_stays_abstract_like_kotlinc() {
    let classes = common::classes_against_kotlinc_module(&[(
        "Sub.kt",
        "abstract class JavaRow : java.util.AbstractList<String>()\n",
    )]);
    let (reference, krusty) = classes.method_blocks("JavaRow");
    for member in ["public abstract int getSize();", "public final int size();"] {
        let block = |blocks: &[(String, String)]| {
            blocks
                .iter()
                .find(|(declaration, _)| declaration == member)
                .map(|(_, body)| body.clone())
                .unwrap_or_else(|| panic!("JavaRow declares no `{member}`"))
        };
        assert_eq!(block(&krusty), block(&reference), "`{member}`");
    }
}

/// Java reaches each implementation through the materialized bridges.
const THROUGH_JAVA: &str = "abstract class Bag : Collection<String>
class Two : Bag() {
    override val size: Int get() = 2
    override fun isEmpty() = false
    override fun contains(element: String) = element == \"a\"
    override fun containsAll(elements: Collection<String>) = elements.all { contains(it) }
    override fun iterator(): Iterator<String> = listOf(\"a\", \"b\").iterator()
}

abstract class Dict : Map<String, Int>
class One : Dict() {
    override val size: Int get() = 1
    override fun isEmpty() = false
    override fun containsKey(key: String) = key == \"k\"
    override fun containsValue(value: Int) = value == 1
    override fun get(key: String): Int? = if (key == \"k\") 1 else null
    override val keys: Set<String> get() = setOf(\"k\")
    override val values: Collection<Int> get() = listOf(1)
    override val entries: Set<Map.Entry<String, Int>> get() = mapOf(\"k\" to 1).entries
}

abstract class Amount : Number()
class Three : Amount() {
    override fun toDouble() = 3.0
    override fun toFloat() = 3f
    override fun toLong() = 3L
    override fun toInt() = 3
    override fun toShort(): Short = 3
    override fun toByte(): Byte = 3
}

abstract class Text : CharSequence
class Ab : Text() {
    override val length: Int get() = 2
    override fun get(index: Int) = \"ab\"[index]
    override fun subSequence(startIndex: Int, endIndex: Int) = \"ab\".subSequence(startIndex, endIndex)
}

fun jvm(receiver: Any, name: String, vararg arguments: Pair<Class<*>, Any?>): Any? =
    receiver.javaClass.getMethod(name, *arguments.map { it.first }.toTypedArray())
        .invoke(receiver, *arguments.map { it.second }.toTypedArray())

fun box(): String {
    val any = Any::class.java
    val int = Int::class.javaPrimitiveType!!
    val outcomes = listOf(
        jvm(Two(), \"size\"),
        jvm(Two(), \"contains\", any to \"a\"),
        jvm(Two(), \"contains\", any to 1),
        jvm(One(), \"size\"),
        jvm(One(), \"get\", any to \"k\"),
        jvm(One(), \"get\", any to 2),
        jvm(One(), \"containsKey\", any to 3),
        jvm(One(), \"containsValue\", any to 1),
        jvm(One(), \"keySet\"),
        jvm(Three(), \"intValue\"),
        jvm(Three(), \"doubleValue\"),
        jvm(Ab(), \"length\"),
        jvm(Ab(), \"charAt\", int to 1),
    ).joinToString()
    val expected = \"2, true, false, 1, 1, null, false, true, [k], 3, 3.0, 2, b\"
    return if (outcomes == expected) \"OK\" else outcomes
}
";

#[test]
fn java_reaches_the_implementations_through_the_materialized_bridges_like_kotlinc() {
    common::expect_box_same_as_kotlinc(THROUGH_JAVA, "abstract_builtin_overrides_through_java");
}

const SCALAR_RETURN_SLOT: &str = "abstract class IntRow : MutableList<Int>
abstract class IntRows : IntRow() {
    override fun removeAt(index: Int): Int = index
}
abstract class IntRowsAgain : IntRows() {
    override fun removeAt(index: Int): Int = index
}
abstract class IntSet : MutableSet<Int>
";

/// `removeAt` of `MutableList<Int>` returns the generic `E`. The materialized method returns
/// `Integer`, the final `remove(int)` bridge unboxes it, and a subclass override implements the
/// abstract method without declaring that final bridge again. `MutableSet<Int>` boxes `remove`
/// to `Integer` as well.
#[test]
fn a_scalar_result_over_a_generic_return_slot_is_boxed_like_kotlinc() {
    common::assert_classes_identical_to_kotlinc(
        "AbstractScalarReturn",
        SCALAR_RETURN_SLOT,
        &["IntRow", "IntRows", "IntRowsAgain", "IntSet"],
    );
}

/// Java's `List.remove(int)` reaches the subclass through the boxed bridge.
#[test]
fn java_reaches_a_boxed_remove_at_through_the_materialized_bridge_like_kotlinc() {
    let program = format!(
        "{SCALAR_RETURN_SLOT}
class Rows : IntRow() {{
    private val items = arrayListOf(1, 2)
    override val size: Int get() = items.size
    override fun isEmpty() = items.isEmpty()
    override fun contains(element: Int) = items.contains(element)
    override fun containsAll(elements: Collection<Int>) = items.containsAll(elements)
    override fun iterator() = items.iterator()
    override fun get(index: Int) = items[index]
    override fun indexOf(element: Int) = items.indexOf(element)
    override fun lastIndexOf(element: Int) = items.lastIndexOf(element)
    override fun listIterator() = items.listIterator()
    override fun listIterator(index: Int) = items.listIterator(index)
    override fun subList(fromIndex: Int, toIndex: Int) = items.subList(fromIndex, toIndex)
    override fun add(element: Int) = items.add(element)
    override fun add(index: Int, element: Int) = items.add(index, element)
    override fun addAll(index: Int, elements: Collection<Int>) = items.addAll(index, elements)
    override fun addAll(elements: Collection<Int>) = items.addAll(elements)
    override fun remove(element: Int) = items.remove(element)
    override fun removeAt(index: Int) = items.removeAt(index)
    override fun removeAll(elements: Collection<Int>) = items.removeAll(elements)
    override fun retainAll(elements: Collection<Int>) = items.retainAll(elements)
    override fun clear() = items.clear()
    override fun set(index: Int, element: Int) = items.set(index, element)
}}

fun box(): String {{
    val remove = java.util.List::class.java.getMethod(\"remove\", Int::class.javaPrimitiveType)
    val outcome = remove.invoke(Rows(), 0)
    return if (outcome == 1) \"OK\" else \"$outcome\"
}}
"
    );
    common::expect_box_same_as_kotlinc(&program, "abstract_builtin_overrides_boxed_remove_at");
}

// PLATFORM-SPECIFIC: complete JVM bridge descriptors, access flags, and class ABI.
#[test]
fn an_adapted_renamed_override_inherits_the_realized_final_bridge_like_kotlinc() {
    common::assert_classes_identical_to_kotlinc(
        "AdaptedInheritedRename",
        include_str!("box_regressions/adaptedInheritedCollectionRename.kt"),
        &[
            "BridgeToken",
            "TokenParent",
            "TokenChild",
            "TokenLeaf",
            "AdaptedInheritedRenameKt",
        ],
    );
}
