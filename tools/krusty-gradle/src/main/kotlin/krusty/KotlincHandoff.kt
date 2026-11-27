package krusty

/**
 * Why krusty did not compile a module it was asked to compile. The build never fails on it: the
 * Kotlin task compiles the module with kotlinc instead, and the build log keeps the reason, so a
 * report still tells what krusty does not implement yet from what krusty got wrong.
 */
internal enum class KrustyFailure(val description: String) {
    /** krusty reports a construct of valid source it does not implement yet: guidance for work. */
    UNSUPPORTED("krusty does not implement everything the module uses"),

    /** krusty crashed, ended abnormally or reported an internal error: a krusty bug. */
    INTERNAL_ERROR("krusty failed with an internal error"),

    /**
     * krusty reported compile errors. kotlinc then decides: if it compiles the module, krusty
     * reported an error kotlinc does not, which is a krusty bug; if not, the source is invalid and
     * kotlinc fails the build with its own errors.
     */
    ERRORS("krusty reported errors"),
}

/** A module krusty hands to kotlinc: the failure's kind and the lines of its report that say why. */
internal class KotlincHandoff(val failure: KrustyFailure, val details: List<String>) {
    /** The handoff record the Kotlin task reads: never empty, so it always reruns on kotlinc. */
    fun record(): String = (listOf(failure.description) + details).joinToString("\n", postfix = "\n")
}

/** At most this many report lines are kept for one handoff; the rest are in the build log. */
private const val MAX_DETAILS = 20

/**
 * Classifies a krusty compile that exited with [exitStatus] and wrote [errorOutput]. Status 3 is
 * krusty's own report that it does not implement everything the source uses. A panic, an internal
 * error or an exit status krusty never chooses is a crash; any other failure is a report of errors.
 */
internal fun kotlincHandoff(exitStatus: Int, errorOutput: String): KotlincHandoff {
    val lines = errorOutput.lines().map(String::trimEnd).filter(String::isNotBlank)
    val internal = lines.filter(::isInternalError)
    val failure = when {
        internal.isNotEmpty() || exitStatus !in KRUSTY_FAILURE_STATUSES -> KrustyFailure.INTERNAL_ERROR
        exitStatus == UNSUPPORTED_EXIT_STATUS -> KrustyFailure.UNSUPPORTED
        else -> KrustyFailure.ERRORS
    }
    val errors = lines.filter { ": error: " in it || it.startsWith("error: ") }
    val details = when {
        internal.isNotEmpty() -> internal
        errors.isNotEmpty() -> errors
        else -> lines.takeLast(MAX_DETAILS).ifEmpty { listOf("exit status $exitStatus") }
    }
    return KotlincHandoff(failure, details.take(MAX_DETAILS))
}

/** A handoff for a krusty that could not be run, or whose classpath fingerprint failed. */
internal fun internalErrorHandoff(reason: String): KotlincHandoff =
    KotlincHandoff(KrustyFailure.INTERNAL_ERROR, reason.lines().filter(String::isNotBlank).take(MAX_DETAILS))

private fun isInternalError(line: String): Boolean =
    "panicked at" in line || "internal compiler panic" in line || "internal error:" in line

/**
 * krusty's exit status for a compile that failed only on constructs it does not implement yet.
 * 1 is any other compile error, 2 a command line krusty rejects.
 */
internal const val UNSUPPORTED_EXIT_STATUS = 3

private val KRUSTY_FAILURE_STATUSES = setOf(1, 2, UNSUPPORTED_EXIT_STATUS)
