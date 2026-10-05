//! Every dependency identity one checked file's IR references: in its expressions, and in the
//! checked side tables that carry a selected dependency declaration beside them. The expression
//! walk is exhaustive on purpose, so a new node that carries an identity fails to compile here
//! instead of leaving its declaration without a frozen fact.

use std::collections::BTreeSet;

use crate::fir::{
    ExternalCallableId, ExternalPropertyId, FirCallTarget, FirPropertyReferenceTarget,
    FirPropertyTarget, ResolvedFunctionOverrideTarget, ResolvedPropertyOverrideTarget,
};
use crate::ir::{
    Callee, IrCallableReferenceTarget, IrCheckedConstructorTarget, IrCheckedOperation, IrExpr,
    IrFile, IrProgressionSource,
};

/// The dependency callables and properties one checked file names, each once.
#[derive(Debug, Default, Eq, PartialEq)]
pub(crate) struct ReferencedDependencies {
    pub callables: BTreeSet<ExternalCallableId>,
    pub properties: BTreeSet<ExternalPropertyId>,
}

pub(crate) fn referenced_dependencies(ir: &IrFile) -> ReferencedDependencies {
    let mut referenced = ReferencedDependencies::default();
    for expression in &ir.exprs {
        referenced.expression(ir, expression);
    }
    referenced.callables.extend(
        ir.external_super_constructors
            .values()
            .chain(ir.external_secondary_super_constructors.values())
            .map(|target| target.declaration),
    );
    for edge in ir.function_overrides.values().flatten() {
        referenced.function_override(edge.implementation);
        referenced.function_override(edge.overridden);
    }
    for edge in ir.property_overrides.values().flatten() {
        referenced.property_override(edge.implementation);
        referenced.property_override(edge.overridden);
    }
    for default in ir.inherited_defaults.values().flatten() {
        match default.body {
            crate::fir::InheritedDefaultBody::DependencyInterfaceMethod(declaration)
            | crate::fir::InheritedDefaultBody::DependencyHolder(declaration) => {
                referenced.callables.insert(declaration);
            }
            crate::fir::InheritedDefaultBody::Module
            | crate::fir::InheritedDefaultBody::JavaDefaultMethod => {}
        }
    }
    for plan in ir
        .checked_properties
        .values()
        .filter_map(|property| property.delegate_plan.as_ref())
    {
        for call in plan
            .provide_delegate
            .iter()
            .chain([&plan.get_value])
            .chain(&plan.set_value)
        {
            referenced.call_target(&call.target);
        }
    }
    referenced
}

/// Exact dependency declarations selected by semantic `super` calls. Target plugins may append
/// these calls after the ordinary handoff inventory was frozen, so the JVM boundary re-reads this
/// narrow carrier once plugin output is final.
pub(super) fn external_super_callables(ir: &IrFile) -> BTreeSet<ExternalCallableId> {
    ir.exprs
        .iter()
        .filter_map(|expression| match expression {
            IrExpr::Call {
                callee:
                    Callee::Super {
                        declaration: Some(ResolvedFunctionOverrideTarget::External(declaration)),
                        ..
                    },
                ..
            } => Some(*declaration),
            _ => None,
        })
        .collect()
}

impl ReferencedDependencies {
    fn expression(&mut self, ir: &IrFile, expression: &IrExpr) {
        match expression {
            IrExpr::Call { callee, .. } => self.callee(callee),
            IrExpr::New {
                external_target, ..
            } => self.callables.extend(*external_target),
            IrExpr::CallableReference(reference) => match &reference.target {
                IrCallableReferenceTarget::External { declaration, .. } => {
                    self.callables.insert(*declaration);
                }
                IrCallableReferenceTarget::Module(_)
                | IrCallableReferenceTarget::Constructor { .. }
                | IrCallableReferenceTarget::Local { .. }
                | IrCallableReferenceTarget::FunctionValueConversion { .. }
                | IrCallableReferenceTarget::FunctionInvoke
                | IrCallableReferenceTarget::Classifier { .. } => {}
            },
            IrExpr::Checked(operation) => self.checked(operation),
            // A local delegated access carries expression templates selected by the frontend.
            // Walk every operand explicitly so dependency identities used by a delegate, an
            // optional dispatch receiver, or a setter value remain frozen even if expression
            // storage stops being a flat all-nodes arena.
            IrExpr::LocalDelegateAccess(access) => {
                self.expression(ir, ir.expr(access.delegate));
                if let Some(receiver) = access.dispatch_receiver {
                    self.expression(ir, ir.expr(receiver));
                }
                if let Some(value) = access.value {
                    self.expression(ir, ir.expr(value));
                }
            }
            // Equality carries no declaration identity itself, but both operands are ordinary IR
            // expressions and may carry one. Visit them explicitly so this exhaustive inventory
            // remains correct if expression storage stops being a flat all-nodes arena.
            IrExpr::Equality { lhs, rhs, .. } => {
                self.expression(ir, ir.expr(*lhs));
                self.expression(ir, ir.expr(*rhs));
            }
            IrExpr::Const(_)
            | IrExpr::BottomValue { .. }
            | IrExpr::ClassConst { .. }
            | IrExpr::KClassLiteral { .. }
            | IrExpr::LocalPropertyReference { .. }
            | IrExpr::SingletonValue { .. }
            | IrExpr::GetValue(_)
            | IrExpr::SetValue { .. }
            | IrExpr::PluginPlaceholder { .. }
            | IrExpr::Return(_)
            | IrExpr::Block { .. }
            | IrExpr::When { .. }
            | IrExpr::TypeOp { .. }
            | IrExpr::While { .. }
            | IrExpr::Break { .. }
            | IrExpr::Continue { .. }
            | IrExpr::Variable { .. }
            | IrExpr::PrimitiveBinOp { .. }
            | IrExpr::PrimitiveNeg { .. }
            | IrExpr::StringConcat(_)
            // A backend-realized property access; before the handoff a dependency property
            // access is still an `ExternalPropertyRead`/`ExternalPropertyWrite` operation.
            | IrExpr::PropertyRead { .. }
            | IrExpr::PropertyWrite { .. }
            | IrExpr::EnclosingInstance { .. }
            | IrExpr::GetField { .. }
            | IrExpr::LateinitInitialized { .. }
            | IrExpr::SetField { .. }
            | IrExpr::GetStatic(_)
            | IrExpr::SetStatic { .. }
            | IrExpr::MethodCall { .. }
            | IrExpr::EnumEntry { .. }
            | IrExpr::StaticInstance { .. }
            | IrExpr::ExternalStaticField { .. }
            | IrExpr::EnumValues { .. }
            | IrExpr::EnumValueOf { .. }
            | IrExpr::EnumEntries { .. }
            | IrExpr::ReifiedClassMarker { .. }
            | IrExpr::ReifiedTypeOp { .. }
            // Debug-frame provenance names no dependency declaration.
            | IrExpr::InlineFrameMarker
            | IrExpr::Lambda { .. }
            | IrExpr::UnitInstance
            | IrExpr::CurrentContinuation
            | IrExpr::InvokeFunction { .. }
            | IrExpr::NotNullAssert { .. }
            | IrExpr::LateinitCheck { .. }
            | IrExpr::ExternalStaticInstance { .. }
            | IrExpr::RefNew { .. }
            | IrExpr::RefGet { .. }
            | IrExpr::RefSet { .. }
            | IrExpr::Throw { .. }
            | IrExpr::Vararg { .. }
            | IrExpr::NewArray { .. }
            | IrExpr::Try { .. }
            // This marker names a constructor value slot, not a dependency declaration.
            | IrExpr::ForwardedSuperArgument { .. } => {}
        }
    }

    fn callee(&mut self, callee: &Callee) {
        match callee {
            Callee::External {
                target,
                default_provider,
                ..
            } => {
                self.callables.insert(*target);
                self.callables.extend(*default_provider);
            }
            Callee::ModuleWithDefaults {
                default_provider, ..
            } => self.function_override(*default_provider),
            Callee::Super {
                declaration: Some(ResolvedFunctionOverrideTarget::External(declaration)),
                ..
            } => {
                self.callables.insert(*declaration);
            }
            Callee::Local(_)
            | Callee::ClassStatic { .. }
            | Callee::ClassStaticWithDefaults { .. }
            | Callee::ClassStaticDefault { .. }
            | Callee::LocalDefault(_)
            | Callee::LocalWithDefaults { .. }
            | Callee::Intrinsic { .. }
            | Callee::CrossFile { .. }
            | Callee::Module { .. }
            | Callee::Static { .. }
            | Callee::Virtual { .. }
            | Callee::Super { .. }
            | Callee::Special { .. } => {}
        }
    }

    fn checked(&mut self, operation: &IrCheckedOperation) {
        match operation {
            IrCheckedOperation::ConstructorDelegation { target, .. } => match target {
                IrCheckedConstructorTarget::External { declaration, .. } => {
                    self.callables.insert(*declaration);
                }
                IrCheckedConstructorTarget::Module(_) => {}
            },
            IrCheckedOperation::ExternalPropertyRead { target, .. }
            | IrCheckedOperation::ExternalPropertyWrite { target, .. } => {
                self.properties.insert(*target);
            }
            IrCheckedOperation::RangeLoop {
                source,
                unsigned_compare,
                ..
            } => {
                self.progression(source);
                self.callables
                    .extend(unsigned_compare.iter().map(|compare| compare.function));
            }
            IrCheckedOperation::PropertyReference { target, .. } => {
                self.property_reference(target);
            }
            IrCheckedOperation::Call { .. }
            | IrCheckedOperation::PropertyRead { .. }
            | IrCheckedOperation::PropertyWrite { .. }
            | IrCheckedOperation::LateinitFieldRead { .. }
            | IrCheckedOperation::BackingFieldRead { .. }
            | IrCheckedOperation::BackingFieldWrite { .. }
            | IrCheckedOperation::RangeConstruction { .. }
            | IrCheckedOperation::RangeContains { .. }
            | IrCheckedOperation::IllegalProgressionStep { .. } => {}
        }
    }

    fn property_reference(&mut self, target: &FirPropertyReferenceTarget) {
        let FirPropertyReferenceTarget::External { getter, setter, .. } = target else {
            return;
        };
        self.property_target(getter);
        if let Some(setter) = setter {
            self.property_target(setter);
        }
    }

    fn property_target(&mut self, target: &FirPropertyTarget) {
        if let FirPropertyTarget::External { property, .. } = target {
            self.properties.insert(*property);
        }
    }

    fn progression(&mut self, source: &IrProgressionSource) {
        match source {
            IrProgressionSource::Step {
                nested,
                last_element,
                ..
            } => {
                self.callables.insert(last_element.function);
                self.progression(nested);
            }
            IrProgressionSource::Reversed(nested) => self.progression(nested),
            IrProgressionSource::Literal { .. } | IrProgressionSource::Value { .. } => {}
        }
    }

    fn call_target(&mut self, target: &FirCallTarget) {
        match target {
            FirCallTarget::External {
                declaration,
                default_provider,
                ..
            } => {
                self.callables.insert(*declaration);
                self.callables.extend(*default_provider);
            }
            FirCallTarget::Module(_)
            | FirCallTarget::Intrinsic { .. }
            | FirCallTarget::Classifier { .. }
            | FirCallTarget::Super { .. } => {}
        }
    }

    fn function_override(&mut self, target: ResolvedFunctionOverrideTarget) {
        match target {
            ResolvedFunctionOverrideTarget::External(callable) => {
                self.callables.insert(callable);
            }
            ResolvedFunctionOverrideTarget::Module(_) => {}
        }
    }

    fn property_override(&mut self, target: ResolvedPropertyOverrideTarget) {
        match target {
            ResolvedPropertyOverrideTarget::External(accessor) => {
                self.callables.insert(accessor);
            }
            ResolvedPropertyOverrideTarget::Module(_) => {}
        }
    }
}
