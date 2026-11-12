// A class extending a runtime exception is laid out on the runtime exception's struct: caught by
// its runtime parents and by itself, with the message and cause its delegation stores, the
// class's own name in `Throwable.toString`, and an identity hash.
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

fun show(value: Any?): String = value.toString()

class Report(val failure: Throwable) {
    override fun hashCode(): Int = failure.hashCode()
}

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

    // Throwable.toString: the class's name, then the message when there is one.
    if (show(ParseFailure("bad")) != "ParseFailure: bad") return "lost Throwable.toString"
    if ("$uncoded" != "CodedFailure") return "lost Throwable.toString without a message"
    if (show(NestedLookupFailure(5, 1)) != "NestedLookupFailure: missing key 5") {
        return "lost an inherited Throwable.toString"
    }
    if (show(thrown) != "RootFailure: root") return "lost Throwable.toString of a Throwable root"

    // Any.hashCode: an exception object's identity hash, stable across calls.
    val silent = SilentFailure()
    if (Report(silent).hashCode() != silent.hashCode()) return "lost an exception's hash"
    if (silent.hashCode() != silent.hashCode()) return "unstable exception hash"
    val runtime: Throwable = IllegalStateException("runtime")
    if (Report(runtime).hashCode() != runtime.hashCode()) return "lost a runtime exception's hash"
    return "OK"
}
