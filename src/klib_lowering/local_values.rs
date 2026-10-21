//! The local variables a KLIB body declares, as checked FIR lowering numbers and publishes them.
//!
//! A local variable takes the next value slot after the function's parameters, in declaration
//! order. Checked FIR lowering publishes every read of a `val` as a stable binding read and every
//! read of a `var` as a mutable one; the serialized variable's flags say which it is.

use crate::fx_hash::HashMap;

use super::decline::KlibBodyDeclineReason;
use crate::ir::IrBindingStability;
use crate::metadata::klib_ir::tree::KlibIrVariable;
use crate::metadata::klib_ir::KlibIrSymbol;
use crate::types::Ty;

/// `IrFlags`' local-variable flags, in the order the KLIB serializer packs them: whether the
/// declaration has annotations (which lowering does not read: a local's annotations change nothing
/// checked FIR lowering emits), then `var`, `const` and `lateinit`.
const HAS_ANNOTATIONS: u64 = 1;
const IS_VAR: u64 = 1 << 1;
const IS_CONST: u64 = 1 << 2;
const IS_LATEINIT: u64 = 1 << 3;

/// The origin of a variable the source declares, rather than one the compiler introduced.
const DEFINED: &str = "DEFINED";
/// The parameter of a source `catch` clause.
const CATCH_PARAMETER: &str = "CATCH_PARAMETER";
/// The variable of a source `for` loop, which kotlinc declares from its iterator's `next()`.
const FOR_LOOP_VARIABLE: &str = "FOR_LOOP_VARIABLE";

/// One declared local variable.
#[derive(Clone, Copy, Debug)]
pub(super) struct LocalValue {
    pub(super) index: u32,
    pub(super) ty: Ty,
    pub(super) mutable: bool,
}

impl LocalValue {
    pub(super) fn stability(self) -> IrBindingStability {
        if self.mutable {
            IrBindingStability::Mutable
        } else {
            IrBindingStability::Stable
        }
    }
}

/// The local variables of one body, by serialized symbol.
pub(super) struct LocalValues {
    values: HashMap<KlibIrSymbol, LocalValue>,
    next: u32,
}

impl LocalValues {
    /// No locals yet: the first one takes the slot after the function's `parameters`.
    pub(super) fn after(parameters: usize) -> Self {
        Self {
            values: HashMap::default(),
            next: u32::try_from(parameters).expect("a parameter list fits u32"),
        }
    }

    /// The slot and binding facts of the serialized `variable`, of semantic type `ty`, before it is
    /// declared. Only a source `val` or `var`, a `for` loop's variable among them, is modelled; a
    /// compiler temporary, a `const` and a `lateinit` variable decline by form.
    pub(super) fn slot(
        &self,
        variable: &KlibIrVariable,
        ty: Ty,
    ) -> Result<LocalValue, KlibBodyDeclineReason> {
        if variable.base.origin != DEFINED && variable.base.origin != FOR_LOOP_VARIABLE {
            return Err(KlibBodyDeclineReason::UnsupportedOperation(
                "a compiler-introduced variable",
            ));
        }
        let flags = variable.base.flags;
        if flags & IS_LATEINIT != 0 {
            return Err(KlibBodyDeclineReason::UnsupportedOperation(
                "a `lateinit` variable",
            ));
        }
        if flags & IS_CONST != 0 {
            return Err(KlibBodyDeclineReason::UnsupportedOperation(
                "a `const` variable",
            ));
        }
        if flags & !(HAS_ANNOTATIONS | IS_VAR | IS_CONST | IS_LATEINIT) != 0 {
            return Err(KlibBodyDeclineReason::SignatureMismatch(format!(
                "a local variable carries flags {flags:#x} beyond annotations, `var`, `const` and \
                 `lateinit`"
            )));
        }
        Ok(LocalValue {
            index: self.next,
            ty,
            mutable: flags & IS_VAR != 0,
        })
    }

    /// The slot of a `catch` clause's parameter `variable`, of semantic type `ty`, before it is
    /// declared: an immutable binding the source names.
    pub(super) fn catch_parameter(
        &self,
        variable: &KlibIrVariable,
        ty: Ty,
    ) -> Result<LocalValue, KlibBodyDeclineReason> {
        if variable.base.origin != CATCH_PARAMETER || variable.base.flags & !HAS_ANNOTATIONS != 0 {
            return Err(KlibBodyDeclineReason::SignatureMismatch(
                "a `catch` parameter is not an immutable catch-parameter variable".to_owned(),
            ));
        }
        Ok(LocalValue {
            index: self.next,
            ty,
            mutable: false,
        })
    }

    /// Declare `symbol` in the slot [`Self::slot`] or [`Self::catch_parameter`] gave it.
    pub(super) fn declare(
        &mut self,
        symbol: &KlibIrSymbol,
        value: LocalValue,
    ) -> Result<(), KlibBodyDeclineReason> {
        debug_assert_eq!(value.index, self.next, "locals are declared in slot order");
        if self.values.insert(symbol.clone(), value).is_some() {
            return Err(KlibBodyDeclineReason::SignatureMismatch(
                "a local variable repeats another declaration's identity".to_owned(),
            ));
        }
        self.next += 1;
        Ok(())
    }

    /// Declare the immutable compiler temporary `symbol` of semantic type `ty` in the next slot.
    pub(super) fn temporary(
        &mut self,
        symbol: &KlibIrSymbol,
        ty: Ty,
    ) -> Result<LocalValue, KlibBodyDeclineReason> {
        let value = LocalValue {
            index: self.next,
            ty,
            mutable: false,
        };
        self.declare(symbol, value)?;
        Ok(value)
    }

    pub(super) fn get(&self, symbol: &KlibIrSymbol) -> Option<LocalValue> {
        self.values.get(symbol).copied()
    }

    /// Reserve internal storage without declaring a serialized value identity. A return-target
    /// symbol owns control flow, not a readable local, even when its frame needs result storage.
    pub(super) fn anonymous_slot(&mut self) -> u32 {
        let index = self.next;
        self.next += 1;
        index
    }
}
