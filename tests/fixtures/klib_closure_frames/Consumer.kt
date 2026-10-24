
var effects = 0
fun captureNumber(): Int { effects = effects * 10 + 1; return 7 }
fun captureText(): String { effects = effects * 10 + 2; return "O" }
fun argument(value: Int): Int { effects = effects * 10 + value; return value }
fun box(): String {
    effects = 0
    val f = make(captureNumber(), captureText())
    if (effects != 12) return "capture effects"
    if (f(argument(3), argument(4)) != "7:O:3:4:9" || effects != 1234) return "first call"
    if (f(argument(5), argument(6)) != "7:O:5:6:9" || effects != 123456) return "second call"
    if (inlineMake(8, "K")(1, 2) != "8:K:1:2:9") return "prepared call"
    if (anonymousMake(10, "A")(2, 3) != "10:A:2:3:9") return "anonymous call"
    return "OK"
}
