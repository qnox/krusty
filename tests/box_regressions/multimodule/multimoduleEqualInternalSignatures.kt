// MODULE: friend
package shared
internal fun answer() = "OK"
// MODULE: sibling
package shared
internal fun answer() = "wrong module"
// MODULE: main(friend, sibling)(friend)
import shared.answer
fun box() = answer()
