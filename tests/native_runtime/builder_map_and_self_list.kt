// Kotlin's answers for `builder_map_and_self_list.c`: `map` over a `StringBuilder` its transform
// grows or shrinks, and a list that holds itself.
fun box(): String = buildString {
    val sb = StringBuilder("ab")
    val r = sb.map { c -> if (sb.length < 4) sb.append('z'); c }
    appendLine("$r $sb")
    val sb2 = StringBuilder("abcd")
    val r2 = sb2.map { c -> if (sb2.length > 2) sb2.setLength(sb2.length - 1); c }
    appendLine("$r2 $sb2")
    val l = mutableListOf<Any>(1, 2)
    l.add(l)
    l.add(3)
    appendLine(l)
    val m = mutableListOf<Any>()
    m.add(m)
    appendLine(m)
}
