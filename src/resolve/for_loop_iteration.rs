//! What a `for` loop iterates through, as checked FIR consumes it: the iterator protocol, selected
//! as ordinary operator calls, and for a `kotlin.ranges` progression the `first`, `last` and `step`
//! members kotlinc's `ForLoopsLowering` reads instead of iterating.

use crate::ast::{ExprId, StmtId};
use crate::diag::Span;
use crate::fir::{ExternalCallableId, ExternalPropertyId};
use crate::symbol_source::SymbolSource;
use crate::types::{wk, Ty};
use std::collections::HashMap;

use super::{Checker, CheckerScope, IncDecSite, IteratorProtocolTarget};

/// The members of a `kotlin.ranges` progression class a counted loop reads, selected from the
/// class's own declarations. Only the class identity is compiler-known (`wk::progression_class`);
/// the element type is the selected `first`'s type.
#[derive(Clone, Debug, PartialEq)]
pub struct ProgressionPlan {
    pub class: wk::ProgressionClass,
    pub first: ProgressionMember,
    pub last: ProgressionMember,
    pub step: ProgressionMember,
    /// The comparison a counted loop over unsigned elements orders them with: the declaration
    /// carrying the provider's `UnsignedCompare` role for the element's carrier. `None` for a
    /// signed or `Char` element.
    pub compare: Option<RuntimeFunction>,
}

/// The progression facts a counted loop reads, keyed by progression class. A class's member plan
/// and the `getProgressionLastElement` overload a `step` over it needs are selected independently,
/// so a loop that never steps does not depend on the helper.
#[derive(Clone, Debug, Default)]
pub struct ProgressionPlans {
    members: HashMap<Ty, ProgressionPlan>,
    /// The overload a stepped progression of the class moves its `last` with
    /// (`getProgressionLastElementByReturnType`), over the element type, or `Int` for a `Char`
    /// progression.
    last_elements: HashMap<Ty, RuntimeFunction>,
}

impl ProgressionPlans {
    /// The selected `first`, `last` and `step` of progression class `class`.
    pub fn plan(&self, class: Ty) -> Option<&ProgressionPlan> {
        self.members.get(&class)
    }

    /// The `getProgressionLastElement` overload a `step` over progression class `class` calls.
    pub fn last_element(&self, class: Ty) -> Option<&RuntimeFunction> {
        self.last_elements.get(&class)
    }
}

impl super::TypeInfo {
    pub fn progression_plan(&self, class: Ty) -> Option<&ProgressionPlan> {
        self.progression_plans.plan(class)
    }

    pub fn progression_last_element(&self, class: Ty) -> Option<&RuntimeFunction> {
        self.progression_plans.last_element(class)
    }
}

/// A stdlib function a counted loop calls on its own, selected from the provider's declarations.
#[derive(Clone, Debug, PartialEq)]
pub struct RuntimeFunction {
    pub function: ExternalCallableId,
    pub parameters: Box<[Ty]>,
    pub result: Ty,
}

/// A selected member property of a progression class, read on a receiver of that class.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ProgressionMember {
    pub property: ExternalPropertyId,
    /// The selected property's type on the progression receiver.
    pub ty: Ty,
}

impl Checker<'_> {
    pub(super) fn iterator_protocol_target(
        &mut self,
        scope: &CheckerScope<'_>,
        statement: Option<StmtId>,
        iterable_ty: Ty,
        span: Span,
        diagnose: bool,
    ) -> Result<Option<IteratorProtocolTarget>, ()> {
        let Some(iterator) = self.zero_arg_operator_call(
            scope,
            statement.map(IncDecSite::Statement),
            iterable_ty,
            "iterator",
            span,
            diagnose.then_some(span),
        )?
        else {
            return Ok(None);
        };
        let iter_ty = iterator.ret();
        let Some(has_next) = self.zero_arg_operator_call(
            scope,
            statement.map(IncDecSite::Statement),
            iter_ty,
            "hasNext",
            span,
            diagnose.then_some(span),
        )?
        else {
            return Ok(None);
        };
        if has_next.ret() != Ty::Boolean {
            if diagnose {
                self.diags.error(
                    span,
                    format!(
                        "the 'iterator().hasNext()' function of the loop range must return \
                         'Boolean', but returns '{}'.",
                        has_next.ret().source_name()
                    ),
                );
            }
            return Err(());
        }
        let Some(next) = self.zero_arg_operator_call(
            scope,
            statement.map(IncDecSite::Statement),
            iter_ty,
            "next",
            span,
            diagnose.then_some(span),
        )?
        else {
            return Ok(None);
        };
        let elem_ty = next.ret();
        Ok(Some(IteratorProtocolTarget {
            iterator: Box::new(iterator),
            has_next: Box::new(has_next),
            next: Box::new(next),
            iter_ty,
            elem_ty,
        }))
    }

    pub(super) fn record_iterator_protocol(
        &mut self,
        scope: &CheckerScope<'_>,
        statement: Option<StmtId>,
        iterable: ExprId,
        iterable_ty: Ty,
    ) -> Result<Option<Ty>, ()> {
        let diagnose = statement.is_some();
        let Some(target) = self.iterator_protocol_target(
            scope,
            statement,
            iterable_ty,
            self.span(iterable),
            diagnose,
        )?
        else {
            return Ok(None);
        };
        let elem = target.elem_ty;
        self.iterator_protocols.insert(iterable, target);
        Ok(Some(elem))
    }

    /// Select the member plan of every well-known progression class, once per checked file. A
    /// counted loop may read any of them: its iterable's own class, a class `step`/`reversed` is
    /// applied to, or the most precise class of a `val` it reads, whatever type the iterable
    /// expression itself was given (`break downTo 1u` is typed `Nothing`).
    pub(super) fn record_progression_plans(&mut self) {
        for class in wk::progression_classes() {
            self.record_progression_plan(Ty::obj_name(class));
        }
    }

    /// Select `first`, `last` and `step` on a `kotlin.ranges` progression class, once per class.
    /// A member the class's declarations do not publish leaves the class without a plan, and a
    /// counted loop that needs one is then rejected by the checker rather than iterated.
    fn record_progression_plan(&mut self, ty: Ty) {
        let ty = ty.platform_lower_bound().non_null();
        if !ty.type_args().is_empty() || self.progression_plans.members.contains_key(&ty) {
            return;
        }
        let Some(class) = ty.obj_internal().and_then(wk::progression_class) else {
            return;
        };
        let resolver = self.resolver();
        let member = |name| {
            let selected = resolver.select_member_property(ty, name)?;
            Some(ProgressionMember {
                property: selected.property?.getter.external_property_identity?,
                ty: selected.ty,
            })
        };
        let (Some(first), Some(last), Some(step)) =
            (member("first"), member("last"), member("step"))
        else {
            return;
        };
        let element = if first.ty == Ty::Char {
            step.ty
        } else {
            first.ty
        };
        if let Some(last_element) = self.progression_last_element(element, step.ty) {
            self.progression_plans
                .last_elements
                .insert(ty, last_element);
        }
        let compare = if first.ty.is_unsigned() {
            match self.unsigned_loop_compare(first.ty) {
                Some(compare) => Some(compare),
                None => return,
            }
        } else {
            None
        };
        self.progression_plans.members.insert(
            ty,
            ProgressionPlan {
                class,
                first,
                last,
                step,
                compare,
            },
        );
    }

    /// The `kotlin.internal.getProgressionLastElement` overload a counted loop calls with
    /// `(first, last, step)` of `element`, `element` and the progression's `step` type, selected by
    /// the ordinary top-level overload selector. It must return `element`.
    fn progression_last_element(&self, element: Ty, step: Ty) -> Option<RuntimeFunction> {
        let function = self.select_runtime_function(
            wk::kotlin_internal_package(),
            wk::PROGRESSION_LAST_ELEMENT,
            &[element, element, step],
        )?;
        (function.result == element).then_some(function)
    }

    /// The comparison a counted loop orders two `element` values with. kotlinc passes it the value
    /// class's declared underlying carrier, and the provider publishes the declaration that plays
    /// that role (`CompilerIntrinsic::UnsignedCompare`) after checking its full signature. Exactly
    /// one declaration may carry it: a second one on the classpath leaves the plan unselected
    /// rather than letting candidate order decide which one a compiler-generated loop calls.
    fn unsigned_loop_compare(&self, element: Ty) -> Option<RuntimeFunction> {
        let carrier = self
            .fed_source()
            .classifier(element.kotlin_class_internal()?)?
            .value_underlying?;
        let role = crate::libraries::CompilerIntrinsic::UnsignedCompare { carrier };
        let (package, name) =
            crate::libraries::builtin_top_level_realization::runtime_function_declaration(role)?;
        let scope = [package];
        let mut candidates = self
            .resolver_in_scope(&scope)
            .top_level_candidates(name)
            .into_iter()
            .filter(|candidate| candidate.callable.compiler_intrinsic == Some(role));
        let (Some(selected), None) = (candidates.next(), candidates.next()) else {
            return None;
        };
        let callable = selected.callable;
        Some(RuntimeFunction {
            function: callable.external_identity?,
            parameters: callable.params.into_boxed_slice(),
            result: callable.ret,
        })
    }

    /// Selects the stdlib function `name` of `package` a compiler-inserted call with `arguments`
    /// reaches, through the same candidate collection and overload selection as a source call to
    /// it. The call is kotlinc's own, so the declaration's visibility (`@PublishedApi internal`)
    /// does not restrict it; an ambiguous or inapplicable family selects nothing.
    fn select_runtime_function(
        &self,
        package: crate::types::TypeName,
        name: &str,
        arguments: &[Ty],
    ) -> Option<RuntimeFunction> {
        let scope = [package];
        let resolver = self.resolver_in_scope(&scope);
        let arguments = arguments
            .iter()
            .map(|argument| crate::symbol_resolver::CallArgKind::Typed(*argument))
            .collect::<Vec<_>>();
        let (selected, _) = resolver.select_top_level_function_candidates_ignoring_visibility(
            name,
            resolver.top_level_candidates(name),
            &arguments,
            &[],
        )?;
        let callable = selected.callable;
        Some(RuntimeFunction {
            function: callable.external_identity?,
            parameters: callable.params.into_boxed_slice(),
            result: callable.ret,
        })
    }
}
