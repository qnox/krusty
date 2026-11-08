package krusty.pluginhost

import org.jetbrains.kotlin.ir.expressions.IrConst
import org.jetbrains.kotlin.ir.expressions.IrConstKind
import org.jetbrains.kotlin.ir.expressions.impl.IrConstImpl
import org.jetbrains.kotlin.ir.types.IrType

/**
 * A constant as kotlinc from 2.5.0-Beta2 builds one. The builder that took
 * `(start, end, type, kind, value)` is gone. The constructor that receives the indicator is
 * internal, so each kind uses the factory that passes that indicator.
 */
internal fun irConst(start: Int, end: Int, type: IrType, kind: IrConstKind, value: Any?): IrConst =
    when (kind) {
        IrConstKind.Boolean -> IrConstImpl.boolean(start, end, type, value as Boolean)
        IrConstKind.Char -> IrConstImpl.char(start, end, type, value as Char)
        IrConstKind.Byte -> IrConstImpl.byte(start, end, type, value as Byte)
        IrConstKind.Short -> IrConstImpl.short(start, end, type, value as Short)
        IrConstKind.Int -> IrConstImpl.int(start, end, type, value as Int)
        IrConstKind.Long -> IrConstImpl.long(start, end, type, value as Long)
        IrConstKind.String -> IrConstImpl.string(start, end, type, value as String)
        IrConstKind.Float -> IrConstImpl.float(start, end, type, value as Float)
        IrConstKind.Double -> IrConstImpl.double(start, end, type, value as Double)
        IrConstKind.Null -> IrConstImpl.constNull(start, end, type)
    }
