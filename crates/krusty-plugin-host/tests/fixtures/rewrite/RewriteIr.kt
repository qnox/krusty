@file:OptIn(org.jetbrains.kotlin.compiler.plugin.ExperimentalCompilerApi::class)

// An IR plugin that changes the module it runs over, as real IR plugins do: it adds 1 to every Int
// constant, and starts every function with a call of `println` with the function's name, made
// without a source position. Every other mode also leaves IR krusty's plugin IR does not cover:
// `branch` returns every returned value through a `when`, `default` gives each function's first
// parameter a default value, `accessor` reads `"".length` before the `println`, `inline` makes
// each function inline, `annotated` annotates each function with `@PublishedApi`, `typeannotation`
// annotates each return type with `@JvmSuppressWildcards(false)`, and `surrogate` prints a string
// holding an unpaired surrogate.
package fixture.rewrite

import org.jetbrains.kotlin.backend.common.extensions.IrGenerationExtension
import org.jetbrains.kotlin.backend.common.extensions.IrPluginContext
import org.jetbrains.kotlin.compiler.plugin.AbstractCliOption
import org.jetbrains.kotlin.compiler.plugin.CliOption
import org.jetbrains.kotlin.compiler.plugin.CommandLineProcessor
import org.jetbrains.kotlin.compiler.plugin.CompilerPluginRegistrar
import org.jetbrains.kotlin.config.CompilerConfiguration
import org.jetbrains.kotlin.config.CompilerConfigurationKey
import org.jetbrains.kotlin.ir.UNDEFINED_OFFSET
import org.jetbrains.kotlin.ir.declarations.IrModuleFragment
import org.jetbrains.kotlin.ir.declarations.IrSimpleFunction
import org.jetbrains.kotlin.ir.expressions.IrBlockBody
import org.jetbrains.kotlin.ir.expressions.IrConst
import org.jetbrains.kotlin.ir.expressions.IrConstKind
import org.jetbrains.kotlin.ir.expressions.IrExpression
import org.jetbrains.kotlin.ir.expressions.IrReturn
import org.jetbrains.kotlin.ir.expressions.impl.IrBranchImpl
import org.jetbrains.kotlin.ir.expressions.impl.IrAnnotationImpl
import org.jetbrains.kotlin.ir.expressions.impl.fromSymbolOwner
import org.jetbrains.kotlin.ir.expressions.impl.IrCallImpl
import org.jetbrains.kotlin.ir.expressions.impl.IrConstImpl
import org.jetbrains.kotlin.ir.expressions.impl.IrWhenImpl
import org.jetbrains.kotlin.ir.types.addAnnotations
import org.jetbrains.kotlin.ir.types.defaultType
import org.jetbrains.kotlin.ir.types.isNullableAny
import org.jetbrains.kotlin.ir.util.constructors
import org.jetbrains.kotlin.ir.visitors.IrTransformer
import org.jetbrains.kotlin.name.CallableId
import org.jetbrains.kotlin.name.ClassId
import org.jetbrains.kotlin.name.FqName
import org.jetbrains.kotlin.name.Name

val MODE: CompilerConfigurationKey<String> = CompilerConfigurationKey.create("rewrite mode")

class RewriteOptions : CommandLineProcessor {
    override val pluginId = "fixture.rewrite"
    override val pluginOptions = listOf(CliOption("mode", "plain|branch|default|accessor|inline|annotated|typeannotation|surrogate", "what to rewrite", required = false))

    override fun processOption(option: AbstractCliOption, value: String, configuration: CompilerConfiguration) {
        configuration.put(MODE, value)
    }
}

class RewriteRegistrar : CompilerPluginRegistrar() {
    override val pluginId = "fixture.rewrite"
    override val supportsK2 = true

    override fun ExtensionStorage.registerExtensions(configuration: CompilerConfiguration) {
        IrGenerationExtension.registerExtension(Rewrite(configuration.get(MODE) ?: "plain"))
    }
}

private class Rewrite(private val mode: String) : IrGenerationExtension {
    override fun generate(moduleFragment: IrModuleFragment, pluginContext: IrPluginContext) {
        val builtIns = pluginContext.irBuiltIns
        val println = pluginContext.referenceFunctions(CallableId(FqName("kotlin.io"), Name.identifier("println")))
            .single { it.owner.parameters.singleOrNull()?.type?.isNullableAny() == true }
        for (file in moduleFragment.files) {
            for (function in file.declarations.filterIsInstance<IrSimpleFunction>()) {
                function.transformChildren(object : IrTransformer<Nothing?>() {
                    override fun visitConst(expression: IrConst, data: Nothing?): IrExpression {
                        if (expression.kind != IrConstKind.Int) return expression
                        return IrConstImpl(
                            expression.startOffset, expression.endOffset, expression.type, IrConstKind.Int,
                            (expression.value as Int) + 1,
                        )
                    }

                    override fun visitReturn(expression: IrReturn, data: Nothing?): IrExpression {
                        expression.transformChildren(this, null)
                        if (mode == "branch") {
                            val value = expression.value
                            expression.value = IrWhenImpl(value.startOffset, value.endOffset, value.type).apply {
                                branches += IrBranchImpl(
                                    IrConstImpl(UNDEFINED_OFFSET, UNDEFINED_OFFSET, builtIns.booleanType, IrConstKind.Boolean, true),
                                    value,
                                )
                            }
                        }
                        return expression
                    }
                }, null)
                val call = IrCallImpl(UNDEFINED_OFFSET, UNDEFINED_OFFSET, builtIns.unitType, println, 0, null, null)
                call.arguments[0] = IrConstImpl(
                    UNDEFINED_OFFSET, UNDEFINED_OFFSET, builtIns.stringType, IrConstKind.String, if (mode == "surrogate") "\uD800" else function.name.asString(),
                )
                (function.body as IrBlockBody).statements.add(0, call)
                when (mode) {
                    "default" -> function.parameters.firstOrNull()?.let { parameter ->
                        parameter.defaultValue = pluginContext.irFactory.createExpressionBody(
                            UNDEFINED_OFFSET, UNDEFINED_OFFSET,
                            IrConstImpl(UNDEFINED_OFFSET, UNDEFINED_OFFSET, parameter.type, IrConstKind.Int, 0),
                        )
                    }
                    "accessor" -> {
                        val length = pluginContext.referenceProperties(
                            CallableId(FqName("kotlin"), FqName("String"), Name.identifier("length")),
                        ).single().owner.getter!!.symbol
                        val read = IrCallImpl(UNDEFINED_OFFSET, UNDEFINED_OFFSET, builtIns.intType, length, 0, null, null)
                        read.arguments[0] = IrConstImpl(
                            UNDEFINED_OFFSET, UNDEFINED_OFFSET, builtIns.stringType, IrConstKind.String, "",
                        )
                        (function.body as IrBlockBody).statements.add(0, read)
                    }
                    "inline" -> function.isInline = true
                    "typeannotation" -> {
                        val suppress = pluginContext.referenceClass(ClassId.fromString("kotlin/jvm/JvmSuppressWildcards"))!!
                        val annotation = IrAnnotationImpl.fromSymbolOwner(
                            UNDEFINED_OFFSET, UNDEFINED_OFFSET, suppress.defaultType,
                            suppress.constructors.single(), null,
                        )
                        annotation.arguments[0] = IrConstImpl(
                            UNDEFINED_OFFSET, UNDEFINED_OFFSET, builtIns.booleanType, IrConstKind.Boolean, false,
                        )
                        function.returnType = function.returnType.addAnnotations(listOf(annotation))
                    }
                    "annotated" -> {
                        val publishedApi = pluginContext.referenceClass(ClassId.fromString("kotlin/PublishedApi"))!!
                        function.annotations += IrAnnotationImpl.fromSymbolOwner(
                            UNDEFINED_OFFSET, UNDEFINED_OFFSET, publishedApi.defaultType,
                            publishedApi.constructors.single(), null,
                        )
                    }
                }
            }
        }
    }
}
