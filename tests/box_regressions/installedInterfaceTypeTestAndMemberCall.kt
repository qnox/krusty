// `Map.getValue` of the standard library tests its receiver against a non-public interface and
// calls that interface's member, so a program that reaches it needs the interface installed in the
// unit that lowers the body.

fun box(): String {
    val missing = emptyMap<String, String>()
    try {
        missing.getValue("O")
        return "no exception for a missing key"
    } catch (e: NoSuchElementException) {
    }
    return "OK"
}
