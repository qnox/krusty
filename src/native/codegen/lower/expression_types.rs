//! The two types Native reads off a lowered expression.
//!
//! The CHECKED type is the frontend's, as common lowering hands it over
//! ([`crate::ir::IrFile::checked_type`]): what the value is in Kotlin. It answers every semantic
//! question — which range, list or map member a call is, which scalar a comparison reads, what a
//! `when` merges to. Nothing here works it out again from the node's shape.
//!
//! The PHYSICAL type is the form the lowering of one node hands its value over in, which is the
//! checked type except where the node reads something stored or declared at another type: a local
//! slot, a field or a static at its declared type, a call at its callee's declared result — a type
//! parameter's erasure being a reference where the checked type is an `Int` — a runtime member at
//! the carrier the runtime answers, and an arithmetic operator at the width it is computed at. [`BodyLowering::convert`] takes a value from that form to
//! whatever a position needs, so this is the only place the two can disagree.

use super::*;

impl BodyLowering<'_, '_, '_> {
    /// The checked Kotlin type of `id`'s value, or `None` when lowering recorded none.
    ///
    /// A block with no value is `Unit` by its shape. A read of a slot whose read has no recorded
    /// type has the type its declaration gave the slot, as Wasm reads it too.
    pub(super) fn checked_type(&self, id: u32) -> Option<Ty> {
        let ty = match self.file.ir.expr(id) {
            IrExpr::Block { value: None, .. } => Some(Ty::Unit),
            IrExpr::GetValue(slot) => {
                self.file
                    .ir
                    .checked_type(id)
                    .or_else(|| match self.values.get(slot) {
                        Some(&(_, ty)) => Some(ty),
                        None => self.unit_values.contains(slot).then_some(Ty::Unit),
                    })
            }
            _ => self.file.ir.checked_type(id),
        };
        if ty.is_none() {
            let rendered = format!("{:?}", self.file.ir.expr(id));
            crate::trace_compiler!(
                "native",
                "expression {id} without a checked type: {}",
                &rendered[..rendered.len().min(200)]
            );
        }
        ty
    }

    /// The type `id`'s lowered value is carried at: how wide it is, and how to read the bits in it.
    ///
    /// The representation answers the first. The checked type answers the second where the value
    /// is an UNSIGNED scalar the erasure stored as the signed one sharing its bits: a `UInt` boxed
    /// as that `Int` prints `-1879048193` for `0x8fffffffU`, and one compared as it answers
    /// `0uL >= ULong.MAX_VALUE` true. That holds only for a scalar no wider than the checked type
    /// says — a pointer is not the value at all, `val any: Any = 7u` being still checked as `UInt`.
    pub(super) fn physical_type(&self, id: u32) -> Option<Ty> {
        let physical = self.representation(id)?;
        let bits = |ty: Ty| self.carrier(ty).clif().map(|clif| clif.bits());
        match self.file.ir.checked_type(id) {
            Some(checked)
                if checked.is_unsigned()
                    && matches!(self.carrier(physical), Carrier::Scalar(..))
                    && bits(checked) <= bits(physical) =>
            {
                Some(checked)
            }
            _ => Some(physical),
        }
    }

    /// The representation lowering produces `id`'s value in; see the module documentation.
    fn representation(&self, id: u32) -> Option<Ty> {
        let ir = &self.file.ir;
        Some(match ir.expr(id) {
            // Storage, at the type it is stored at.
            IrExpr::GetValue(slot) => match self.values.get(slot) {
                Some(&(_, ty)) => ty,
                None if self.unit_values.contains(slot) => Ty::Unit,
                None => return None,
            },
            IrExpr::GetField { class, index, .. }
            // The RAW field behind `::prop.isInitialized`; the comparison against null is a node
            // of its own around this one.
            | IrExpr::LateinitInitialized { class, index, .. } => {
                super::super::super::captures::physical_ty(
                    ir,
                    *class,
                    *index,
                    ir.classes[*class as usize].fields[*index as usize].ty,
                )
            }
            IrExpr::EnclosingInstance { inner, .. } => {
                let (class, field) = self.enclosing_field(*inner).ok()?;
                ir.classes[class as usize].fields[field as usize].ty
            }
            IrExpr::GetStatic(index) => ir.statics[*index as usize].ty,
            IrExpr::RefGet { elem, .. } | IrExpr::RefSet { elem, .. } => *elem,
            // An object this lowering or the runtime builds: a captured variable's cell, a
            // lambda's closure, a reflection object.
            IrExpr::RefNew { .. }
            | IrExpr::Lambda { .. }
            | IrExpr::KClassLiteral { .. }
            | IrExpr::LocalPropertyReference(_)
            | IrExpr::Checked(IrCheckedOperation::PropertyReference { .. }) => any(),
            IrExpr::Checked(IrCheckedOperation::PropertyRead { target, .. }) => {
                match self.file.is_imported_property(target) {
                    // A property declared in another file: the type its getter entry point
                    // returns.
                    true => self.module_property_ty(target)?,
                    false => ir.checked_properties.get(target)?.ty,
                }
            }
            // A declaration's result, at the type the declaration gives it.
            IrExpr::Call { callee, .. } => match callee {
                Callee::Local(function)
                | Callee::LocalWithDefaults { function, .. }
                | Callee::ClassStaticWithDefaults { function, .. } => {
                    ir.functions[*function as usize].ret
                }
                Callee::External { ret, .. }
                | Callee::Intrinsic { ret, .. }
                | Callee::Module { ret, .. }
                | Callee::ModuleWithDefaults { ret, .. }
                | Callee::Super { ret, .. }
                // Another file's function returns what its declaration fixes; both files carry
                // that type by the same projection, a value class as its value.
                | Callee::CrossFile { ret, .. } => *ret,
                Callee::Special { source, .. } => {
                    let function = ir.checked_callable_functions.get(&(*source)?)?;
                    ir.functions[*function as usize].ret
                }
                // A VIRTUAL call yields what the slot it dispatches through carries: the DECLARED
                // return rather than the one this receiver's class narrows it to, which for a
                // generic member is a reference.
                Callee::Virtual {
                    params: Some((_, ret)),
                    ..
                } => *ret,
                _ => return None,
            },
            IrExpr::MethodCall { class, index, .. } => {
                let function = ir.classes[*class as usize].methods[*index as usize];
                ir.functions[function as usize].ret
            }
            IrExpr::InvokeFunction { ret, .. } => *ret,
            // What the accessor a local delegated property's access calls declares.
            IrExpr::LocalDelegateAccess(access) => {
                let plan = ir.local_delegate_plans.get(access.plan as usize)?;
                match access.value {
                    Some(_) => plan.setter.as_ref()?.result,
                    None => plan.getter.result,
                }
            }
            // A `try` merges every arm at its result's carrier.
            IrExpr::Try { result, .. } => *result,
            IrExpr::Checked(IrCheckedOperation::ExternalPropertyRead {
                target,
                receiver: Some(receiver),
                ..
            }) if self.runtime_property_answer(*target, *receiver).is_some() => {
                self.runtime_property_answer(*target, *receiver)?
            }
            // A node passing a value through hands it over as it received it. `x!!` takes the
            // nullability off, unboxing a nullable primitive.
            IrExpr::Block {
                value: Some(value), ..
            } => self.physical_type(*value)?,
            IrExpr::NotNullAssert { operand, .. } => self.physical_type(*operand)?.non_null(),
            IrExpr::LateinitCheck { operand, .. } => self.physical_type(*operand)?,
            // A node naming its own type is produced at that type.
            IrExpr::Const(_)
            | IrExpr::UnitInstance
            | IrExpr::TypeOp { .. }
            | IrExpr::StringConcat(_)
            | IrExpr::PrimitiveNeg { .. }
            | IrExpr::Equality { .. }
            | IrExpr::New { .. }
            | IrExpr::SingletonValue { .. }
            | IrExpr::EnumEntry { .. }
            | IrExpr::EnumValueOf { .. }
            | IrExpr::EnumValues { .. }
            | IrExpr::EnumEntries { .. }
            | IrExpr::NewArray { .. }
            | IrExpr::Vararg { .. }
            | IrExpr::CallableReference(_)
            | IrExpr::Checked(IrCheckedOperation::RangeConstruction { .. }) => ir.named_type(id)?,
            // A `when` merges its arms at its checked type, an arithmetic operator is narrowed to
            // the result its selected operator declares, and a dependency property's other reads
            // answer at theirs.
            IrExpr::Block { value: None, .. }
            | IrExpr::When { .. }
            | IrExpr::PrimitiveBinOp { .. }
            | IrExpr::Checked(IrCheckedOperation::ExternalPropertyRead { .. }) => {
                self.checked_type(id)?
            }
            _ => return None,
        })
    }

    /// The carrier a dependency property's runtime answer arrives in, where that is not the
    /// checked result's: a lazy's value and a pair's components are references whatever the
    /// checked element type, an indexed value's index is an `Int`, a list's `size` and a map's
    /// members are what the runtime returns, and a reflective property's what its getter declares.
    fn runtime_property_answer(
        &self,
        target: crate::fir::ExternalPropertyId,
        receiver: u32,
    ) -> Option<Ty> {
        // In the order lowering tries them: the first that claims the read decides.
        if self.enum_member_name(target).is_some() {
            return None;
        }
        if self.reference_property_role(target).is_some() {
            return self
                .file
                .callables
                .property(target)
                .map(|property| property.result);
        }
        if self.class_name_accessor(target).is_some()
            || self.throwable_field(target).is_some()
            || self.is_text_length(target)
            || self.external_getter_is_indices(target)
            || self.range_getter(target, receiver).is_some()
        {
            return None;
        }
        if self.lazy_getter(target, receiver).is_some()
            || self.pair_getter(target, receiver).is_some()
        {
            return Some(any());
        }
        if let Some(name) = self.indexed_value_getter(target, receiver) {
            return Some(lists::indexed_value_getter_ty(&name));
        }
        if self.list_getter(target, receiver).is_some() {
            return Some(Ty::Int);
        }
        self.map_getter(target, receiver)
            .map(|property| property.answer())
    }
}
