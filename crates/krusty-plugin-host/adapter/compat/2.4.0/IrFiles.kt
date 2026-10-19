package krusty.pluginhost

import org.jetbrains.kotlin.ir.IrFileEntry
import org.jetbrains.kotlin.ir.declarations.IrFile
import org.jetbrains.kotlin.ir.declarations.IrModuleFragment
import org.jetbrains.kotlin.ir.declarations.impl.IrFileImpl
import org.jetbrains.kotlin.ir.symbols.impl.IrFileSymbolImpl
import org.jetbrains.kotlin.name.FqName

/** A file of [module], as kotlinc before 2.4.20 makes one: the module is set after construction. */
internal fun newIrFile(entry: IrFileEntry, packageName: FqName, module: IrModuleFragment): IrFile =
    IrFileImpl(entry, IrFileSymbolImpl(), packageName).also { it.module = module }
