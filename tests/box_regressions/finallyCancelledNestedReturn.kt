// A return pending through an outer `finally` keeps its own value when the cleanup starts a nested
// return and a nested `finally` cancels it by breaking out of the cleanup's loop.
fun choose(): Int {
    try {
        return 1
    } finally {
        while (true) {
            try {
                return 2
            } finally {
                break
            }
        }
    }
}
fun box(): String = if (choose() == 1) "OK" else "FAIL"
