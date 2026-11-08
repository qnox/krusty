fun next(): Int = 2

fun box(): String {
    val task: String

    do {
        if (next() % 2 == 0) {
            task = "OK"
            break
        }
    }
    while (true)

    return task
}
