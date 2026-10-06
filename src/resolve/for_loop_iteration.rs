//! What a `for` loop iterates through, as checked FIR consumes it: the iterator protocol, selected
//! as ordinary operator calls, for a `kotlin.ranges` progression the `first`, `last` and `step`
//! members kotlinc's `ForLoopsLowering` reads instead of iterating, and for a `CharSequence` the
//! `length` and `get` it indexes with.

use crate::ast::{ExprId, StmtId};
use crate::diag::Span;
use crate::fir::{ExternalCallableId, ExternalPropertyId};
use crate::libraries::TypeEnhancement;
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
    pub first: LoopMemberProperty,
    pub last: LoopMemberProperty,
    pub step: LoopMemberProperty,
    /// The comparison a counted loop over unsigned elements orders them with: the declaration
    /// carrying the provider's `UnsignedCompare` role for the element's carrier. `None` for a
    /// signed or `Char` element.
    pub compare: Option<RuntimeFunction>,
}

/// The progression facts a counted loop reads, keyed by progression class. A class's member plan
/// and the `getProgressionLastElement` overload a `step` over it needs are selected independently,
/// so a loop that never steps does not depend on the helper.
#[derive(Clone, Debug, Default)]
pub struct IterationPlans {
    members: HashMap<Ty, ProgressionPlan>,
    /// The overload a stepped progression of the class moves its `last` with
    /// (`getProgressionLastElementByReturnType`), over the element type, or `Int` for a `Char`
    /// progression.
    last_elements: HashMap<Ty, RuntimeFunction>,
    /// `kotlin.CharSequence`'s own `length` and `get`, selected once a loop indexes a
    /// `CharSequence`.
    char_sequence: Option<CharSequenceIndexing>,
    /// The `index` and `value` properties of `kotlin.collections.IndexedValue`, selected once a
    /// loop destructures a `withIndex()` call.
    indexed_value: Option<IndexedValueProperties>,
}

/// The properties a name-based destructuring entry of a `withIndex()` loop may read
/// (`STDLIB_INDEXED_VALUE_GET_INDEX_NAME`, `STDLIB_INDEXED_VALUE_GET_VALUE_NAME`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct IndexedValueProperties {
    pub index: ExternalPropertyId,
    pub value: ExternalPropertyId,
}

/// The members kotlinc's `CharSequenceIterationHandler` indexes a `CharSequence` with: the
/// `length` it compares the index with on every iteration and the `get(Int)` it reads each
/// element with, both declared by `kotlin.CharSequence` itself whatever the receiver's class.
#[derive(Clone, Debug)]
pub struct CharSequenceIndexing {
    pub length: LoopMemberProperty,
    pub get: Box<super::ResolvedCall>,
}

impl IterationPlans {
    /// The selected `first`, `last` and `step` of progression class `class`.
    pub fn plan(&self, class: Ty) -> Option<&ProgressionPlan> {
        self.members.get(&class)
    }

    /// The `getProgressionLastElement` overload a `step` over progression class `class` calls.
    pub fn last_element(&self, class: Ty) -> Option<&RuntimeFunction> {
        self.last_elements.get(&class)
    }

    /// The selected `CharSequence.length` and `CharSequence.get`.
    pub fn char_sequence(&self) -> Option<&CharSequenceIndexing> {
        self.char_sequence.as_ref()
    }

    /// The selected `IndexedValue.index` and `IndexedValue.value`.
    pub fn indexed_value(&self) -> Option<IndexedValueProperties> {
        self.indexed_value
    }
}

impl super::TypeInfo {
    pub fn progression_plan(&self, class: Ty) -> Option<&ProgressionPlan> {
        self.iteration_plans.plan(class)
    }

    pub fn progression_last_element(&self, class: Ty) -> Option<&RuntimeFunction> {
        self.iteration_plans.last_element(class)
    }

    pub fn char_sequence_indexing(&self) -> Option<&CharSequenceIndexing> {
        self.iteration_plans.char_sequence()
    }

    pub fn indexed_value_properties(&self) -> Option<IndexedValueProperties> {
        self.iteration_plans.indexed_value()
    }
}

/// Where a loop's `iterator()` and `next()` results carry kotlinc's `EnhancedNullability`, as their
/// declared results substitute the iterable's marks (`hashMap.entries` iterates marked entries).
#[derive(Clone, Debug, Default)]
pub struct ProtocolEnhancement {
    pub iterator: TypeEnhancement,
    pub element: TypeEnhancement,
}

/// A stdlib function a counted loop calls on its own, selected from the provider's declarations.
#[derive(Clone, Debug, PartialEq)]
pub struct RuntimeFunction {
    pub function: ExternalCallableId,
    pub parameters: Box<[Ty]>,
    pub result: Ty,
}

/// A selected member property a counted loop reads on a receiver of the declaring class.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LoopMemberProperty {
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
            enhancement: ProtocolEnhancement::default(),
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
        let Some(mut target) = self.iterator_protocol_target(
            scope,
            statement,
            iterable_ty,
            self.span(iterable),
            diagnose,
        )?
        else {
            return Ok(None);
        };
        let iterable_marks = self.value_enhancement(iterable);
        let iterator = self.call_result_enhancement(
            &target.iterator,
            Some((iterable_ty, iterable_marks)),
            None,
        );
        let element = self.call_result_enhancement(
            &target.next,
            Some((target.iter_ty, iterator.clone())),
            None,
        );
        target.enhancement = ProtocolEnhancement { iterator, element };
        let elem = target.elem_ty;
        self.iterator_protocols.insert(iterable, target);
        Ok(Some(elem))
    }

    /// What a `for (… in iterable)` loop may iterate through besides the iterable's own iterator:
    /// the members of a progression, and the receiver of a destructured `withIndex()`.
    pub(super) fn record_iteration_plans(
        &mut self,
        scope: &CheckerScope<'_>,
        statement: StmtId,
        iterable: ExprId,
    ) {
        self.record_progression_plans();
        self.record_with_index_iteration(scope, statement, iterable);
    }

    /// kotlinc's `WithIndexHandler` iterates the receiver of a `withIndex()` call whose
    /// `IndexedValue` the loop destructures in its header. An `Iterable` or a `Sequence` receiver
    /// is iterated through the `iterator()` of the class the declaration receives it as
    /// (`DefaultIterableHandler`, `DefaultSequenceHandler`), selected here on that receiver type
    /// and recorded for the receiver expression.
    fn record_with_index_iteration(
        &mut self,
        scope: &CheckerScope<'_>,
        statement: StmtId,
        iterable: ExprId,
    ) {
        if !self.file.destructuring.loops.contains_key(&statement) {
            return;
        }
        let Some(super::ResolvedCall::Extension(call)) = self.resolved_calls.get(&iterable) else {
            return;
        };
        if call.callable.compiler_intrinsic != Some(crate::libraries::CompilerIntrinsic::WithIndex)
        {
            return;
        }
        let receiver_ty = call.receiver;
        self.record_indexed_value_properties();
        let class = receiver_ty.obj_internal();
        if class == Some(wk::char_sequence()) {
            self.record_char_sequence_indexing(receiver_ty);
            return;
        }
        if class != Some(wk::iterable()) && class != Some(wk::sequence()) {
            return;
        }
        let crate::ast::Expr::Call { callee, .. } = self.file.expr(iterable) else {
            return;
        };
        let crate::ast::Expr::Member { receiver, .. } = self.file.expr(*callee) else {
            return;
        };
        let receiver = *receiver;
        crate::trace_compiler!(
            "resolve",
            "withIndex loop receiver {receiver:?} iterated as {receiver_ty:?}",
        );
        if let Ok(Some(target)) =
            self.iterator_protocol_target(scope, None, receiver_ty, self.span(receiver), false)
        {
            self.iterator_protocols.insert(receiver, target);
        }
    }

    /// Select `index` and `value` on `kotlin.collections.IndexedValue`, once per checked file, so a
    /// name-based entry is matched by the property its read selected rather than by spelling.
    fn record_indexed_value_properties(&mut self) {
        if self.iteration_plans.indexed_value.is_some() {
            return;
        }
        let indexed_value = Ty::obj_name(wk::indexed_value());
        let resolver = self.resolver();
        let property = |name| {
            resolver
                .select_member_property(indexed_value, name)?
                .property?
                .getter
                .external_property_identity
        };
        if let (Some(index), Some(value)) = (
            property(wk::INDEXED_VALUE_INDEX),
            property(wk::INDEXED_VALUE_VALUE),
        ) {
            self.iteration_plans.indexed_value = Some(IndexedValueProperties { index, value });
        }
    }

    /// Select `length` and `get(Int)` on `kotlin.CharSequence` (`char_sequence`), once per checked
    /// file: the declarations kotlinc's `CharSequenceIterationHandler` takes from the class itself.
    /// A family that selects no single member leaves the loop iterating `IndexedValue`s.
    fn record_char_sequence_indexing(&mut self, char_sequence: Ty) {
        if self.iteration_plans.char_sequence.is_some() {
            return;
        }
        let Some(length) = self
            .resolver()
            .select_member_property(char_sequence, wk::CHAR_SEQUENCE_LENGTH)
            .and_then(|selected| {
                Some(LoopMemberProperty {
                    property: selected.property?.getter.external_property_identity?,
                    ty: selected.ty,
                })
            })
            .filter(|length| length.ty == Ty::Int)
        else {
            return;
        };
        let Some(get) = self.char_sequence_get(char_sequence) else {
            return;
        };
        crate::trace_compiler!(
            "resolve",
            "CharSequence indexed through {length:?} and {get:?}"
        );
        self.iteration_plans.char_sequence = Some(CharSequenceIndexing {
            length,
            get: Box::new(get),
        });
    }

    /// The member `operator fun get(index: Int): Char` of `kotlin.CharSequence`, selected among the
    /// class's members by ordinary overload selection over an `Int` argument.
    fn char_sequence_get(&self, char_sequence: Ty) -> Option<super::ResolvedCall> {
        let name = wk::INDEXED_GET;
        let (mut functions, _) = self
            .stable_receiver_callables(char_sequence, name)
            .into_parts();
        functions
            .overloads
            .retain(|candidate| candidate.kind == crate::libraries::FnKind::Member);
        let callables = crate::libraries::Callables::Functions(functions);
        let crate::symbol_resolver::CandidateSelection::Selected((selected, params, ret)) = self
            .resolver()
            .select_receiver_function_with_params_tracking(
                char_sequence,
                name,
                &[crate::symbol_resolver::CallArgKind::Typed(Ty::Int)],
                &[],
                &callables,
                None,
            )
        else {
            return None;
        };
        if !selected.flags.operator || selected.context_count != 0 || ret != Ty::Char {
            return None;
        }
        let mut member = selected.member_with_return(ret);
        member.params = params;
        Some(super::ResolvedCall::Member(
            crate::symbol_resolver::ResolvedMember {
                receiver: char_sequence,
                physical_params: selected.callable.physical_params.clone(),
                context_args: Vec::new(),
                ret,
                member,
                projected_return_hazard: selected.projected_return_hazard,
                suspend: selected.flags.suspend,
                origin: selected.callable.origin.clone(),
            },
        ))
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
        if !ty.type_args().is_empty() || self.iteration_plans.members.contains_key(&ty) {
            return;
        }
        let Some(class) = ty.obj_internal().and_then(wk::progression_class) else {
            return;
        };
        let resolver = self.resolver();
        let member = |name| {
            let selected = resolver.select_member_property(ty, name)?;
            Some(LoopMemberProperty {
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
            self.iteration_plans.last_elements.insert(ty, last_element);
        }
        let compare = if first.ty.is_unsigned() {
            match self.unsigned_loop_compare(first.ty) {
                Some(compare) => Some(compare),
                None => return,
            }
        } else {
            None
        };
        self.iteration_plans.members.insert(
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
