// MODULE: friend
package shared
internal fun answer() = "OK"
// MODULE: sibling
package shared
internal fun unrelated() = "not a friend"
// MODULE: main(friend, sibling)(friend)
import shared.answer
fun box() = answer()
