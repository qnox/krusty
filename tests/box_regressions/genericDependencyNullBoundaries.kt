// MODULE: lib
fun <T> dependencyFirst(value: T, unused: String?): T = value

fun <T> dependencyWithOptional(value: T, optional: T?): T = value

fun <T> dependencyRequire(value: T?): T = value!!

fun <T> dependencyHiddenNull(): T {
    val value: T? = null
    return value!!
}

fun <T> dependencyKeepNull(value: T?): T? = value

// MODULE: main(lib)
fun box(): String {
    if (dependencyFirst(42, null) != 42) return "unrelated nullable parameter"
    if (dependencyFirst(4294967297L, "unused") != 4294967297L) return "Long parameter"
    if (dependencyWithOptional(42, null) != 42) return "shared nullable Int formal"
    if (dependencyWithOptional(4294967297L, null) != 4294967297L) return "shared nullable Long formal"
    try {
        dependencyRequire<Int>(null)
        return "missing Int exception"
    } catch (error: Throwable) {
        if (error !is NullPointerException) return "wrong Int exception"
    }
    try {
        dependencyRequire<Long>(null)
        return "missing Long exception"
    } catch (error: Throwable) {
        if (error !is NullPointerException) return "wrong Long exception"
    }
    try {
        dependencyHiddenNull<Int>()
        return "missing hidden Int exception"
    } catch (error: Throwable) {
        if (error !is NullPointerException) return "wrong hidden Int exception"
    }
    try {
        dependencyHiddenNull<Long>()
        return "missing hidden Long exception"
    } catch (error: Throwable) {
        if (error !is NullPointerException) return "wrong hidden Long exception"
    }
    if (dependencyKeepNull<String>(null) != null) return "null reference"
    if (dependencyKeepNull<String>("OK") != "OK") return "nonnull reference"
    return "OK"
}
