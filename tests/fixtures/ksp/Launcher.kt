import com.google.devtools.ksp.processing.*
import com.google.devtools.ksp.impl.KotlinSymbolProcessing
import java.io.File
import java.net.URLClassLoader
import java.util.ServiceLoader

private fun log() = object : KSPLogger {
    override fun logging(message: String, symbol: com.google.devtools.ksp.symbol.KSNode?) {}
    override fun info(message: String, symbol: com.google.devtools.ksp.symbol.KSNode?) {}
    override fun warn(message: String, symbol: com.google.devtools.ksp.symbol.KSNode?) { println("[w] $message") }
    override fun error(message: String, symbol: com.google.devtools.ksp.symbol.KSNode?) { println("[e] $message") }
    override fun exception(e: Throwable) { e.printStackTrace() }
}

fun main(args: Array<String>) {
    val srcDir = File(args[0]); val outBase = File(args[1]); val procJar = File(args[2]); val jdk = File(args[3])
    val libs = args.drop(4).map { File(it) }.filter { it.exists() }
    listOf("classes","kotlin","java","resources","caches").forEach { File(outBase, it).mkdirs() }
    val config = KSPJvmConfig.Builder().apply {
        moduleName = "app"
        sourceRoots = listOf(srcDir)
        javaSourceRoots = emptyList()
        libraries = libs
        projectBaseDir = outBase
        outputBaseDir = outBase
        cachesDir = File(outBase, "caches")
        classOutputDir = File(outBase, "classes")
        kotlinOutputDir = File(outBase, "kotlin")
        javaOutputDir = File(outBase, "java")
        resourceOutputDir = File(outBase, "resources")
        jdkHome = jdk
        jvmTarget = "17"
        languageVersion = "2.0"
        apiVersion = "2.0"
    }.build()
    // Load the processor provider FROM THE JAR via ServiceLoader (genuine from-jar discovery).
    val cl = URLClassLoader(arrayOf(procJar.toURI().toURL()), KSPJvmConfig::class.java.classLoader)
    val providers = ServiceLoader.load(SymbolProcessorProvider::class.java, cl).toList()
    println("providers discovered from jar: ${providers.size}")
    val code = KotlinSymbolProcessing(config, providers, log()).execute()
    println("KSP exit code: $code")
    if (code != KotlinSymbolProcessing.ExitCode.OK) kotlin.system.exitProcess(1)
}
