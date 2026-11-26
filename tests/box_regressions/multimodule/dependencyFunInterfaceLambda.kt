// MODULE: lib
// A lambda converted to a dependency's `fun interface` answers calls through the interface from
// the dependency's code and from the converting module's alike, also through an interface the
// dependency's one extends, and is an instance of both.
package lib

interface Named { fun name(id: Int): String }
fun interface Namer : Named

fun describe(namer: Named, id: Int): String = namer.name(id)

// MODULE: main(lib)
import lib.*

fun box(): String {
    val suffix = "K"
    val namer = Namer { id -> if (id == 1) "O" else suffix }
    if (describe(namer, 1) != "O") return "dependency call"
    if (namer.name(2) != "K") return "module call"
    val named: Any = namer
    if (named !is Namer || named !is Named) return "membership"
    return describe(namer, 1) + namer.name(2)
}
