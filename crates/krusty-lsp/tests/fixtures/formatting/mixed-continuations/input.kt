val total = 1 +
    2 +
    3
val label = "prefix" + """
line1
""".trimIndent()
val nested = foo(1 +
    2)
val single = consume(object {
    fun value() = 1
})
val withLambda = consume({ a: Int ->
    a + 1
})
