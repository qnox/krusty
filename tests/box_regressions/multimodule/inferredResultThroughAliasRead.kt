// MODULE: lib
package lib

typealias Labels = List<String>

val labels: Labels = listOf("a")

fun labelsOf(): Labels = labels

class Shelf {
    val stored: Labels = listOf("b")
    var current: Labels = listOf("c")

    fun stocked(): Labels = stored
}

object Catalog {
    val entries: Labels = listOf("d")
}

val Shelf.extra: Labels get() = stored

// MODULE: main(lib)
import lib.*

typealias Names = List<String>

val names: Names = listOf("e")

fun namesOf(): Names = names

class Holder {
    val held: Names = listOf("f")

    fun found(): Names = held
}

// Every inferred declaration below reads or calls one spelled through a typealias, so its own
// metadata type carries that abbreviation, as kotlinc writes it.
class Reads(shelf: Shelf, holder: Holder) {
    val held: Names = listOf("g")
    val libraryValue = labels
    val libraryCall = labelsOf()
    val libraryMember = shelf.stored
    val libraryVariable = shelf.current
    val libraryMemberCall = shelf.stocked()
    val libraryObject = Catalog.entries
    val libraryExtension = shelf.extra
    val ownValue = names
    val ownCall = namesOf()
    val ownMember = holder.held
    val ownMemberCall = holder.found()
    val implicitMember = held

    fun all(): List<String> = libraryValue + libraryCall + libraryMember + libraryVariable +
        libraryMemberCall + libraryObject + libraryExtension + ownValue + ownCall + ownMember +
        ownMemberCall + implicitMember
}

fun inferredFunction() = namesOf()

fun box(): String {
    val reads = Reads(Shelf(), Holder())
    if (reads.all().joinToString("") != "aabcbdbeeffg") return "fail: ${reads.all()}"
    if (inferredFunction() != listOf("e")) return "fail: function"
    return "OK"
}
