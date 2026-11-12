// PLATFORM-SPECIFIC: JVM oracle for the upstream test-generator DSL, not a compiler box fixture.
import java.io.File
import java.util.regex.Pattern
import java.nio.file.Files
import org.jetbrains.kotlin.test.TargetBackend

const val checkoutRoot = __CHECKOUT__
const val selectedTarget = __TARGET__
val selectedFiles = sortedSetOf<String>()
val selectedRoots = sortedSetOf<String>()
typealias AnnotationModel = Any

class TestGroupSuite {
    fun testGroup(
        testsRoot: String,
        testDataRoot: String,
        testRunnerMethodName: String = "runTest",
        init: TestGroup.() -> Unit,
    ) { TestGroup(testDataRoot).init() }
}
class TestGroup(val dataRoot: String) {
    inline fun <reified T> testClass(
        suiteTestClassName: String = "",
        annotations: List<Any> = emptyList(),
        noinline init: TestClass.() -> Unit,
    ) { TestClass(dataRoot, T::class.java.simpleName).init() }
}
class TestClass(val dataRoot: String, val runner: String) {
    val defaultSkipTestAllFilesCheck = false
    __MODEL_SIGNATURE__ {
        val relativeRoot = (dataRoot + "/" + relativeRootPath).trimEnd('/')
        val boxPath = relativeRoot.split('/').any { it.startsWith("box") }
        if (runner == "AbstractDirectivesValidatorTest") return
        if (!boxPath && !runner.contains("Box") && runner != "AbstractFirWasmJsTranslatorTest") return
        if (selectedTarget.startsWith("wasm") &&
            (runner.contains("Wasi") != (selectedTarget == "wasm-wasi"))) return
        check(!excludeParentDirs) { "unsupported upstream directory-based box model: " + relativeRoot }
        val directory = File(checkoutRoot, relativeRoot)
        check(directory.isDirectory) { "missing upstream box root: " + relativeRoot }
        selectedRoots.add(relativeRoot)
        val matcher = Pattern.compile(pattern)
        val excluded = excludedPattern?.let { Pattern.compile(it) }
        val files = if (recursive) directory.walkTopDown().asSequence()
                    else directory.listFiles().orEmpty().asSequence()
        for (file in files) {
            if (!file.isFile || !matcher.matcher(file.name).matches()) continue
            if (excluded?.matcher(file.name)?.matches() == true) continue
            val relative = file.relativeTo(directory).invariantSeparatorsPath
            if (excludeDirs.any { relative == it || relative.startsWith(it + "/") }) continue
            if (relative.split('/').dropLast(1).any { it in excludeDirsRecursively }) continue
            selectedFiles.add(file.relativeTo(File(checkoutRoot)).invariantSeparatorsPath)
        }
    }
}
fun generateTestGroupSuiteWithJUnit5(
    args: Array<String>,
    mainClassName: String = "",
    init: TestGroupSuite.() -> Unit,
) { TestGroupSuite().init() }
fun <T> provider(): Any = Any()
fun codegenBox(): Any = Any()
fun klib(): Any = Any()
fun temporarilyDisabledBecauseOfKt85080(): Any = Any()
fun klibIrInliner(): Any = Any()
fun klibSyntheticAccessors(): Array<Any> = emptyArray()

object TestGeneratorUtil {
    fun getMainClassName(): String = "SelectionOracle"
    __GENERATOR_CONSTANTS__
}
__OTHER_CONSTANTS__
__RUNNER_CLASSES__
__SPEC_HELPER__
__GENERATOR_MAIN__

fun hex(text: String): String = text.toByteArray(Charsets.UTF_8)
    .joinToString("") { (it.toInt() and 255).toString(16).padStart(2, '0') }

fun box(): String {
    main(arrayOf(checkoutRoot + "/generated"))
    return selectedRoots.joinToString("\n") { "R\t" + hex(it) } + "\n" +
        selectedFiles.joinToString("\n") { "F\t" + hex(it) }
}
