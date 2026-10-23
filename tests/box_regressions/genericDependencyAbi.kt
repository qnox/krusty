// MODULE: lib
var identityCalls = 0

fun <T> dependencyIdentity(value: T): T {
    identityCalls++
    return value
}

fun callsToIdentity(): Int = identityCalls

fun <T> T.dependencyEcho(value: T): T {
    identityCalls++
    return value
}

// Expanding the dependency's serialized inline body keeps its ordinary generic extension call.
inline fun <T> serializedEcho(receiver: T, value: T): T = receiver.dependencyEcho(value)

// MODULE: main(lib)
var argumentCalls = 0

fun intArgument(): Int {
    argumentCalls++
    return 42
}

fun stringArgument(): String {
    argumentCalls++
    return "OK"
}

fun longArgument(): Long {
    argumentCalls++
    return 0x123456789abcdefL
}

fun box(): String {
    if (dependencyIdentity(intArgument()) != 42) return "Int result"
    if (dependencyIdentity(stringArgument()) != "OK") return "String result"
    if (dependencyIdentity(longArgument()) != 0x123456789abcdefL) return "Long result"
    if (dependencyIdentity(-7) != -7) return "second Int result"
    if (dependencyIdentity("second") != "second") return "second String result"
    if (intArgument().dependencyEcho(intArgument()) != 42) return "Int extension"
    if (stringArgument().dependencyEcho(stringArgument()) != "OK") return "String extension"
    if (longArgument().dependencyEcho(longArgument()) != 0x123456789abcdefL) return "Long extension"
    if (serializedEcho(1, 2) != 2) return "serialized Int extension"
    if (serializedEcho("receiver", "value") != "value") return "serialized String extension"
    if (serializedEcho(1L, 0x123456789abcdefL) != 0x123456789abcdefL) return "serialized Long extension"
    if (argumentCalls != 9) return "argument effects"
    if (callsToIdentity() != 11) return "callee effects"
    return "OK"
}
