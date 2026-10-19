@file:OptIn(org.jetbrains.kotlin.compiler.plugin.ExperimentalCompilerApi::class)

// Registrars for the host's lifecycle tests. Each records its disposal in the file the `marker`
// option names, so a test can see what was disposed and in which order.
package fixture

import org.jetbrains.kotlin.backend.common.extensions.IrGenerationExtension
import org.jetbrains.kotlin.backend.common.extensions.IrPluginContext
import org.jetbrains.kotlin.compiler.plugin.AbstractCliOption
import org.jetbrains.kotlin.compiler.plugin.CliOption
import org.jetbrains.kotlin.compiler.plugin.CommandLineProcessor
import org.jetbrains.kotlin.compiler.plugin.CompilerPluginRegistrar
import org.jetbrains.kotlin.config.CompilerConfiguration
import org.jetbrains.kotlin.config.CompilerConfigurationKey
import org.jetbrains.kotlin.ir.declarations.IrModuleFragment
import java.io.File

val MARKER: CompilerConfigurationKey<String> = CompilerConfigurationKey.create("fixture marker")

class Options : CommandLineProcessor {
    override val pluginId = "fixture"
    override val pluginOptions = listOf(CliOption("marker", "<path>", "where disposals are recorded", required = true))

    override fun processOption(option: AbstractCliOption, value: String, configuration: CompilerConfiguration) {
        configuration.put(MARKER, value)
    }
}

private class NoOp : IrGenerationExtension {
    override fun generate(moduleFragment: IrModuleFragment, pluginContext: IrPluginContext) {}
}

private fun CompilerPluginRegistrar.ExtensionStorage.recordDisposal(configuration: CompilerConfiguration, name: String) {
    val marker = File(configuration.get(MARKER)!!)
    registerDisposable(object : CompilerPluginRegistrar.PluginDisposable {
        override fun dispose() {
            marker.appendText("$name disposed\n")
        }
    })
}

class First : CompilerPluginRegistrar() {
    override val pluginId = "fixture.first"
    override val supportsK2 = true

    override fun ExtensionStorage.registerExtensions(configuration: CompilerConfiguration) {
        IrGenerationExtension.registerExtension(NoOp())
        recordDisposal(configuration, "first")
    }
}

/** Registers one IR extension more than the storage already holds, so its count shows what it saw. */
class Second : CompilerPluginRegistrar() {
    override val pluginId = "fixture.second"
    override val supportsK2 = true

    override fun ExtensionStorage.registerExtensions(configuration: CompilerConfiguration) {
        repeat(get(IrGenerationExtension).size + 1) { IrGenerationExtension.registerExtension(NoOp()) }
        recordDisposal(configuration, "second")
    }
}

class Failing : CompilerPluginRegistrar() {
    override val pluginId = "fixture.failing"
    override val supportsK2 = true

    override fun ExtensionStorage.registerExtensions(configuration: CompilerConfiguration) {
        recordDisposal(configuration, "failing")
        error("the failing fixture registrar fails")
    }
}
