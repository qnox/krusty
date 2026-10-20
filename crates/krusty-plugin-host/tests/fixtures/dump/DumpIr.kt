@file:OptIn(org.jetbrains.kotlin.compiler.plugin.ExperimentalCompilerApi::class)

// An IR plugin that writes the module's IR, as kotlinc dumps it, to the file its `out` option names,
// then every element's offsets with the line and column its start is at: what a test compares between kotlinc's own IR and the plugin host's.
package fixture.dump

import org.jetbrains.kotlin.backend.common.extensions.IrGenerationExtension
import org.jetbrains.kotlin.backend.common.extensions.IrPluginContext
import org.jetbrains.kotlin.compiler.plugin.AbstractCliOption
import org.jetbrains.kotlin.compiler.plugin.CliOption
import org.jetbrains.kotlin.compiler.plugin.CommandLineProcessor
import org.jetbrains.kotlin.compiler.plugin.CompilerPluginRegistrar
import org.jetbrains.kotlin.config.CompilerConfiguration
import org.jetbrains.kotlin.config.CompilerConfigurationKey
import org.jetbrains.kotlin.ir.IrElement
import org.jetbrains.kotlin.ir.declarations.IrModuleFragment
import org.jetbrains.kotlin.ir.util.dump
import org.jetbrains.kotlin.ir.util.render
import org.jetbrains.kotlin.ir.visitors.IrVisitorVoid
import org.jetbrains.kotlin.ir.visitors.acceptChildrenVoid
import org.jetbrains.kotlin.ir.visitors.acceptVoid
import java.io.File

val OUT: CompilerConfigurationKey<String> = CompilerConfigurationKey.create("dump out")

class DumpOptions : CommandLineProcessor {
    override val pluginId = "fixture.dump"
    override val pluginOptions = listOf(CliOption("out", "<path>", "where the dump goes", required = true))

    override fun processOption(option: AbstractCliOption, value: String, configuration: CompilerConfiguration) {
        configuration.put(OUT, value)
    }
}

class DumpRegistrar : CompilerPluginRegistrar() {
    override val pluginId = "fixture.dump"
    override val supportsK2 = true

    override fun ExtensionStorage.registerExtensions(configuration: CompilerConfiguration) {
        IrGenerationExtension.registerExtension(Dump(configuration.get(OUT)!!))
    }
}

private class Dump(private val out: String) : IrGenerationExtension {
    override fun generate(moduleFragment: IrModuleFragment, pluginContext: IrPluginContext) {
        val text = StringBuilder()
        for (file in moduleFragment.files) {
            text.append(file.dump())
            file.acceptVoid(object : IrVisitorVoid() {
                override fun visitElement(element: IrElement) {
                    val start = element.startOffset
                    val at = if (start < 0) "" else "${file.fileEntry.getLineNumber(start)}:${file.fileEntry.getColumnNumber(start)} "
                    text.append("${start}:${element.endOffset} $at${element.render().lineSequence().first()}\n")
                    element.acceptChildrenVoid(this)
                }
            })
        }
        File(out).writeText(text.toString())
    }
}
