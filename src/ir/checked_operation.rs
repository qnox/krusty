//! The checked operations common lowering carries from checked FIR to the backend: each names a
//! semantic target and call shape the frontend already selected.

use super::{
    ExprId, IrCheckedArgument, IrCheckedConstructorTarget, IrCheckedSubstitution, IrLoopIndex,
    IrProgressionSource, IrPropertyDispatch, IrRuntimeFunction,
};
use crate::types::Ty;

/// Operations whose semantic target and call shape were finalized in checked FIR. Common lowering
/// translates body-local operands only; target realization belongs to the backend/module emitter.
#[derive(Clone, Debug, PartialEq)]
pub enum IrCheckedOperation {
    Call {
        target: crate::fir::CallableId,
        dispatch_receiver: Option<ExprId>,
        extension_receiver: Option<ExprId>,
        arguments: Vec<IrCheckedArgument>,
        substitutions: Vec<IrCheckedSubstitution>,
    },
    ConstructorDelegation {
        target: IrCheckedConstructorTarget,
        outer_parameter: Option<Ty>,
        outer_receiver: Option<ExprId>,
        arguments: Vec<IrCheckedArgument>,
        substitutions: Vec<IrCheckedSubstitution>,
    },
    PropertyRead {
        target: crate::fir::PropertyId,
        dispatch_receiver: Option<ExprId>,
        extension_receiver: Option<ExprId>,
        context_arguments: Vec<ExprId>,
        substitutions: Vec<IrCheckedSubstitution>,
    },
    PropertyWrite {
        target: crate::fir::PropertyId,
        dispatch_receiver: Option<ExprId>,
        extension_receiver: Option<ExprId>,
        context_arguments: Vec<ExprId>,
        value: ExprId,
        substitutions: Vec<IrCheckedSubstitution>,
    },
    /// Read a dependency property selected by checked FIR. `target` is an opaque provider-owned
    /// declaration identity; common lowering deliberately does not interpret it as a method or a
    /// field. The target backend realizes that choice after common IR has been produced.
    ExternalPropertyRead {
        target: crate::fir::ExternalPropertyId,
        dispatch: IrPropertyDispatch,
        receiver: Option<ExprId>,
        arguments: Vec<ExprId>,
        parameters: Vec<Ty>,
        result: Ty,
        source_receiver: Option<Ty>,
    },
    /// Write a dependency property selected by checked FIR. As with `ExternalPropertyRead`, the
    /// physical storage/accessor choice belongs exclusively to the target backend.
    ExternalPropertyWrite {
        target: crate::fir::ExternalPropertyId,
        dispatch: IrPropertyDispatch,
        receiver: Option<ExprId>,
        arguments: Vec<ExprId>,
        parameters: Vec<Ty>,
        result: Ty,
        source_receiver: Option<Ty>,
    },
    LateinitFieldRead {
        target: crate::fir::PropertyId,
        dispatch_receiver: Option<ExprId>,
    },
    BackingFieldRead {
        target: crate::fir::PropertyId,
        dispatch_receiver: Option<ExprId>,
    },
    BackingFieldWrite {
        target: crate::fir::PropertyId,
        dispatch_receiver: Option<ExprId>,
        value: ExprId,
    },
    RangeConstruction {
        operation: crate::fir::FirRangeOperation,
        start: ExprId,
        start_type: Ty,
        end: ExprId,
        end_type: Ty,
        result: Ty,
    },
    RangeContains {
        operation: crate::fir::FirRangeOperation,
        value: ExprId,
        start: ExprId,
        end: ExprId,
        negated: bool,
        counter: Ty,
        /// Exact selected unsigned comparison consumed by common realization. `None` for every
        /// signed, character or floating-point counter.
        unsigned_compare: Option<IrRuntimeFunction>,
    },
    /// `value in start..end` over the closed range `Comparable.rangeTo` builds. `lower` is the
    /// checked `value.compareTo(start)` call selected on `Comparable`; the value is compared with
    /// `end` through the same member. Common realization orders and stores the operands as
    /// kotlinc's `RangeContainsLowering` does.
    ComparableRangeContains {
        lower: ExprId,
        end: ExprId,
        negated: bool,
    },
    /// `step`'s failure on a step that is not positive: an `IllegalArgumentException` whose
    /// message is `Step must be positive, was: $step.`.
    IllegalProgressionStep { step: ExprId },
    RangeLoop {
        variable: u32,
        /// Source identity of the loop variable. A representation backend attaches it to the
        /// declaration it creates; it must not recover the name from a slot or generated spelling.
        variable_name: Option<Box<str>>,
        counter: Ty,
        source: IrProgressionSource,
        /// The selected comparison ordering an unsigned counter; `None` for any other counter.
        unsigned_compare: Option<IrRuntimeFunction>,
        body: ExprId,
        label: String,
        /// The index a destructured `withIndex()` loop over the progression counts beside it.
        with_index: Option<IrLoopIndex>,
    },
    PropertyReference {
        target: crate::fir::FirPropertyReferenceTarget,
        /// This reference is the compiler-generated `KProperty` metadata argument of a delegated
        /// property, not a source-written callable-reference value. Backends may realize the two
        /// checked semantic shapes differently; common lowering still retains only the selected
        /// property identity and never chooses a platform representation.
        delegated: bool,
        binding: crate::fir::FirCallableReferenceBinding,
        dispatch_receiver: Option<ExprId>,
        extension_receiver: Option<ExprId>,
        mutable: bool,
        substitutions: Vec<IrCheckedSubstitution>,
        adaptation: Option<Box<crate::fir::FirReferenceAdaptation>>,
    },
}
