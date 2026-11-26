// MODULE: lib
package lib

typealias MaybeLabels = List<String>?
typealias MaybeBoxed<T> = List<T>?

val maybeLabels: MaybeLabels = listOf("a")

fun maybeLabelsOf(): MaybeLabels = listOf("b")
fun <T> maybeBoxed(value: T): MaybeBoxed<T> = listOf(value)

class Shelf {
    val maybe: MaybeLabels = null
    val marked: MaybeLabels? = listOf("c")

    fun maybeOf(): MaybeLabels = listOf("d")
}

// MODULE: main(lib)
import lib.*

typealias MaybeNames = List<String>?

val maybeNames: MaybeNames = listOf("e")

fun maybeNamesOf(): MaybeNames = listOf("f")

class Holder {
    val maybe: MaybeNames = listOf("g")

    fun maybeOf(): MaybeNames = null
}

// Each selected declaration's alias expands to a nullable type. kotlinc keeps the alias use
// unmarked unless it was written `?`, and a `!!` result still carries the abbreviation.
class Nullable(shelf: Shelf, holder: Holder) {
    val libraryValue = maybeLabels
    val libraryCall = maybeLabelsOf()
    val libraryGenericCall = maybeBoxed(1)
    val libraryMember = shelf.maybe
    val libraryMarked = shelf.marked
    val libraryMemberCall = shelf.maybeOf()
    val libraryChainedCall = Shelf().maybeOf()
    val libraryAsserted = maybeLabelsOf()!!
    val ownValue = maybeNames
    val ownCall = maybeNamesOf()
    val ownMember = holder.maybe
    val ownMemberCall = holder.maybeOf()
    val ownChainedRead = Holder().maybe
    val ownAsserted = maybeNamesOf()!!

    fun assertedMember() = Holder().maybe!!
}

fun box(): String {
    val values = Nullable(Shelf(), Holder())
    val present = listOfNotNull(
        values.libraryValue,
        values.libraryCall,
        values.libraryMember,
        values.libraryMarked,
        values.libraryMemberCall,
        values.libraryChainedCall,
        values.libraryAsserted,
        values.ownValue,
        values.ownCall,
        values.ownMember,
        values.ownMemberCall,
        values.ownChainedRead,
        values.ownAsserted,
        values.assertedMember(),
    ).flatten()
    if (present.joinToString("") != "abcddbefggfg") return "fail: $present"
    if (values.libraryGenericCall != listOf(1)) return "fail: generic"
    return "OK"
}
