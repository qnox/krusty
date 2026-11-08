package krusty.pluginhost

import org.jetbrains.kotlin.ir.expressions.IrConst
import org.jetbrains.kotlin.ir.expressions.IrConstKind
import org.jetbrains.kotlin.ir.expressions.impl.IrConstImpl
import org.jetbrains.kotlin.ir.types.IrType

/** A constant as kotlinc before 2.5.0-Beta2 builds one, through the builder's argument list. */
internal fun irConst(start: Int, end: Int, type: IrType, kind: IrConstKind, value: Any?): IrConst =
    IrConstImpl(start, end, type, kind, value)
