// MODULE: lib
fun prefix() = "O"
fun answer(head: String = prefix(), tail: String = head + "!") = head + tail
fun String.twice(times: Int = length, separator: String = "-") =
    if (times == 2) this + separator + this else this
// MODULE: main(lib)
fun box(): String {
    if (answer() != "OO!") return "fail: answer()"
    if (answer(tail = "K") != "OK") return "fail: answer(tail)"
    if (answer("K") != "KK!") return "fail: answer(head)"
    if ("ab".twice() != "ab-ab") return "fail: twice()"
    if ("ab".twice(separator = "+") != "ab+ab") return "fail: twice(separator)"
    if ("abc".twice() != "abc") return "fail: twice(3)"
    return "OK"
}
