//! The boxed result kotlinc gives a primitive override of a non-primitive declaration.
//!
//! kotlinc's JVM signature mapper boxes a function's primitive result when any declaration it
//! overrides returns something else (`forceBoxedReturnTypeOnOverride`): `override fun next(): Int`
//! of `Iterator<T>.next(): T` is `next()Ljava/lang/Integer;`, and so is `invoke` of a
//! `() -> Int` object. The override's own JVM result is the wrapper, so its body boxes once, a call
//! through the class unboxes, and the bridge to the erased declaration returns the box as it is.
//!
//! The Kotlin declaration still returns the primitive, and common IR keeps saying so: its function
//! result, its returns and its calls are untouched. Only the JVM carrier of the result changes, so
//! this pass records the choice in [`OverrideResults`], keyed by function, and the descriptors and
//! the emitter read it there. A declaration of another file takes the same choice from the fact its
//! module record carries, so a caller or a subclass in any file agrees with the declaring one.

use std::collections::{HashMap, HashSet};

use crate::fir::CallableId;
use crate::ir::{is_kotlin_primitive, Callee, ExprId, FunId, IrExpr, IrFile};
use crate::types::Ty;

/// The functions of this file whose JVM result is the wrapper of their primitive Kotlin result.
#[derive(Default)]
pub(crate) struct OverrideResults {
    boxed: HashSet<FunId>,
    /// Value-class member calls the value-class pass realized as a static call of a boxed
    /// member's `-impl`, with the primitive Kotlin result each reads out of the wrapper.
    static_member_calls: HashMap<ExprId, Ty>,
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
                    declaration.overrides_non_primitive_result
                        && is_kotlin_primitive(declaration.result)
                        && !declaration.flags.has(crate::fir::DeclarationFlags::SUSPEND)
                })
                .map(|declaration| declaration.result),
        }
    }

    /// The primitive Kotlin result of the call `expression`, when its callee returns the wrapper in
    /// its place: a member call through the class, a `super` call, a member call of a declaration
    /// in another file, or the static member a value-class call was realized as.
    pub(crate) fn boxed_call_result(&self, ir: &IrFile, expression: ExprId) -> Option<Ty> {
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
                callee: Callee::Static { .. },
                ..
            } => self.static_member_calls.get(&expression).copied(),
            _ => None,
        }
    }
}

/// Choose the wrapper as the JVM result of every override whose primitive result replaces a
/// non-primitive one. Common IR is read, never changed.
pub(super) fn box_primitive_override_results(ir: &IrFile) -> OverrideResults {
    let mut results = OverrideResults::default();
    for (class, declaration) in ir.classes.iter().enumerate() {
        // The classes whose override edges the bridge pass reads; see `derive_bridges`.
        if !declaration.is_source_declared && declaration.enum_entry_of.is_none() {
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
            if edge.implementation_owner == owner
                && ir.classes[class].methods.contains(&function)
                && is_kotlin_primitive(ir.functions[function as usize].ret)
                && !ir.suspend_funs.contains(&function)
                && edge.overrides_non_primitive_result()
            {
                results.boxed.insert(function);
            }
        }
    }
    results
}
