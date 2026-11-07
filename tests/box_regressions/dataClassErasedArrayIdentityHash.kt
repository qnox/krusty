data class ErasedArrayHolder(val value: Any) {
    override fun toString(): String = "ErasedArrayHolder"
}

fun box(): String {
    val array = arrayOf("before", "second")
    val holder = ErasedArrayHolder(array)
    val hash = holder.hashCode()
    if (holder != ErasedArrayHolder(array)) return "shared array equality"
    if (ErasedArrayHolder(array).hashCode() != hash) return "shared array hash"
    array[0] = "after"
    if (array[0] != "after") return "array mutation"
    if (holder.hashCode() != hash) return "hash changed with contents"
    if (holder != ErasedArrayHolder(array)) return "identity changed with contents"
    if (holder == ErasedArrayHolder(arrayOf("after", "second"))) return "content equality"
    return "OK"
}
