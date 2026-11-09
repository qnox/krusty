class ParseFailure(message: String) : Exception(message)

open class LookupFailure(val key: Int) : IllegalStateException("missing key $key")

class NestedLookupFailure(key: Int, val depth: Int) : LookupFailure(key)

class SilentFailure : RuntimeException()

class CodedFailure : IllegalArgumentException {
    val code: Int

    constructor(code: Int) : super("code $code") {
        this.code = code
    }

    constructor() : super() {
        code = -1
    }
}

class RootFailure(message: String?) : Throwable(message)

fun parse(text: String): Int {
    if (text == "") throw ParseFailure("empty input")
    return text.length
}

fun lookup(key: Int, depth: Int): Int {
    if (depth > 0) throw NestedLookupFailure(key, depth)
    throw LookupFailure(key)
}

fun box(): String {
    try {
        parse("")
        return "parse did not throw"
    } catch (failure: ParseFailure) {
        if (failure.message != "empty input") return "lost parse message"
    }

    val caught = try {
        lookup(7, 2)
    } catch (failure: LookupFailure) {
        if (failure !is NestedLookupFailure) return "lost nested subtype"
        if (failure.depth != 2 || failure.key != 7) return "lost subclass fields"
        if (failure.message != "missing key 7") return "lost inherited message"
        1
    }
    if (caught != 1) return "nested failure was not caught"

    try {
        lookup(3, 0)
    } catch (failure: NestedLookupFailure) {
        return "caught a superclass instance as its subclass"
    } catch (failure: IllegalStateException) {
        if (failure !is LookupFailure || failure.key != 3) return "lost runtime parent catch"
    }

    try {
        throw SilentFailure()
    } catch (failure: ParseFailure) {
        return "caught a sibling subclass"
    } catch (failure: RuntimeException) {
        if (failure !is SilentFailure) return "lost silent subtype"
        if (failure.message != null) return "invented a message"
    }

    val coded = CodedFailure(42)
    if (coded.code != 42 || coded.message != "code 42") return "lost secondary delegation"
    val uncoded = CodedFailure()
    if (uncoded.code != -1 || uncoded.message != null) return "lost no-argument delegation"

    val thrown: Throwable = RootFailure("root")
    if (thrown is Exception) return "a Throwable subclass became an Exception"
    if (thrown !is RootFailure || thrown.message != "root") return "lost Throwable subclass"

    val failures: Array<Throwable> = arrayOf(ParseFailure("a"), coded, SilentFailure())
    var exceptions = 0
    for (failure in failures) {
        if (failure is Exception) exceptions++
        if (failure is IllegalArgumentException && failure !is CodedFailure) return "wrong sibling"
    }
    if (exceptions != 3) return "lost exception parents"
    return "OK"
}
