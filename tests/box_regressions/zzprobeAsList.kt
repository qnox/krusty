fun box(): String {
    val list = arrayOf("a", "b", "c").asList()
    if (list.size != 3) return "size"
    if (list.indexOf("c") != 2) return "indexOf"
    return if (list.contains("b")) "OK" else "bad"
}
