//! A read-only iterator's superclass declared in another source file of the same module decides the
//! class's stubs as one in the class's own file does. The subclass file's IR does not declare the
//! superclass and no class file of it exists yet, so its members come from the module's retained
//! declaration records: a stub the superclass already realizes is not repeated, a member it
//! declares abstract leaves its subclass to implement it, and a generic member it implements, in
//! the class's substitution context, needs no stub.

use super::common;

const SUPERCLASSES: &str = "package split

open class Base : Iterator<String> {
    override fun hasNext() = false
    override fun next() = \"\"
}

abstract class DeclaresRemove : Iterator<String> {
    abstract fun remove()
}

open class Editor<T>(val seed: T) {
    var last: T = seed
    open fun set(e: T) { last = e }
    open fun add(e: T) { last = e }
    open fun remove() {}
}
";

const SUBCLASSES: &str = "package split

class Derived : Base()

class ImplementsRemove : DeclaresRemove() {
    var removed = 0
    override fun hasNext() = false
    override fun next() = \"\"
    override fun remove() { removed++ }
}

class Walk : Editor<String>(\"\"), ListIterator<String> {
    override fun hasNext() = false
    override fun next() = \"\"
    override fun hasPrevious() = false
    override fun previous() = \"\"
    override fun nextIndex() = 0
    override fun previousIndex() = 0
}
";

#[test]
fn a_superclass_in_another_source_file_decides_read_only_stubs_like_kotlinc() {
    let classes = common::classes_against_kotlinc_module(&[
        ("Subclasses.kt", SUBCLASSES),
        ("Superclasses.kt", SUPERCLASSES),
    ]);
    assert_eq!(
        classes.reference.keys().collect::<Vec<_>>(),
        [
            "split/Base",
            "split/DeclaresRemove",
            "split/Derived",
            "split/Editor",
            "split/ImplementsRemove",
            "split/Walk",
        ]
    );
    assert_eq!(classes.differences(), Vec::<String>::new());
}

/// A Kotlin cast to the mutable face fails on a read-only class, so the members are reached the way
/// a Java caller reaches them, through the JVM methods: `Derived` reaches `Base`'s stub,
/// `ImplementsRemove` its own `remove`, and `Walk` the members `Editor<String>` implements.
#[test]
fn a_superclass_in_another_source_file_keeps_read_only_stub_behavior_like_kotlinc() {
    let program = "package split

fun call(receiver: Any, name: String, vararg arguments: Any): String {
    val parameters = Array<Class<*>>(arguments.size) { Any::class.java }
    return try {
        receiver.javaClass.getMethod(name, *parameters).invoke(receiver, *arguments)
        \"accepted\"
    } catch (e: java.lang.reflect.InvocationTargetException) {
        val cause = e.cause
        if (cause is UnsupportedOperationException) cause.message ?: \"none\" else \"$cause\"
    }
}

fun box(): String {
    val rejected = call(Derived(), \"remove\")
    val implementer = ImplementsRemove()
    val implemented = call(implementer, \"remove\")
    val walk = Walk()
    val set = call(walk, \"set\", \"set\")
    val afterSet = walk.last
    val add = call(walk, \"add\", \"add\")
    val removed = call(walk, \"remove\")
    val declared = listOf(Derived::class.java, ImplementsRemove::class.java, Walk::class.java)
        .map { type -> type.declaredMethods.map { it.name }.sorted() }
    return \"$rejected; $implemented ${implementer.removed}; $set $afterSet $add ${walk.last} $removed; $declared\"
}
";
    let sources = [
        ("Subclasses.kt", SUBCLASSES),
        ("Superclasses.kt", SUPERCLASSES),
        ("Box.kt", program),
    ];
    let reference = common::kotlinc_box_files_result(&sources, "split.BoxKt");
    assert_eq!(
        reference,
        "Operation is not supported for read-only collection; accepted 1; \
         accepted set accepted add accepted; \
         [[], [getRemoved, hasNext, next, next, remove, setRemoved], \
         [hasNext, hasPrevious, next, next, nextIndex, previous, previous, previousIndex]]",
        "kotlinc fixture"
    );
    assert_eq!(
        common::compile_and_run_files_with_stdlib(&sources).expect("krusty compiles and runs"),
        reference
    );
}
