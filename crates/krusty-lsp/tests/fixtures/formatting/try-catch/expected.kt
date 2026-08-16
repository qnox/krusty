val i =
    try {
        1
    } catch (e: Exception) {
        2
    }

fun f() {
    try {
        work()
    } finally {
        done()
    }
}
