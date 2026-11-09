// A `StringBuilder` appended to through `Appendable`, from a file declaring no `Appendable` of its
// own: the call goes through the interface member, and the builder is one the target may make
// itself rather than a class of the program. The stdlib's `joinTo` appends to its buffer this way.

fun put(a: Appendable): Appendable = a.append('y').append("zz")

fun <A : Appendable> bounded(buffer: A): A {
    buffer.append(null)
    return buffer
}

fun box(): String {
    val sb = StringBuilder("x")
    if (put(sb) !== sb) return "fail answer"
    if (bounded(sb).toString() != "xyzznull") return "fail text: $sb"
    return "OK"
}
