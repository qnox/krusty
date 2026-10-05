//! The boxed result kotlinc gives a scalar result over a reference-returning overridden slot.
//!
//! kotlinc's JVM signature mapper boxes a function's primitive result when any declaration it
//! overrides returns something else (`forceBoxedReturnTypeOnOverride`): `override fun next(): Int`
//! of `Iterator<T>.next(): T` is `next()Ljava/lang/Integer;`, and so is `invoke` of a
//! `() -> Int` object. The override's own JVM result is the wrapper, so its body boxes once, a call
//! through the class unboxes, and the bridge to the erased declaration returns the box as it is.
//!
//! The Kotlin declaration still returns the primitive, and common IR keeps saying so: its function
//! result, its returns and its calls are untouched, and it classifies no type as primitive. The
//! question is one of JVM representation alone ([`scalar_over_reference`]): does the override's
//! result map to an unboxed scalar where an overridden slot maps to a reference? This pass answers
//! it from the exact override edges and records the choice in [`OverrideResults`], keyed by
//! function, and the descriptors and the emitter read it there. A declaration of another file is
//! answered from the overridden results its module record carries, so a caller or a subclass in any
//! file agrees with the declaring one.

use std::collections::{HashMap, HashSet};

use crate::fir::{CallableId, ExternalCallableId, PropertyId};
use crate::ir::{Callee, ExprId, FunId, IrExpr, IrFile};
use crate::jvm::backend::SkipReason;
use crate::jvm::physical_type::ir_ty_to_jvm;
use crate::types::{Ty, TypeName};

/// The functions of this file whose JVM result is the wrapper of their primitive Kotlin result.
#[derive(Default)]
pub(crate) struct OverrideResults {
    boxed: HashSet<FunId>,
    /// Value-class member calls the value-class pass realized as a static call of a boxed
    /// member's `-impl`, with the primitive Kotlin result each reads out of the wrapper.
    static_member_calls: HashMap<ExprId, Ty>,
    /// The properties of this file whose getter result is the wrapper of their primitive type,
    /// by checked identity and by their place among the owning class's properties.
    boxed_properties: HashSet<PropertyId>,
    boxed_members: HashSet<(TypeName, u32)>,
    /// Realized `super` calls of a boxed getter, with the primitive type each reads out of it.
    super_getter_calls: HashMap<ExprId, Ty>,
}

/// Whether a declaration result of `ty` maps to an unboxed JVM scalar of its own. An unsigned type
/// is a value class, whose override result follows the value-class representation instead.
fn scalar_result(ty: Ty) -> bool {
    !ty.is_unsigned() && !matches!(ty, Ty::TyParam(..)) && ir_ty_to_jvm(&ty).is_jvm_scalar()
}

/// Whether an override whose result is `implementation` returns an unboxed JVM scalar where the
/// overridden slot, declared with the unapplied `overridden`, returns a reference. kotlinc then
/// declares the override with the scalar's wrapper (`forceBoxedReturnTypeOnOverride`).
pub(crate) fn scalar_over_reference(implementation: Ty, overridden: Ty) -> bool {
    scalar_result(implementation) && !scalar_result(overridden)
}

/// The overridden results of `target`'s method that its scalar result returns as the wrapper over:
/// each one needs a bridge from its erased slot (see [`scalar_over_reference`]). A suspend method
/// returns `Object` through its continuation contract instead.
pub(crate) fn sam_results_boxed_over(
    target: &crate::ir::IrSamTarget,
) -> impl Iterator<Item = Ty> + '_ {
    target
        .overridden_results
        .iter()
        .copied()
        .filter(move |&overridden| {
            !target.suspend && scalar_over_reference(target.declared_result, overridden)
        })
}

/// Whether the JVM result of `target`'s method is the wrapper of its scalar Kotlin result.
pub(crate) fn boxes_sam_result(target: &crate::ir::IrSamTarget) -> bool {
    sam_results_boxed_over(target).next().is_some()
}

/// The primitive Kotlin result of the selected dependency member when its exact class-file slot is
/// that primitive's wrapper. The identity is already frozen at the frontend/backend boundary; a
/// missing fact is an invalid backend input, never a reason to guess from the semantic type.
pub(crate) fn external_boxed_result(
    callables: &crate::backend::CheckedBackendCallables,
    target: ExternalCallableId,
    declared: Ty,
) -> Result<Option<Ty>, SkipReason> {
    let callable = callables.callable(target).ok_or(SkipReason::Bridges)?;
    if callable.kind != crate::libraries::ExternalCallableKind::Member || !scalar_result(declared) {
        return Ok(None);
    }
    let wrapper = ir_ty_to_jvm(&Ty::nullable(declared));
    let physical = ir_ty_to_jvm(&callable.physical_ret);
    Ok((physical == wrapper).then_some(declared))
}

impl OverrideResults {
    /// Whether `function`'s JVM result is the wrapper of its primitive one.
    pub(crate) fn boxes(&self, function: FunId) -> bool {
        self.boxed.contains(&function)
    }

    /// The result `function` is declared with on the JVM: its Kotlin result, or that result's
    /// wrapper when it is boxed.
    pub(crate) fn physical_result(&self, ir: &IrFile, function: FunId) -> Ty {
        let result = ir.functions[function as usize].ret;
        if self.boxes(function) {
            Ty::nullable(result)
        } else {
            result
        }
    }

    /// Whether the getter of the `index`th property of `owner`, a class of this file, returns the
    /// wrapper of the property's primitive type.
    pub(crate) fn boxes_member_property(&self, owner: TypeName, index: u32) -> bool {
        self.boxed_members.contains(&(owner, index))
    }

    /// The primitive type of the current-module property `property`, in this file or another, when
    /// its getter returns that primitive's wrapper.
    pub(crate) fn boxed_property_result(&self, ir: &IrFile, property: PropertyId) -> Option<Ty> {
        if self.boxed_properties.contains(&property) {
            return ir
                .checked_properties
                .get(&property)
                .map(|declared| declared.ty);
        }
        if ir.checked_properties.contains_key(&property) {
            return None;
        }
        ir.referenced_module_properties
            .get(&property)
            .filter(|declaration| {
                declaration
                    .overridden_types
                    .iter()
                    .any(|&overridden| scalar_over_reference(declaration.ty, overridden))
            })
            .map(|declaration| declaration.ty)
    }

    /// Record that the value-class pass realized `call` as a static call of the `-impl` of a member
    /// whose JVM result is the wrapper of the primitive `result`.
    pub(crate) fn record_static_member_call(&mut self, call: ExprId, result: Ty) {
        self.static_member_calls.insert(call, result);
    }

    /// The primitive Kotlin result of the current-module declaration `callable`, in this file or
    /// another, when its JVM result is that primitive's wrapper.
    pub(crate) fn boxed_callable_result(&self, ir: &IrFile, callable: CallableId) -> Option<Ty> {
        match ir.checked_callable_functions.get(&callable) {
            Some(&function) => self
                .boxes(function)
                .then(|| ir.functions[function as usize].ret),
            None => ir
                .referenced_module_callables
                .get(&callable)
                .filter(|declaration| {
                    !declaration.flags.has(crate::fir::DeclarationFlags::SUSPEND)
                        && declaration.overridden_results.iter().any(|&overridden| {
                            scalar_over_reference(declaration.result, overridden)
                        })
                })
                .map(|declaration| declaration.result),
        }
    }

    /// The primitive Kotlin result of the call `expression`, when its callee returns the wrapper in
    /// its place: a member call through the class, a `super` call, a member call of a declaration
    /// in another file, or the static member a value-class call was realized as.
    pub(crate) fn boxed_call_result(&self, ir: &IrFile, expression: ExprId) -> Option<Ty> {
        let boxed = self.declared_boxed_call_result(ir, expression)?;
        // A call realized through an overridden dependency slot takes that slot's descriptor:
        // `Collection.size()I` for an `override val size: Int` returns the scalar itself.
        match ir.jvm_overridden_call_realizations.get(&expression) {
            Some(realization)
                if realization
                    .descriptor
                    .rsplit_once(')')
                    .is_some_and(|(_, result)| result.len() == 1) =>
            {
                None
            }
            _ => Some(boxed),
        }
    }

    fn declared_boxed_call_result(&self, ir: &IrFile, expression: ExprId) -> Option<Ty> {
        match ir.expr(expression) {
            IrExpr::MethodCall { class, index, .. } => {
                let function = ir.classes[*class as usize].methods[*index as usize];
                self.boxes(function)
                    .then(|| ir.functions[function as usize].ret)
            }
            IrExpr::Call {
                callee:
                    Callee::Special {
                        source: Some(callable),
                        ..
                    }
                    | Callee::Virtual {
                        module_target: Some(callable),
                        ..
                    },
                ..
            } => self.boxed_callable_result(ir, *callable),
            IrExpr::Call {
                callee:
                    Callee::Virtual {
                        target:
                            Some(crate::ir::IrVirtualTarget::PropertyGetter(
                                crate::fir::ResolvedPropertyOverrideTarget::Module(property),
                            )),
                        ..
                    },
                ..
            } => self.boxed_property_result(ir, *property),
            IrExpr::Call {
                callee: Callee::Static { .. },
                ..
            } => self.static_member_calls.get(&expression).copied(),
            IrExpr::Call {
                callee: Callee::Special { source: None, .. },
                ..
            } => self.super_getter_calls.get(&expression).copied(),
            _ => None,
        }
    }
}

/// Source classes, enum-entry subclasses, and anonymous classes copied for a reified inline call.
/// A copied class is not a source declaration, but it carries the declaration's override edges
/// retargeted at its specialized methods, and those methods need the same bridges.
pub(super) fn realizes_overrides(ir: &IrFile, class: usize) -> bool {
    let declaration = &ir.classes[class];
    declaration.is_source_declared
        || declaration.enum_entry_of.is_some()
        || ir
            .specialized_anonymous_classes
            .contains_key(&u32::try_from(class).expect("class index"))
}

/// Choose the wrapper as the JVM result of every override whose scalar result stands over a
/// reference-returning overridden slot. Common IR is read, never changed.
pub(super) fn box_primitive_override_results(
    ir: &IrFile,
    callables: &crate::backend::CheckedBackendCallables,
    property_realizations: &crate::jvm::property_realizations::PropertyRealizations,
) -> Result<OverrideResults, SkipReason> {
    let mut results = OverrideResults::default();
    for class in &ir.classes {
        if let Some(wrapper) = &class.sam_wrapper {
            if wrapper.boxes_primitive_result {
                results.boxed.insert(wrapper.method);
            }
        }
    }
    for (class, declaration) in ir.classes.iter().enumerate() {
        // The classes whose override edges the bridge pass reads; see `derive_bridges`.
        if !realizes_overrides(ir, class) {
            continue;
        }
        let owner = declaration.fq_name;
        let Some(edges) = ir.function_overrides.get(&owner) else {
            continue;
        };
        for edge in edges {
            let Some(function) = crate::jvm::bridges::implementation_function(ir, edge) else {
                continue;
            };
            let dependency_boxed_result = match edge.overridden {
                crate::fir::ResolvedFunctionOverrideTarget::External(target) if !edge.suspend => {
                    external_boxed_result(callables, target, edge.declared_result)?.is_some()
                }
                _ => false,
            };
            let result = ir.functions[function as usize].ret;
            if edge.implementation_owner == owner
                && ir.classes[class].methods.contains(&function)
                && scalar_result(result)
                && !ir.suspend_funs.contains(&function)
                && (scalar_over_reference(result, edge.declared_result) || dependency_boxed_result)
            {
                results.boxed.insert(function);
            }
        }
        box_primitive_property_overrides(ir, callables, owner, &mut results)?;
    }
    for (call, property) in property_realizations.super_getters() {
        if let Some(result) = results.boxed_property_result(ir, property) {
            results.super_getter_calls.insert(call, result);
        }
    }
    Ok(results)
}

/// The property analogue: a primitive property of `owner` overriding one whose type is not a
/// primitive, or a dependency getter already returning the wrapper, returns the wrapper from its
/// getter, its source-written getter or the delegation forwarder standing for it included.
fn box_primitive_property_overrides(
    ir: &IrFile,
    callables: &crate::backend::CheckedBackendCallables,
    owner: TypeName,
    results: &mut OverrideResults,
) -> Result<(), SkipReason> {
    let Some(edges) = ir.property_overrides.get(&owner) else {
        return Ok(());
    };
    for edge in edges {
        if edge.implementation_owner != owner
            || !scalar_result(edge.implementation_type)
            || edge.implementation_receiver.is_some()
            || !(scalar_over_reference(edge.implementation_type, edge.declared_type)
                || external_boxed_getter(callables, edge.overridden, edge.declared_type)?)
        {
            continue;
        }
        if let Some(forwarder) = edge.implementation_getter {
            results.boxed.insert(forwarder);
            continue;
        }
        let crate::fir::ResolvedPropertyOverrideTarget::Module(property) = edge.implementation
        else {
            continue;
        };
        let Some(crate::ir::IrLocalPropertyLayout::Member {
            owner: declaring,
            getter,
            property: index,
            ..
        }) = ir.local_property_layouts.get(&property)
        else {
            continue;
        };
        if *declaring != owner {
            continue;
        }
        results.boxed_properties.insert(property);
        results.boxed_members.insert((owner, *index));
        if let Some(getter) = getter {
            results.boxed.insert(*getter);
        }
    }
    Ok(())
}

/// Whether `property`, when a dependency's, has a getter returning the wrapper of its primitive
/// `declared` type (see [`external_boxed_result`]).
pub(crate) fn external_boxed_getter(
    callables: &crate::backend::CheckedBackendCallables,
    property: crate::fir::ResolvedPropertyOverrideTarget,
    declared: Ty,
) -> Result<bool, SkipReason> {
    match property {
        crate::fir::ResolvedPropertyOverrideTarget::External(target) => {
            Ok(external_boxed_result(callables, target, declared)?.is_some())
        }
        crate::fir::ResolvedPropertyOverrideTarget::Module(_) => Ok(false),
    }
}
