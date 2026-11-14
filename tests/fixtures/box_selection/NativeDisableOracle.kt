@CLASS_LEVEL_PROPERTIES@
const val KLIB_IR_INLINER = "klibIrInliner"
class CacheMode(val alias: Alias = Alias.WITHOUT_CACHE) { enum class Alias { WITHOUT_CACHE } }
enum class TestMode { TWO_STAGE_MULTI_MODULE }
enum class OptimizationMode { DEBUG }
enum class GC(val shortcut: String) { NOOP("noop") }
enum class GCSchedulerType { DEFAULT }
class GCType(val gc: GC? = null)
class GCScheduler(val scheduler: GCSchedulerType? = null)
enum class Allocator { STD }
enum class ThreadStateChecker { DISABLED }
enum class KlibIrInlinerMode { DEFAULT }
enum class Family {
    LINUX, MINGW, OSX, IOS, TVOS, WATCHOS, ANDROID;
    val isAppleFamily: Boolean get() = this in listOf(OSX, IOS, TVOS, WATCHOS)
}
enum class Architecture { X64, ARM64, X86, ARM32 }
class Target {
    val name = "@TARGET@"
    val family = Family.LINUX
    val architecture = Architecture.@ARCH@
    fun supportsCoreSymbolication() = false
}
class KotlinNativeTargets(val testTarget: Target = Target())
class Settings {
    inline fun <reified T: Any> get(): T = when (T::class) {
        CacheMode::class -> CacheMode()
        TestMode::class -> TestMode.TWO_STAGE_MULTI_MODULE
        OptimizationMode::class -> OptimizationMode.DEBUG
        GCType::class -> GCType()
        GCScheduler::class -> GCScheduler()
        Allocator::class -> Allocator.STD
        ThreadStateChecker::class -> ThreadStateChecker.DISABLED
        KotlinNativeTargets::class -> KotlinNativeTargets()
        KlibIrInlinerMode::class -> KlibIrInlinerMode.DEFAULT
        else -> error("unsupported oracle setting")
    } as T
}
class StringDirective
class RegisteredDirectives(val values: List<String>, val present: Boolean) {
    operator fun get(directive: StringDirective) = values
    operator fun contains(directive: StringDirective) = present
}
@CONSTANTS@
@EVALUATOR@
fun excluded(values: List<String>): Boolean = Settings().evaluate(RegisteredDirectives(values, true), StringDirective())
fun box(): String = listOf(@QUERIES@).joinToString("\n")
