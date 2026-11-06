// A value of a type parameter whose upper bound admits null (`V : S?`) is proven `V & Any` by a
// null check, so it then infers and passes as the non-null `S` it is bounded by.

class Slot<S : Any>(val name: String)

class Keyed<S : Any>(val key: S, val slot: Slot<S>)

fun <S : Any> keyed(key: S, slot: Slot<S>): Keyed<S> = Keyed(key, slot)

fun <S : Any, V : S?> inferredConstructor(value: V, slot: Slot<S>): Keyed<S>? =
    if (value != null) Keyed(value, slot) else null

fun <S : Any, V : S?> explicitConstructor(value: V, slot: Slot<S>): Keyed<S>? =
    if (value != null) Keyed<S>(value, slot) else null

fun <S : Any, V : S?> inferredFunction(value: V, slot: Slot<S>): Keyed<S>? {
    if (value == null) return null
    return keyed(value, slot)
}

fun <S : Any, V : S?> declaredLocal(value: V): S? {
    if (value == null) return null
    val proven: S = value
    return proven
}

fun box(): String {
    val slot = Slot<String>("s")
    if (inferredConstructor<String, String?>("a", slot)?.key != "a") return "inferred constructor"
    if (inferredConstructor<String, String?>(null, slot) != null) return "inferred constructor null"
    if (explicitConstructor<String, String?>("b", slot)?.key != "b") return "explicit constructor"
    if (inferredFunction<String, String?>("c", slot)?.slot?.name != "s") return "inferred function"
    if (inferredFunction<String, String?>(null, slot) != null) return "inferred function null"
    if (declaredLocal<String, String?>("d") != "d") return "declared local"
    return "OK"
}
