package krusty.pluginhost

import org.jetbrains.kotlin.descriptors.DescriptorVisibilities
import org.jetbrains.kotlin.descriptors.DescriptorVisibility
import org.jetbrains.kotlin.fir.pipeline.Fir2IrActualizedResult
import org.jetbrains.kotlin.ir.builders.declarations.addValueParameter
import org.jetbrains.kotlin.ir.builders.declarations.buildFun
import org.jetbrains.kotlin.ir.builders.declarations.buildVariable
import org.jetbrains.kotlin.ir.declarations.IrDeclarationOrigin
import org.jetbrains.kotlin.ir.declarations.IrFile
import org.jetbrains.kotlin.ir.declarations.IrSimpleFunction
import org.jetbrains.kotlin.ir.declarations.IrValueDeclaration
import org.jetbrains.kotlin.ir.expressions.IrBody
import org.jetbrains.kotlin.ir.expressions.IrConstKind
import org.jetbrains.kotlin.ir.expressions.IrExpression
import org.jetbrains.kotlin.ir.expressions.IrStatementOrigin
import org.jetbrains.kotlin.ir.expressions.impl.IrCallImpl
import org.jetbrains.kotlin.ir.expressions.impl.IrConstImpl
import org.jetbrains.kotlin.ir.expressions.impl.IrGetValueImpl
import org.jetbrains.kotlin.ir.expressions.impl.IrReturnImpl
import org.jetbrains.kotlin.ir.expressions.impl.IrSetValueImpl
import org.jetbrains.kotlin.ir.IrStatement
import org.jetbrains.kotlin.ir.symbols.IrSimpleFunctionSymbol
import org.jetbrains.kotlin.ir.types.IrType
import org.jetbrains.kotlin.ir.types.IrTypeArgument
import org.jetbrains.kotlin.ir.types.createType
import org.jetbrains.kotlin.ir.types.impl.IrStarProjectionImpl
import org.jetbrains.kotlin.ir.types.impl.makeTypeProjection
import org.jetbrains.kotlin.ir.util.NaiveSourceBasedFileEntryImpl
import org.jetbrains.kotlin.name.FqName
import org.jetbrains.kotlin.name.Name
import java.io.DataInputStream

/**
 * Builds a module's IR, read from krusty's framing (`crates/krusty-plugin-host/src/protocol/module_ir.rs`),
 * in [ir]'s session, the way kotlinc's fir2ir builds a module's sources: every declaration first, so
 * a body can call any function of the module, then every body.
 */
internal class ModuleBuilder(private val ir: Fir2IrActualizedResult) {
    private val builtIns = ir.irBuiltIns
    private val factory = ir.pluginContext.irFactory

    /** The module's functions, by file and then by position in it, as krusty numbers them. */
    private val functions = mutableListOf<List<IrSimpleFunction>>()

    /** Reads the module's files and builds them, unattached to the session's module. */
    fun read(input: DataInputStream): List<IrFile> {
        val files = input.readList { readFile() }
        for ((file, bodies) in files) {
            for ((function, body) in file.declarations.filterIsInstance<IrSimpleFunction>().zip(bodies)) {
                function.body = body.build(function)
            }
        }
        return files.map { it.first }
    }

    /** A file with its functions declared, and each function's body still to build. */
    private fun DataInputStream.readFile(): Pair<IrFile, List<BodyReader>> {
        val path = readString()
        val packageName = readString()
        val text = readString()
        val lineStarts = listOf(0) + text.indices.filter { text[it] == '\n' }.map { it + 1 }
        val entry = NaiveSourceBasedFileEntryImpl(path, lineStarts.toIntArray(), text.length)
        val file = newIrFile(entry, FqName(packageName), ir.irModuleFragment)
        val bodies = mutableListOf<BodyReader>()
        val declared = readList {
            val (function, body) = readFunction(file)
            bodies += body
            function
        }
        functions += declared
        file.declarations += declared
        return file to bodies
    }

    private fun DataInputStream.readFunction(file: IrFile): Pair<IrSimpleFunction, BodyReader> {
        val start = readInt()
        val end = readInt()
        val name = readString()
        val visibility = readVisibility()
        val function = factory.buildFun {
            startOffset = start
            endOffset = end
            this.name = Name.identifier(name)
            this.visibility = visibility
            origin = IrDeclarationOrigin.DEFINED
        }
        function.parent = file
        val parameters = readList { Parameter(readInt(), readInt(), readString(), readType(function)) }
        for (parameter in parameters) {
            function.addValueParameter {
                startOffset = parameter.start
                endOffset = parameter.end
                this.name = Name.identifier(parameter.name)
                type = parameter.type
            }
        }
        function.returnType = readType(function)
        return function to BodyReader(readBody())
    }

    private class Parameter(val start: Int, val end: Int, val name: String, val type: IrType)

    private fun DataInputStream.readVisibility(): DescriptorVisibility = when (val tag = readByte().toInt()) {
        0 -> DescriptorVisibilities.PUBLIC
        1 -> DescriptorVisibilities.INTERNAL
        2 -> DescriptorVisibilities.PRIVATE
        else -> throw Refused("unknown visibility $tag")
    }

    /** A body as read, built once every function of the module is declared. */
    private inner class BodyReader(private val body: Body) {
        fun build(function: IrSimpleFunction): IrBody {
            val values = function.parameters.toMutableList<IrValueDeclaration>()
            return factory.createBlockBody(body.start, body.end).also { block ->
                block.statements += body.statements.map { it.build(function, values) }
            }
        }
    }

    // A body is read in full before it is built, since its calls may name functions declared later.
    private fun DataInputStream.readBody(): Body {
        val start = readInt()
        val end = readInt()
        return Body(start, end, readList { readStatement() })
    }

    private class Body(val start: Int, val end: Int, val statements: List<Node>)

    /** A statement or expression as read, built against its function and the values in scope. */
    private fun interface Node {
        fun build(function: IrSimpleFunction, values: MutableList<IrValueDeclaration>): IrStatement
    }

    private fun Node.expression(function: IrSimpleFunction, values: MutableList<IrValueDeclaration>) =
        build(function, values) as IrExpression

    private fun DataInputStream.readStatement(): Node = when (val tag = readByte().toInt()) {
        0 -> readVariable()
        1 -> readExpression()
        else -> throw Refused("unknown statement kind $tag")
    }

    private fun DataInputStream.readVariable(): Node {
        val start = readInt()
        val end = readInt()
        val name = readString()
        val type = readTypeNode()
        val mutable = readBoolean()
        val initializer = readOptionalExpression()
        return Node { function, values ->
            val variable = buildVariable(
                function, start, end, IrDeclarationOrigin.DEFINED, Name.identifier(name), type(function), mutable,
            )
            variable.initializer = initializer?.expression(function, values)
            values += variable
            variable
        }
    }

    private fun DataInputStream.readOptionalExpression(): Node? = if (readBoolean()) readExpression() else null

    private fun DataInputStream.readExpression(): Node {
        val start = readInt()
        val end = readInt()
        return when (val tag = readByte().toInt()) {
            0 -> readConstant(start, end)
            1 -> {
                val id = readInt()
                Node { _, values -> IrGetValueImpl(start, end, value(values, id).symbol, null) }
            }
            2 -> {
                val id = readInt()
                val to = readExpression()
                Node { function, values ->
                    IrSetValueImpl(
                        start, end, builtIns.unitType, value(values, id).symbol, to.expression(function, values),
                        IrStatementOrigin.EQ,
                    )
                }
            }
            3 -> {
                val value = readExpression()
                Node { function, values ->
                    IrReturnImpl(start, end, builtIns.nothingType, function.symbol, value.expression(function, values))
                }
            }
            4 -> readCall(start, end)
            else -> throw Refused("unknown expression kind $tag")
        }
    }

    private fun value(values: List<IrValueDeclaration>, id: Int): IrValueDeclaration =
        values.getOrNull(id) ?: throw Refused("value $id is not in scope")

    private fun DataInputStream.readConstant(start: Int, end: Int): Node = when (val tag = readByte().toInt()) {
        0 -> {
            val value = readBoolean()
            Node { _, _ -> IrConstImpl(start, end, builtIns.booleanType, IrConstKind.Boolean, value) }
        }
        1 -> {
            val value = readInt()
            Node { _, _ -> IrConstImpl(start, end, builtIns.intType, IrConstKind.Int, value) }
        }
        2 -> {
            val value = readLong()
            Node { _, _ -> IrConstImpl(start, end, builtIns.longType, IrConstKind.Long, value) }
        }
        3 -> {
            val value = readString()
            Node { _, _ -> IrConstImpl(start, end, builtIns.stringType, IrConstKind.String, value) }
        }
        4 -> {
            val type = readTypeNode()
            Node { function, _ -> IrConstImpl(start, end, type(function), IrConstKind.Null, null) }
        }
        else -> throw Refused("unknown constant kind $tag")
    }

    private fun DataInputStream.readCall(start: Int, end: Int): Node {
        val callee: () -> IrSimpleFunctionSymbol = when (val tag = readByte().toInt()) {
            0 -> {
                val file = readInt()
                val index = readInt()
                ({
                    functions.getOrNull(file)?.getOrNull(index)?.symbol
                        ?: throw Refused("the module has no function $index in file $file")
                })
            }
            1 -> {
                val identity = readCallable()
                if (identity.kind != CallableKind.FUNCTION) throw Refused("$identity is not a function")
                ({ resolveFunction(ir, identity).symbol })
            }
            else -> throw Refused("unknown callee kind $tag")
        }
        val type = readTypeNode()
        val typeArguments = readList { readTypeNode() }
        val arguments = readList { readOptionalExpression() }
        return Node { function, values ->
            val symbol = callee()
            val parameters = symbol.owner.parameters.size
            if (arguments.size != parameters) {
                throw Refused("a call to ${symbol.owner.name} passes ${arguments.size} arguments to its $parameters parameters")
            }
            val call = IrCallImpl(start, end, type(function), symbol, typeArguments.size, null, null)
            typeArguments.forEachIndexed { index, argument -> call.typeArguments[index] = argument(function) }
            arguments.forEachIndexed { index, argument -> call.arguments[index] = argument?.expression(function, values) }
            call
        }
    }

    /** A type read before the function whose type parameters it may name is known. */
    private fun DataInputStream.readTypeNode(): (IrSimpleFunction) -> IrType {
        val identity = readType()
        return { function -> irType(identity, function) }
    }

    private fun DataInputStream.readType(function: IrSimpleFunction): IrType = irType(readType(), function)

    private fun irType(identity: TypeIdentity, function: IrSimpleFunction): IrType {
        if (identity.annotations.isNotEmpty()) {
            throw Refused("the plugin host cannot build annotated types yet: ${identity.annotations.joinToString()}")
        }
        return when (identity) {
            is ClassType -> {
                val symbol = ir.pluginContext.referenceClass(identity.classifier.classId)
                    ?: throw Refused("${identity.classifier} names no class on the session's classpath")
                symbol.createType(identity.nullable, identity.arguments.map { irArgument(it, function) })
            }
            is TypeParameterType -> throw Refused(
                "the plugin host cannot build a module function's type parameters yet",
            )
        }
    }

    private fun irArgument(identity: ArgumentIdentity, function: IrSimpleFunction): IrTypeArgument = when (identity) {
        Star -> IrStarProjectionImpl
        is Projection -> makeTypeProjection(irType(identity.type, function), identity.variance)
    }
}
