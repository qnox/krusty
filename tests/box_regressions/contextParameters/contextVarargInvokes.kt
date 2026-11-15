// LANGUAGE: +ContextParameters
// FILE: Main.kt
class Runner {
  context(tag: String)
  operator fun Int.invoke(vararg xs: Int): Int = this + tag.length + xs.size
  fun run(): Int = with("t") { val n = 1; n(2, 3) }
}
fun local(): Int {
  context(tag: String)
  fun inner(vararg xs: Int, last: Int = 5): Int = tag.length + xs.sum() + last
  return with("ab") { inner(1, 2) + inner(last = 1) }
}
fun box(): String {
  if (Runner().run() != 4) return "fail invoke ${Runner().run()}"
  return if (local() == 10 + 3) "OK" else "fail local ${local()}"
}
