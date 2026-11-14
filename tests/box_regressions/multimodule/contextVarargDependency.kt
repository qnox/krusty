// LANGUAGE: +ContextParameters
// MODULE: lib
// FILE: Lib.kt
package lib
context(tag: String)
fun tagged(vararg pairs: Entry<Int>, tail: Entry<Int>): Int =
  pairs.size + tail.value + tag.length
context(tag: String) fun plain(vararg xs: Int): Int = xs.sum() + tag.length
context(tag: String) fun Int.ext(vararg xs: Int, last: Int = 0): Int = this + xs.size + last + tag.length
class Entry<T>(val key: String, val value: T)
fun <T> entry(key: String, value: T): Entry<T> = Entry(key, value)
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
    intArrayOf(
      tagged(entry("a", 1), entry("b", 2), tail = entry("c", 3)),
      tagged(tail = entry("z", 5)),
      plain(1, 2, 3),
      plain(),
      plain(*intArrayOf(4, 5)),
      10.ext(1, 2, last = 5),
      Box().member("x"),
    )
  }
  val expected = intArrayOf(6, 6, 7, 1, 10, 18, 2)
  if (results.size != expected.size) return "fail size ${results.size}"
  var i = 0
  while (i < results.size) {
    if (results[i] != expected[i]) return "fail at $i: ${results[i]}"
    i++
  }
  return if (Box().run() == 9) "OK" else "fail member extension ${Box().run()}"
}
