package krusty

import kotlin.test.Test
import kotlin.test.assertEquals

/**
 * How a krusty compile that did not compile its module is handed to kotlinc: the kind of failure
 * from krusty's exit status and report, and the report lines that say why. The Gradle integration
 * suite drives the handoff itself through a real build.
 */
class KotlincHandoffTest {
    @Test
    fun exitStatusThreeIsAnUnsupportedConstruct() {
        val handoff = kotlincHandoff(
            3,
            "a.kt:1:1: error: krusty's Compose compiler plugin does not support a lambda yet\n",
        )
        assertEquals(KrustyFailure.UNSUPPORTED, handoff.failure)
        assertEquals(listOf("a.kt:1:1: error: krusty's Compose compiler plugin does not support a lambda yet"), handoff.details)
        assertEquals(
            "krusty does not implement everything the module uses\n" +
                "a.kt:1:1: error: krusty's Compose compiler plugin does not support a lambda yet\n",
            handoff.record(),
        )
    }

    @Test
    fun anyOtherCompileErrorIsAReportOfErrors() {
        val handoff = kotlincHandoff(
            1,
            "warning: unrelated\na.kt:2:5: error: unresolved reference 'x'.\n    x\n    ^\nb.kt:3:1: error: cannot destructure this type\n",
        )
        assertEquals(KrustyFailure.ERRORS, handoff.failure)
        assertEquals(
            listOf("a.kt:2:5: error: unresolved reference 'x'.", "b.kt:3:1: error: cannot destructure this type"),
            handoff.details,
        )
    }

    @Test
    fun aPanicOrInternalErrorIsAnInternalErrorWhateverTheStatus() {
        val panic = kotlincHandoff(1, "a.kt:1:1: error: unresolved reference 'x'.\nerror: krusty: internal compiler panic: no slot\n")
        assertEquals(KrustyFailure.INTERNAL_ERROR, panic.failure)
        assertEquals(listOf("error: krusty: internal compiler panic: no slot"), panic.details)

        val internal = kotlincHandoff(3, "a.kt:1:1: error: internal error: module signatures were not finalized\n")
        assertEquals(KrustyFailure.INTERNAL_ERROR, internal.failure)
        assertEquals(listOf("a.kt:1:1: error: internal error: module signatures were not finalized"), internal.details)
    }

    @Test
    fun anExitStatusKrustyNeverChoosesIsAnInternalError() {
        val killed = kotlincHandoff(139, "")
        assertEquals(KrustyFailure.INTERNAL_ERROR, killed.failure)
        assertEquals(listOf("exit status 139"), killed.details)

        val unexplained = kotlincHandoff(101, "thread 'main' stopped\n")
        assertEquals(KrustyFailure.INTERNAL_ERROR, unexplained.failure)
        assertEquals(listOf("thread 'main' stopped"), unexplained.details)
    }

    @Test
    fun aReportIsKeptToItsFirstTwentyLines() {
        val report = (1..25).joinToString("\n") { "a.kt:$it:1: error: unresolved reference 'x$it'." }
        val handoff = kotlincHandoff(1, report)
        assertEquals((1..20).map { "a.kt:$it:1: error: unresolved reference 'x$it'." }, handoff.details)
    }
}
