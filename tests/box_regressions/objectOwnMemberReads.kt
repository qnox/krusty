object Counter {
    val step: Int get() = 2

    fun twice(): Int = Counter.step + Counter.step

    fun four(): Int = Counter.twice() + this.twice() - Counter.step

    val viaThis: Int = step * 2
}

fun box(): String {
    if (Counter.twice() != 4) return "fail twice: ${Counter.twice()}"
    if (Counter.four() != 6) return "fail four: ${Counter.four()}"
    if (Counter.viaThis != 4) return "fail viaThis: ${Counter.viaThis}"
    return "OK"
}
