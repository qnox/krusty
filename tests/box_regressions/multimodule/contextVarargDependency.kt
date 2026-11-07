// LANGUAGE: +ContextParameters
// MODULE: lib
// FILE: Lib.kt
package lib
context(tag: String)
fun tagged(vararg pairs: Pair<String, Int>, tail: Pair<String, Int>): Int =
  pairs.size + tail.second + tag.length
context(tag: String) fun plain(vararg xs: Int): Int = xs.sum() + tag.length
context(tag: String) fun Int.ext(vararg xs: Int, last: Int = 0): Int = this + xs.size + last + tag.length
class Box {
  context(tag: String) fun member(vararg xs: String): Int = xs.size + tag.length
  context(tag: String) fun Int.memberExt(vararg xs: Int): Int = this + xs.size + tag.length
  fun run(): Int = with("tt") { member("a", "b") + 1.memberExt(4, 5) }
}
// MODULE: main(lib)
// FILE: Main.kt
import lib.*
fun box(): String {
  val results = with("t") {
    listOf(
      tagged("a" to 1, "b" to 2, tail = "c" to 3),
      tagged(tail = "z" to 5),
      plain(1, 2, 3),
      plain(),
      plain(*intArrayOf(4, 5)),
      10.ext(1, 2, last = 5),
      Box().member("x"),
    )
  }
  val expected = listOf(6, 6, 7, 1, 10, 18, 2)
  if (results != expected) return "fail $results"
  return if (Box().run() == 9) "OK" else "fail member extension ${Box().run()}"
}
