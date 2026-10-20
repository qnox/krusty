//! kotlinc's `CollectionStubMethodLowering` for the iterator faces. A class that implements the
//! read-only `Iterator` or `ListIterator` without its mutable counterpart realizes every candidate
//! member of `MutableIterator`/`MutableListIterator` that nothing in the class effectively
//! overrides as a public stub throwing `UnsupportedOperationException("Operation is not supported
//! for read-only collection")`, after its declared members and before its bridges. A stub whose
//! descriptor differs from the mutable declaration's erasure gets a throwing bridge as well. An
//! interface gets no stub, a subclass of a class that already has one (in this file or a dependency)
//! does not repeat it, a member inherited from a superclass or an interface default counts as an
//! override, and a member declared abstract by an abstract superclass suppresses its stub there.
use super::common;

const SOURCE: &str = "package store\n\
                      \n\
                      class Counter(val n: Int) : Iterator<Int> {\n\
                      \x20   var i = 0\n\
                      \x20   override fun hasNext() = i < n\n\
                      \x20   override fun next() = i++\n\
                      }\n\
                      \n\
                      class Gen<T>(val t: T) : Iterator<T> {\n\
                      \x20   override fun hasNext() = false\n\
                      \x20   override fun next() = t\n\
                      }\n\
                      \n\
                      class Strings : ListIterator<String> {\n\
                      \x20   override fun hasNext() = false\n\
                      \x20   override fun next(): String = \"\"\n\
                      \x20   override fun hasPrevious() = false\n\
                      \x20   override fun previous(): String = \"\"\n\
                      \x20   override fun nextIndex() = 0\n\
                      \x20   override fun previousIndex() = 0\n\
                      }\n\
                      \n\
                      class Ints : ListIterator<Int> {\n\
                      \x20   override fun hasNext() = false\n\
                      \x20   override fun next() = 1\n\
                      \x20   override fun hasPrevious() = false\n\
                      \x20   override fun previous() = 1\n\
                      \x20   override fun nextIndex() = 0\n\
                      \x20   override fun previousIndex() = 0\n\
                      }\n\
                      \n\
                      abstract class AbstractIt<T> : Iterator<T>\n\
                      \n\
                      abstract class AbstractListIt<T> : ListIterator<T>\n\
                      \n\
                      abstract class PartialListIt : ListIterator<String> {\n\
                      \x20   override fun next() = \"\"\n\
                      }\n\
                      \n\
                      open class Base : Iterator<String> {\n\
                      \x20   override fun hasNext() = false\n\
                      \x20   override fun next() = \"\"\n\
                      }\n\
                      \n\
                      class Derived : Base()\n\
                      \n\
                      abstract class DeclaresRemove : Iterator<String> {\n\
                      \x20   abstract fun remove()\n\
                      }\n\
                      \n\
                      class ImplementsRemove : DeclaresRemove() {\n\
                      \x20   override fun hasNext() = false\n\
                      \x20   override fun next() = \"\"\n\
                      \x20   override fun remove() {}\n\
                      }\n\
                      \n\
                      interface Source : Iterator<String>\n\
                      \n\
                      class FromSource : Source {\n\
                      \x20   override fun hasNext() = false\n\
                      \x20   override fun next() = \"\"\n\
                      }\n\
                      \n\
                      class Mutable : MutableIterator<String> {\n\
                      \x20   override fun hasNext() = false\n\
                      \x20   override fun next() = \"\"\n\
                      \x20   override fun remove() {}\n\
                      }\n\
                      \n\
                      open class Cursor {\n\
                      \x20   open fun hasNext() = false\n\
                      \x20   open fun next(): Any = \"\"\n\
                      }\n\
                      \n\
                      abstract class Walk : Cursor(), ListIterator<Any>\n\
                      \n\
                      interface Removable {\n\
                      \x20   fun remove() {}\n\
                      }\n\
                      \n\
                      class Quiet : Iterator<String>, Removable {\n\
                      \x20   override fun hasNext() = false\n\
                      \x20   override fun next() = \"\"\n\
                      }\n\
                      \n\
                      class Letters : AbstractIterator<Char>() {\n\
                      \x20   override fun computeNext() = done()\n\
                      }\n\
                      \n\
                      fun longs(): Iterator<Long> = object : Iterator<Long> {\n\
                      \x20   override fun hasNext() = false\n\
                      \x20   override fun next() = 1L\n\
                      }\n";

#[test]
fn read_only_iterator_stubs_are_byte_identical_to_kotlinc() {
    for class in [
        "store/Counter",
        "store/Gen",
        "store/Strings",
        "store/Ints",
        "store/AbstractIt",
        "store/AbstractListIt",
        "store/PartialListIt",
        "store/Base",
        "store/Derived",
        "store/DeclaresRemove",
        "store/ImplementsRemove",
        "store/Source",
        "store/FromSource",
        "store/Mutable",
        "store/Walk",
        "store/Quiet",
        "store/Letters",
        "store/ReadOnlyIteratorStubsKt$longs$1",
    ] {
        common::byte_diff_against_kotlinc_cp(
            "ReadOnlyIteratorStubs",
            SOURCE,
            class,
            &[common::stdlib_jar()],
        )
        .expect("reference kotlinc is provisioned")
        .unwrap_or_else(|diff| panic!("{class} differs from kotlinc: {diff}"));
    }
}

/// A Kotlin cast to the mutable face fails on a read-only class, so the stubs are reached the way
/// a Java caller reaches them: through the JVM method itself.
#[test]
fn read_only_iterator_stubs_throw_when_called_through_the_jvm_interface() {
    let source = "class Counter : Iterator<Int> {\n\
                  \x20   override fun hasNext() = false\n\
                  \x20   override fun next() = 1\n\
                  }\n\
                  \n\
                  class Ints : ListIterator<Int> {\n\
                  \x20   override fun hasNext() = false\n\
                  \x20   override fun next() = 1\n\
                  \x20   override fun hasPrevious() = false\n\
                  \x20   override fun previous() = 1\n\
                  \x20   override fun nextIndex() = 0\n\
                  \x20   override fun previousIndex() = 0\n\
                  }\n\
                  \n\
                  fun rejected(receiver: Any, name: String, vararg parameters: Class<*>): String {\n\
                  \x20   val arguments = Array<Any?>(parameters.size) { if (parameters[it].isPrimitive) 0 else null }\n\
                  \x20   return try {\n\
                  \x20       receiver.javaClass.getMethod(name, *parameters).invoke(receiver, *arguments)\n\
                  \x20       \"accepted\"\n\
                  \x20   } catch (e: java.lang.reflect.InvocationTargetException) {\n\
                  \x20       val cause = e.cause\n\
                  \x20       if (cause is UnsupportedOperationException) cause.message ?: \"none\" else \"$cause\"\n\
                  \x20   }\n\
                  }\n\
                  \n\
                  fun box(): String {\n\
                  \x20   val expected = \"Operation is not supported for read-only collection\"\n\
                  \x20   if (rejected(Counter(), \"remove\") != expected) return \"remove\"\n\
                  \x20   val ints = Ints()\n\
                  \x20   val int = Integer.TYPE\n\
                  \x20   val any = Any::class.java\n\
                  \x20   if (rejected(ints, \"set\", int) != expected) return \"set(int)\"\n\
                  \x20   if (rejected(ints, \"set\", any) != expected) return \"set(Object)\"\n\
                  \x20   if (rejected(ints, \"add\", any) != expected) return \"add(Object)\"\n\
                  \x20   if (rejected(ints, \"remove\") != expected) return \"ListIterator.remove\"\n\
                  \x20   val anonymous: Iterator<Long> = object : Iterator<Long> {\n\
                  \x20       override fun hasNext() = false\n\
                  \x20       override fun next() = 1L\n\
                  \x20   }\n\
                  \x20   if (rejected(anonymous, \"remove\") != expected) return \"anonymous\"\n\
                  \x20   return \"OK\"\n\
                  }\n";
    assert_eq!(
        common::compile_and_run_with_stdlib(source, "ReadOnlyIteratorStubsRun").as_deref(),
        Some("OK")
    );
}
