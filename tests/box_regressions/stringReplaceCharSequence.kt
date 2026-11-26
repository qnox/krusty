// WITH_STDLIB

fun box(): String {
    val text = "a1b1"
    val replaced = text.replace("1", "-")
    if (replaced != "a-b-") return "FAIL: $replaced"
    val cut = "xyxy".replace("y", "")
    if (cut != "xx") return "FAIL cut: $cut"
    return "OK"
}
