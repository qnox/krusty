fun box(): String {
    val sb = StringBuilder("ab😀c")
    val same = sb.reverse()
    if (same !== sb) return "fail identity"
    if (sb.toString() != "c😀ba") return "fail pair: " + sb
    if (sb.length != 5) return "fail length"
    if (StringBuilder().reverse().toString() != "") return "fail empty"
    // A low half followed by a high half becomes a pair once reversed, and stays whole after that.
    val halves = StringBuilder().append('\uDE00').append('\uD83D')
    if (halves.reverse().toString() != "😀") return "fail halves"
    if (halves.reverse().toString() != "\uD83D\uDE00") return "fail halves again"
    return StringBuilder("KO").reverse().toString()
}
