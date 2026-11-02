// An `internal` member of another module is invisible at the call site, so an applicable
// extension takes the operator call, the indexed read and the call, as for a private member.
// MODULE: lib
package lib

class B {
    internal operator fun plus(x: Int): String = "fail: member plus"
    internal operator fun get(index: Int): String = "fail: member get"
    internal fun tag(): String = "fail: member tag"
}

fun b(): B = B()

// MODULE: main(lib)
import lib.B
import lib.b

operator fun B.plus(x: Int): String = "O"
operator fun B.get(index: Int): String = "K"
fun B.tag(): String = ""

fun box(): String = (b() + 1) + b()[0] + b().tag()
