// WITH_STDLIB
// `Char.isSurrogate()` is the stdlib body `this in MIN_SURROGATE..MAX_SURROGATE`: a range
// literal's membership in a library body is a comparison with its bounds.
fun box(): String {
    if ('a'.isSurrogate()) return "fail1"
    if (!'\uD800'.isSurrogate()) return "fail2"
    if (!'\uDFFF'.isSurrogate()) return "fail3"
    if (''.isSurrogate()) return "fail4"
    return "OK"
}
