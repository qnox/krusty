fun box(): String {
val filled = Array(2) { 7u }
filled[1] = 9u
val empty: Array<UInt?> = Array(2) { null }
empty[0] = 5u
val literal = arrayOf<UInt?>(1u, null)
var sum = 0u
for (x in literal) sum += x ?: 0u
val ok = filled[0] + filled[1] == 16u && empty[0] == 5u && empty[1] == null && sum == 1u
return if (ok) "OK" else "fail"
}
