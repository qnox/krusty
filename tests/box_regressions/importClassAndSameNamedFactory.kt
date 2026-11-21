// Imports share syntax but not namespaces: a class and a function with one name imported from two
// packages are both visible, and a type position binds the class whichever import comes last.
// FILE: types.kt
package types

class Token(val text: String)

// FILE: factories.kt
package factories

fun Token(): types.Token = types.Token("K")

// FILE: main.kt
import types.Token
import factories.Token

fun describe(token: Token): String = token.text

fun box(): String {
    val first: Token = Token("O")
    return describe(first) + describe(Token())
}
