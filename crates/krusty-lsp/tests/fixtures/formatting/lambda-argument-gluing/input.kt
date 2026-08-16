fun f() {
    consume({ a: Int ->
        a + 1
    })
    consume(1, { a: Int ->
        a + 1
    })
    consume({ a: Int ->
        a + 1
    }, 1)
}
