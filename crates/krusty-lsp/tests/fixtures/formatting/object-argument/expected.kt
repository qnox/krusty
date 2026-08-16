fun nested() {
    consume(
        object {
            fun value() = 1
        },
    )
    consume(
        1,
        object {
            fun value() = 2
        },
    )
}
