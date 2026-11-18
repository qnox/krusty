package krusty.pluginhost

import org.jetbrains.kotlin.descriptors.DescriptorVisibilities
import org.jetbrains.kotlin.ir.IrElement
import org.jetbrains.kotlin.ir.declarations.IrClass
import org.jetbrains.kotlin.ir.declarations.IrDeclarationParent
import org.jetbrains.kotlin.ir.declarations.IrFile
import org.jetbrains.kotlin.ir.declarations.IrFunction
import org.jetbrains.kotlin.ir.declarations.IrPackageFragment
import org.jetbrains.kotlin.ir.declarations.IrParameterKind
import org.jetbrains.kotlin.ir.declarations.IrSimpleFunction
import org.jetbrains.kotlin.ir.declarations.IrValueDeclaration
import org.jetbrains.kotlin.ir.declarations.IrVariable
import org.jetbrains.kotlin.ir.expressions.IrBlockBody
import org.jetbrains.kotlin.ir.expressions.IrCall
import org.jetbrains.kotlin.ir.expressions.IrConst
import org.jetbrains.kotlin.ir.expressions.IrConstKind
import org.jetbrains.kotlin.ir.expressions.IrExpression
import org.jetbrains.kotlin.ir.expressions.IrGetValue
import org.jetbrains.kotlin.ir.expressions.IrReturn
import org.jetbrains.kotlin.ir.expressions.IrSetValue
import org.jetbrains.kotlin.ir.symbols.IrClassSymbol
import org.jetbrains.kotlin.ir.symbols.IrTypeParameterSymbol
import org.jetbrains.kotlin.ir.types.IrSimpleType
import org.jetbrains.kotlin.ir.types.IrStarProjection
import org.jetbrains.kotlin.ir.types.IrType
import org.jetbrains.kotlin.ir.types.IrTypeArgument
import org.jetbrains.kotlin.ir.types.IrTypeProjection
import org.jetbrains.kotlin.ir.types.isMarkedNullable
import org.jetbrains.kotlin.ir.util.classId
import org.jetbrains.kotlin.ir.util.parentClassOrNull
import org.jetbrains.kotlin.ir.util.render
import org.jetbrains.kotlin.types.Variance
import java.io.DataOutputStream

/**
 * Writes the module the loaded plugins left, in the framing [ModuleBuilder] reads
 * (`crates/krusty-plugin-host/src/protocol/module_ir.rs` reads it back): the files krusty sent, in
 * its order, each with every declaration it holds now. An element a plugin made without a source
 * position keeps kotlinc's offsets for that, `UNDEFINED_OFFSET` or `SYNTHETIC_OFFSET`.
 *
 * The whole module is written before anything is sent, so IR outside what krusty's plugin IR covers
 * refuses the request by what it is, instead of returning a module with that IR left out.
 */
internal class ModuleWriter(
    /** Each file krusty sent, in its order, with its text. */
    private val sent: Map<IrFile, String>,
) {
    /** Each function of the module as it is now, by its file and its position in it. */
    private val functions = HashMap<IrSimpleFunction, Pair<Int, Int>>()

    fun write(files: List<IrFile>): ByteArray {
        val added = files.filter { it !in sent }
        if (added.isNotEmpty()) {
            throw Refused("a plugin added the file ${added.first().fileEntry.name}, which krusty's plugin IR does not cover yet")
        }
        val missing = sent.keys.filter { it !in files }
        if (missing.isNotEmpty()) throw Refused("a plugin removed the file ${missing.first().fileEntry.name}")
        files.forEachIndexed { fileIndex, file ->
            file.declarations.forEachIndexed { index, declaration ->
                val function = declaration as? IrSimpleFunction
                    ?: throw uncovered(declaration, "a top-level declaration other than a function")
                functions[function] = fileIndex to index
            }
        }
        val bytes = java.io.ByteArrayOutputStream()
        DataOutputStream(bytes).apply {
            writeInt(files.size)
            for (file in files) writeFile(file)
        }.flush()
        return bytes.toByteArray()
    }

    private fun DataOutputStream.writeFile(file: IrFile) {
        writeString(file.fileEntry.name)
        writeString(file.packageFqName.asString())
        writeString(sent.getValue(file))
        writeInt(file.declarations.size)
        for (declaration in file.declarations) writeFunction(declaration as IrSimpleFunction)
    }

    private fun DataOutputStream.writeFunction(function: IrSimpleFunction) {
        writeOffsets(function)
        writeString(function.name.asString())
        writeByte(
            when (function.visibility) {
                DescriptorVisibilities.PUBLIC -> 0
                DescriptorVisibilities.INTERNAL -> 1
                DescriptorVisibilities.PRIVATE -> 2
                else -> throw uncovered(function, "a function with ${function.visibility} visibility")
            },
        )
        if (function.typeParameters.isNotEmpty()) throw uncovered(function, "a function with type parameters")
        if (function.annotations.isNotEmpty()) throw uncovered(function, "an annotated function")
        val modifiers = listOfNotNull(
            "inline".takeIf { function.isInline },
            "suspend".takeIf { function.isSuspend },
            "operator".takeIf { function.isOperator },
            "infix".takeIf { function.isInfix },
            "tailrec".takeIf { function.isTailrec },
            "external".takeIf { function.isExternal },
            "expect".takeIf { function.isExpect },
        )
        if (modifiers.isNotEmpty()) throw uncovered(function, "a function with the ${modifiers.joinToString(" ")} modifier")
        val other = function.parameters.firstOrNull { it.kind != IrParameterKind.Regular }
        if (other != null) throw uncovered(function, "a function with a ${other.kind} parameter")
        writeInt(function.parameters.size)
        for (parameter in function.parameters) {
            if (parameter.defaultValue != null) throw uncovered(parameter, "a parameter with a default value")
            if (parameter.varargElementType != null) throw uncovered(parameter, "a vararg parameter")
            if (parameter.annotations.isNotEmpty()) throw uncovered(parameter, "an annotated parameter")
            if (parameter.isCrossinline || parameter.isNoinline) {
                throw uncovered(parameter, "a crossinline or noinline parameter")
            }
            writeOffsets(parameter)
            writeString(parameter.name.asString())
            writeType(parameter.type, function)
        }
        writeType(function.returnType, function)
        val body = function.body as? IrBlockBody
            ?: throw uncovered(function, "a function without a block body")
        val values = function.parameters.toMutableList<IrValueDeclaration>()
        writeOffsets(body)
        writeInt(body.statements.size)
        for (statement in body.statements) {
            when (statement) {
                is IrVariable -> {
                    writeByte(0)
                    writeVariable(statement, function, values)
                }
                is IrExpression -> {
                    writeByte(1)
                    writeExpression(statement, function, values)
                }
                else -> throw uncovered(statement, "a statement")
            }
        }
    }

    private fun DataOutputStream.writeVariable(
        variable: IrVariable,
        function: IrSimpleFunction,
        values: MutableList<IrValueDeclaration>,
    ) {
        if (variable.isConst || variable.isLateinit) throw uncovered(variable, "a const or lateinit variable")
        if (variable.annotations.isNotEmpty()) throw uncovered(variable, "an annotated variable")
        writeOffsets(variable)
        writeString(variable.name.asString())
        writeType(variable.type, function)
        writeBoolean(variable.isVar)
        writeOptionalExpression(variable.initializer, function, values)
        values += variable
    }

    private fun DataOutputStream.writeOptionalExpression(
        expression: IrExpression?,
        function: IrSimpleFunction,
        values: List<IrValueDeclaration>,
    ) {
        if (expression == null) {
            writeBoolean(false)
        } else {
            writeBoolean(true)
            writeExpression(expression, function, values)
        }
    }

    private fun DataOutputStream.writeExpression(
        expression: IrExpression,
        function: IrSimpleFunction,
        values: List<IrValueDeclaration>,
    ) {
        writeOffsets(expression)
        when (expression) {
            is IrConst -> {
                writeByte(0)
                writeConstant(expression, function)
            }
            is IrGetValue -> {
                writeByte(1)
                writeInt(valueId(expression, expression.symbol.owner, values))
            }
            is IrSetValue -> {
                writeByte(2)
                writeInt(valueId(expression, expression.symbol.owner, values))
                writeExpression(expression.value, function, values)
            }
            is IrReturn -> {
                if (expression.returnTargetSymbol != function.symbol) {
                    throw uncovered(expression, "a return from a declaration other than its function")
                }
                writeByte(3)
                writeExpression(expression.value, function, values)
            }
            is IrCall -> {
                writeByte(4)
                writeCall(expression, function, values)
            }
            else -> throw uncovered(expression, "an expression")
        }
    }

    private fun valueId(at: IrElement, value: IrValueDeclaration, values: List<IrValueDeclaration>): Int {
        val id = values.indexOf(value)
        if (id < 0) throw uncovered(at, "a value that is not a parameter or variable in scope")
        return id
    }

    private fun DataOutputStream.writeConstant(constant: IrConst, function: IrSimpleFunction) {
        when (constant.kind) {
            IrConstKind.Boolean -> {
                writeByte(0)
                writeBoolean(constant.value as Boolean)
            }
            IrConstKind.Int -> {
                writeByte(1)
                writeInt(constant.value as Int)
            }
            IrConstKind.Long -> {
                writeByte(2)
                writeLong(constant.value as Long)
            }
            IrConstKind.String -> {
                writeByte(3)
                val text = constant.value as String
                // The wire carries UTF-8, which cannot spell an unpaired surrogate.
                if (!text.isWellFormedUtf16()) throw uncovered(constant, "a string constant with an unpaired surrogate")
                writeString(text)
            }
            IrConstKind.Null -> {
                writeByte(4)
                writeType(constant.type, function)
            }
            else -> throw uncovered(constant, "a ${constant.kind} constant")
        }
    }

    private fun DataOutputStream.writeCall(call: IrCall, function: IrSimpleFunction, values: List<IrValueDeclaration>) {
        if (call.superQualifierSymbol != null) throw uncovered(call, "a super call")
        val callee = call.symbol.owner
        // An accessor is named by its property, which the callable identity cannot spell yet.
        if (callee.correspondingPropertySymbol != null) throw uncovered(call, "a call of a property accessor")
        val module = functions[callee]
        if (module != null) {
            writeByte(0)
            writeInt(module.first)
            writeInt(module.second)
        } else {
            writeByte(1)
            writeCallable(callee, call)
        }
        writeType(call.type, function)
        writeInt(call.typeArguments.size)
        for (argument in call.typeArguments) {
            writeType(argument ?: throw uncovered(call, "a call with a missing type argument"), function)
        }
        writeInt(call.arguments.size)
        for (argument in call.arguments) writeOptionalExpression(argument, function, values)
    }

    /** [callee] named by its owner and signature, as [readCallable] reads it. */
    private fun DataOutputStream.writeCallable(callee: IrSimpleFunction, at: IrElement) {
        writeByte(0)
        val (packageName, classes) = owner(callee.parent, at)
        writeString(packageName)
        writeInt(classes.size)
        for (name in classes) writeString(name)
        writeString(callee.name.asString())
        writeInt(callee.typeParameters.size)
        fun ofKind(kind: IrParameterKind) = callee.parameters.filter { it.kind == kind }
        val context = ofKind(IrParameterKind.Context)
        writeInt(context.size)
        for (parameter in context) writeType(parameter.type, callee)
        val receiver = ofKind(IrParameterKind.ExtensionReceiver).singleOrNull()
        writeBoolean(receiver != null)
        if (receiver != null) writeType(receiver.type, callee)
        val regular = ofKind(IrParameterKind.Regular)
        writeInt(regular.size)
        for (parameter in regular) writeType(parameter.type, callee)
    }

    /** The package and the class names, outermost first, that [parent] is named by. */
    private fun owner(parent: IrDeclarationParent, at: IrElement): Pair<String, List<String>> = when (parent) {
        is IrPackageFragment -> parent.packageFqName.asString() to emptyList()
        is IrClass -> {
            val classId = parent.classId ?: throw uncovered(at, "a call to a member of a local class")
            classId.packageFqName.asString() to classId.relativeClassName.pathSegments().map { it.asString() }
        }
        else -> throw uncovered(at, "a call to a local function")
    }

    /** [type] as [readType] reads it, its type parameters named by the declaration that owns them. */
    private fun DataOutputStream.writeType(type: IrType, function: IrFunction) {
        if (type !is IrSimpleType) throw Refused("krusty's plugin IR does not cover the type ${type.render()} yet")
        when (val classifier = type.classifier) {
            is IrClassSymbol -> {
                val classId = classifier.owner.classId
                    ?: throw Refused("krusty's plugin IR does not cover the local class type ${type.render()} yet")
                writeByte(0)
                writeString(classId.packageFqName.asString())
                val names = classId.relativeClassName.pathSegments()
                writeInt(names.size)
                for (name in names) writeString(name.asString())
                writeInt(type.arguments.size)
                for (argument in type.arguments) writeArgument(argument, function)
            }
            is IrTypeParameterSymbol -> {
                val parameter = classifier.owner
                writeByte(1)
                when (val parent = parameter.parent) {
                    function -> writeByte(0)
                    is IrClass -> {
                        val classId = parent.classId
                            ?: throw Refused("krusty's plugin IR does not cover a local class's type parameter yet")
                        writeByte(1)
                        writeString(classId.packageFqName.asString())
                        val names = classId.relativeClassName.pathSegments()
                        writeInt(names.size)
                        for (name in names) writeString(name.asString())
                    }
                    else -> throw Refused("krusty's plugin IR does not cover the type parameter ${type.render()} yet")
                }
                writeInt(parameter.index)
            }
            else -> throw Refused("krusty's plugin IR does not cover the type ${type.render()} yet")
        }
        writeBoolean(type.isMarkedNullable())
        // A type annotation travels as its class alone, so one that carries anything more would
        // come back as a different annotation.
        val annotations = type.annotations.map { annotation ->
            if (annotation.typeArguments.isNotEmpty() || annotation.arguments.any { it != null }) {
                throw Refused("krusty's plugin IR does not cover the type annotation ${annotation.render()} yet")
            }
            annotation.symbol.owner.parentClassOrNull?.classId
                ?: throw Refused("krusty's plugin IR does not cover the type annotation ${annotation.render()} yet")
        }
        writeInt(annotations.size)
        for (annotation in annotations) {
            writeString(annotation.packageFqName.asString())
            val names = annotation.relativeClassName.pathSegments()
            writeInt(names.size)
            for (name in names) writeString(name.asString())
        }
    }

    private fun DataOutputStream.writeArgument(argument: IrTypeArgument, function: IrFunction) {
        when (argument) {
            is IrStarProjection -> writeByte(0)
            is IrTypeProjection -> {
                writeByte(
                    when (argument.variance) {
                        Variance.INVARIANT -> 1
                        Variance.IN_VARIANCE -> 2
                        Variance.OUT_VARIANCE -> 3
                    },
                )
                writeType(argument.type, function)
            }
        }
    }

    private fun DataOutputStream.writeOffsets(element: IrElement) {
        writeInt(element.startOffset)
        writeInt(element.endOffset)
    }

    private fun uncovered(element: IrElement, what: String) = Refused(
        "a plugin produced $what, which krusty's plugin IR does not cover yet: ${element.render().lineSequence().first()}",
    )
}

/** Whether every surrogate in the string is half of a pair, as UTF-8 needs. */
private fun String.isWellFormedUtf16(): Boolean {
    var index = 0
    while (index < length) {
        val unit = this[index]
        when {
            unit.isHighSurrogate() -> {
                if (index + 1 >= length || !this[index + 1].isLowSurrogate()) return false
                index += 2
            }
            unit.isLowSurrogate() -> return false
            else -> index++
        }
    }
    return true
}
