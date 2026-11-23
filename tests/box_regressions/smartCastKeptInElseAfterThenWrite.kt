class Node(val next: Node?, var tag: String?)

fun elseKeepsProof(start: Node?, rewrite: Boolean): String {
    var n = start
    if (n != null) {
        if (rewrite) {
            n = n.next
        } else {
            n.tag = "seen"
        }
    }
    return if (n == null) "null" else n.tag ?: "unset"
}

fun elseKeepsProofAfterNull(start: Node?, clear: Boolean): String {
    var n = start
    if (n != null) {
        if (clear) {
            n = null
        } else {
            return n.tag ?: "unset"
        }
    }
    return "cleared"
}

fun whenArmKeepsProof(start: Node?, rewrite: Boolean): String {
    var n = start
    if (n != null) {
        when {
            rewrite -> n = n.next
            else -> n.tag = "when"
        }
    }
    return if (n == null) "null" else n.tag ?: "unset"
}

fun box(): String {
    val node = Node(null, null)
    if (elseKeepsProof(node, false) != "seen") return "FAIL else write"
    if (elseKeepsProof(Node(node, null), true) != "seen") return "FAIL then write"
    if (elseKeepsProof(Node(null, null), true) != "null") return "FAIL then to null"
    if (elseKeepsProofAfterNull(Node(null, "t"), false) != "t") return "FAIL else read"
    if (elseKeepsProofAfterNull(Node(null, "t"), true) != "cleared") return "FAIL cleared"
    if (whenArmKeepsProof(Node(null, null), false) != "when") return "FAIL when"
    return "OK"
}
