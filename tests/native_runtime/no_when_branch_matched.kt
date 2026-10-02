// Kotlin's answers for `no_when_branch_matched.c`: the exception an unmatched exhaustive `when`
// raises.
fun box(): String {
    val e = NoWhenBranchMatchedException()
    return "${e::class.qualifiedName} ${e.message} ${(e as Any) is RuntimeException} $e\n"
}
