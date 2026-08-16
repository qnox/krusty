fun g(x: Int) {
    foo(when (x) {
        1 ->
            "one"
        else -> "other"
    })
}
