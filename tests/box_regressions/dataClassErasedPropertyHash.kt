class Keyed(val key: Int) {
override fun hashCode() = key
override fun equals(other: Any?) = other is Keyed && other.key == key
}
data class Holder(val value: Any) { override fun toString() = "Holder" }
fun box(): String {
if (Holder(Keyed(7)).hashCode() != 7) return "Keyed"
if (Holder("a").hashCode() != 97) return "String"
if (Holder(5).hashCode() != 5) return "Int"
return "OK"
}