// WITH_STDLIB

interface ContractMarker
class ContractImplementation : ContractMarker
fun hasContract(value: Any): Boolean = value is ContractMarker
fun allocateRoot(): Any = Any()

fun box(): String {
    val first = Any()
    val same: Any = first
    val second = allocateRoot()
    if (first !== same || first === second) return "identity"
    if (first != same || first == second) return "equals"
    val hash = first.hashCode()
    if (same.hashCode() != hash || first.hashCode() != hash) return "hash"
    val text = first.toString()
    if (text.length == 0 || same.toString() != text) return "rendering"
    if (hasContract(first) || hasContract(second)) return "root interface"
    if (!hasContract(ContractImplementation())) return "source interface"
    return "OK"
}
