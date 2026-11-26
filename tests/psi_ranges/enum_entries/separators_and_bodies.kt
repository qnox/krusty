enum class E(val v: Int) {
    /** doc */
    A(1),
    @Deprecated("x") B(2) { },
    // c
    C(3), `in`,
    D;
    fun f() = 1
}
