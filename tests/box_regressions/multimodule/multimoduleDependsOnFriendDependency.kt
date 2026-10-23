// MODULE: friend
package shared
internal fun answer() = "OK"
// MODULE: common(friend)(friend)
package common
import shared.answer
fun commonAnswer() = answer()
// MODULE: main()()(common)
import common.commonAnswer
fun box() = commonAnswer()
