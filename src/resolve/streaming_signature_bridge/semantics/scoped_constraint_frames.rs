//! The stacks of contextual receivers and builder-inference constraint frames that signature
//! inference keeps while it walks a contextual function literal's body.

use super::*;

/// Builder-inference frames that completed, and what each contextual function argument was
/// completed with.
#[derive(Clone, Debug, Default, PartialEq)]
pub(in crate::resolve::streaming_signature_bridge) struct CompletedFrames {
    /// Completed frames not yet consumed by a function literal of their declaration.
    pending: HashMap<crate::fir::DeclarationId, crate::symbol_resolver::GSigBinds>,
    /// The builder solution each function argument was completed with, by the argument's source
    /// file and span. A postponed-call variable that the body checker recorded for those same
    /// arguments resolves through this solution, and through nothing else.
    by_argument: HashMap<(u32, crate::diag::Span), crate::symbol_resolver::GSigBinds>,
}

/// The builder-inference constraint stores of one signature evaluation.
#[derive(Default)]
pub(in crate::resolve::streaming_signature_bridge) struct ScopedConstraintFrames {
    /// Each owner's stack of frame inputs: the declared types whose parameters the frame solves.
    pub(in crate::resolve::streaming_signature_bridge) inputs:
        RefCell<HashMap<crate::fir::DeclarationId, Vec<Vec<Ty>>>>,
    /// Each owner's stack of open frames and the bindings each has collected so far.
    pub(in crate::resolve::streaming_signature_bridge) active:
        RefCell<HashMap<crate::fir::DeclarationId, Vec<crate::symbol_resolver::GSigBinds>>>,
    completed: RefCell<CompletedFrames>,
    checkpoints: RefCell<Vec<ScopedConstraintCheckpoint>>,
}

/// Constraint stores whose mutations belong to one provisional postponed-argument evaluation.
struct ScopedConstraintCheckpoint {
    active: HashMap<crate::fir::DeclarationId, Vec<crate::symbol_resolver::GSigBinds>>,
    completed: CompletedFrames,
}

impl ScopedConstraintCheckpoint {
    fn capture(
        active: &HashMap<crate::fir::DeclarationId, Vec<crate::symbol_resolver::GSigBinds>>,
        completed: &CompletedFrames,
    ) -> Self {
        Self {
            active: active.clone(),
            completed: completed.clone(),
        }
    }

    fn restore(
        self,
        active: &mut HashMap<crate::fir::DeclarationId, Vec<crate::symbol_resolver::GSigBinds>>,
        completed: &mut CompletedFrames,
    ) {
        *active = self.active;
        *completed = self.completed;
    }
}

impl ProductionSignatureSemantics<'_> {
    /// Consume the frames completed for `function`'s declaration as this literal's solution, and
    /// keep that solution for the argument.
    pub(super) fn complete_contextual_function(
        &self,
        function: crate::fir::ContextualFunctionSite,
    ) -> crate::symbol_resolver::GSigBinds {
        let mut completed = self.scoped_frames.completed.borrow_mut();
        let bindings = completed
            .pending
            .remove(&function.declaration)
            .unwrap_or_default();
        if let (false, Some(crate::fir::Origin::Source { file, span })) = (
            bindings.is_empty(),
            self.headers.signature_origins.get(function.argument),
        ) {
            completed
                .by_argument
                .insert((file.raw(), span), bindings.clone());
        }
        bindings
    }

    /// The solution signature inference reached for the postponed call that owns `variable`.
    /// `None` when the variable is not a postponed-call variable, or no function argument the body
    /// checker analysed under it was completed here.
    pub(in crate::resolve::streaming_signature_bridge) fn completed_postponed_variable(
        &self,
        variable: crate::types::TyParamName,
    ) -> Option<Ty> {
        let origin = crate::types::postponed_variable_origin(variable)?;
        let completed = self.scoped_frames.completed.borrow();
        origin.arguments.iter().find_map(|argument| {
            completed
                .by_argument
                .get(&(origin.file, *argument))?
                .get(&origin.formal)
                .copied()
        })
    }

    /// `ty` with every postponed-call variable replaced by its call's completed solution, or `None`
    /// while one of them has none. A declaration a builder lambda published before its call was
    /// solved (a retained capture) is typed by this, never by a variable or its bound.
    pub(in crate::resolve::streaming_signature_bridge) fn complete_postponed_variables(
        &self,
        ty: Ty,
    ) -> Option<Ty> {
        let unsolved = std::cell::Cell::new(false);
        let completed = crate::types::ty_map_symbols(ty, &|symbol, bound| match symbol.kind() {
            crate::types::TyKind::Var(variable, _) => self
                .completed_postponed_variable(variable)
                .unwrap_or_else(|| {
                    unsolved.set(true);
                    symbol
                }),
            _ => symbol.with_symbol_bound(bound),
        });
        (!unsolved.get()).then_some(completed)
    }

    fn begin_scoped_constraint_transaction(&self) {
        let checkpoint = ScopedConstraintCheckpoint::capture(
            &self.scoped_frames.active.borrow(),
            &self.scoped_frames.completed.borrow(),
        );
        self.scoped_frames.checkpoints.borrow_mut().push(checkpoint);
    }

    fn commit_scoped_constraint_transaction(&self) {
        self.scoped_frames
            .checkpoints
            .borrow_mut()
            .pop()
            .expect("a committed postponed-argument transaction must have a checkpoint");
    }

    fn rollback_scoped_constraint_transaction(&self) {
        let checkpoint = self
            .scoped_frames
            .checkpoints
            .borrow_mut()
            .pop()
            .expect("a rolled-back postponed-argument transaction must have a checkpoint");
        checkpoint.restore(
            &mut self.scoped_frames.active.borrow_mut(),
            &mut self.scoped_frames.completed.borrow_mut(),
        );
    }

    pub(in crate::resolve::streaming_signature_bridge) fn active_scoped_constraint_frame(
        &self,
        declaration: crate::fir::DeclarationId,
    ) -> Option<(crate::fir::DeclarationId, usize)> {
        let constraints = self.scoped_frames.active.borrow();
        let inputs = self.scoped_frames.inputs.borrow();
        let mut current = Some(declaration);
        while let Some(declaration) = current {
            if let (Some(constraint_stack), Some(input_stack)) =
                (constraints.get(&declaration), inputs.get(&declaration))
            {
                if let Some(index) =
                    input_stack
                        .iter()
                        .enumerate()
                        .rev()
                        .find_map(|(index, inputs)| {
                            (constraint_stack.get(index).is_some()
                                && inputs.iter().any(|input| input.mentions_ty_param()))
                            .then_some(index)
                        })
                {
                    return Some((declaration, index));
                }
            }
            current = self.declaration_semantic_parent(declaration);
        }
        None
    }

    pub(super) fn push_scoped_receiver(
        &self,
        declaration: crate::fir::DeclarationId,
        receiver: Ty,
        context_parameter: bool,
    ) {
        self.scoped_receivers
            .borrow_mut()
            .entry(declaration)
            .or_default()
            .push(if context_parameter {
                SignatureImplicitReceiver::context_parameter(receiver)
            } else {
                SignatureImplicitReceiver::ordinary(receiver)
            });
    }

    pub(super) fn pop_scoped_receiver(&self, declaration: crate::fir::DeclarationId) {
        let mut receivers = self.scoped_receivers.borrow_mut();
        let remove = receivers.get_mut(&declaration).is_some_and(|stack| {
            stack.pop();
            stack.is_empty()
        });
        if remove {
            receivers.remove(&declaration);
        }
    }

    pub(super) fn push_scoped_constraint_frame(
        &self,
        declaration: crate::fir::DeclarationId,
        inputs: Vec<Ty>,
    ) {
        self.scoped_frames
            .inputs
            .borrow_mut()
            .entry(declaration)
            .or_default()
            .push(inputs);
        self.scoped_frames
            .active
            .borrow_mut()
            .entry(declaration)
            .or_default()
            .push(crate::symbol_resolver::GSigBinds::default());
    }

    pub(super) fn pop_scoped_constraint_frame(&self, declaration: crate::fir::DeclarationId) {
        let mut semantic_ancestors = Vec::new();
        let mut ancestor = self.declaration_semantic_parent(declaration);
        while let Some(current) = ancestor {
            semantic_ancestors.push(current);
            ancestor = self.declaration_semantic_parent(current);
        }
        let mut inputs = self.scoped_frames.inputs.borrow_mut();
        let remove_inputs = inputs.get_mut(&declaration).is_some_and(|stack| {
            stack.pop();
            stack.is_empty()
        });
        if remove_inputs {
            inputs.remove(&declaration);
        }
        drop(inputs);

        let completed = {
            let mut all = self.scoped_frames.active.borrow_mut();
            let (completed, remove) = all
                .get_mut(&declaration)
                .map(|stack| (stack.pop(), stack.is_empty()))
                .unwrap_or((None, false));
            if remove {
                all.remove(&declaration);
            }
            completed
        };
        let Some(completed) = completed else {
            return;
        };
        let mut all = self.scoped_frames.active.borrow_mut();
        let active_parent = all
            .get(&declaration)
            .and_then(|stack| stack.last())
            .is_some()
            .then_some(declaration)
            .or_else(|| {
                semantic_ancestors
                    .into_iter()
                    .find(|ancestor| all.get(ancestor).and_then(|stack| stack.last()).is_some())
            });
        if let Some(parent) = active_parent
            .and_then(|parent| all.get_mut(&parent))
            .and_then(|stack| stack.last_mut())
        {
            Self::merge_scoped_constraints(parent, completed.clone());
            drop(all);
            let mut finished = self.scoped_frames.completed.borrow_mut();
            let target = finished.pending.entry(declaration).or_default();
            Self::merge_scoped_constraints(target, completed);
            return;
        }
        drop(all);
        let mut finished = self.scoped_frames.completed.borrow_mut();
        let target = finished.pending.entry(declaration).or_default();
        Self::merge_scoped_constraints(target, completed);
    }

    pub(in crate::resolve::streaming_signature_bridge) fn record_scoped_argument_constraints(
        &self,
        scope: crate::fir::SignatureScope,
        parameters: &[Ty],
        arguments: &[Ty],
    ) {
        self.record_scoped_call_constraints(scope, parameters, arguments, &[]);
    }

    /// Record arguments against a callee's declared parameters. `callee_formals` are the callee's
    /// own type variables: a relation between one of them and a builder variable
    /// (`wrap(this)` relating `X` to `E`) is solved by the callee's own inference and is not a
    /// bound of the builder variable.
    pub(super) fn record_scoped_call_constraints(
        &self,
        scope: crate::fir::SignatureScope,
        parameters: &[Ty],
        arguments: &[Ty],
        callee_formals: &[crate::types::TyParamName],
    ) {
        let constraint_frame = self.active_scoped_constraint_frame(scope.owner);
        crate::trace_compiler!(
            "signature",
            "scoped argument owner lookup scope={:?} active={constraint_frame:?} parameters={parameters:?} arguments={arguments:?}",
            scope.owner,
        );
        let Some((constraint_owner, constraint_index)) = constraint_frame else {
            return;
        };
        let active_formals = self
            .scoped_frames
            .inputs
            .borrow()
            .get(&constraint_owner)
            .and_then(|stack| stack.get(constraint_index))
            .map(|inputs| {
                let mut formals = crate::fx_hash::HashSet::default();
                for input in inputs {
                    super::super::postponed_calls::collect_type_parameters(*input, &mut formals);
                }
                formals
            })
            .unwrap_or_default();
        if active_formals.is_empty() {
            return;
        }
        let mut all = self.scoped_frames.active.borrow_mut();
        let Some(active) = all
            .get_mut(&constraint_owner)
            .and_then(|stack| stack.get_mut(constraint_index))
        else {
            return;
        };
        crate::trace_compiler!(
            "signature",
            "scoped argument constraints owner={:?} parameters={parameters:?} arguments={arguments:?} before={active:?}",
            constraint_owner,
        );
        let module = crate::module_symbols::ModuleSymbols::for_file(self.table, scope.source.raw());
        let source = crate::symbol_source::CompositeSource::new(vec![
            &module as &dyn crate::symbol_source::SymbolSource,
            &*self.table.libraries as &dyn crate::symbol_source::SymbolSource,
        ]);
        for (&parameter, &argument) in parameters.iter().zip(arguments) {
            let constraints =
                crate::symbol_resolver::collect_assignability_constraints_from_symbols(
                    &source, parameter, argument,
                );
            let mut inferred = constraints.lower;
            for (formal, upper) in constraints.upper {
                for actual in upper {
                    inferred
                        .entry(crate::types::TyParamName::new(&formal))
                        .and_modify(|known| {
                            *known = crate::symbol_resolver::merge_inferred_ty(Some(*known), actual)
                        })
                        .or_insert(actual);
                }
            }
            inferred.retain(|formal, actual| {
                active_formals.contains(formal.as_str())
                    && !crate::types::ty_mentions_param(*actual, callee_formals)
            });
            Self::merge_scoped_constraints_from_symbols(&source, active, inferred);
        }
        crate::trace_compiler!(
            "signature",
            "scoped argument constraints owner={:?} after={active:?}",
            constraint_owner,
        );
    }
}

impl crate::fir::PostponedArgumentTransaction for ProductionSignatureSemantics<'_> {
    fn begin_postponed_argument_transaction(&self) {
        self.begin_scoped_constraint_transaction();
    }

    fn commit_postponed_argument_transaction(&self) {
        self.commit_scoped_constraint_transaction();
    }

    fn rollback_postponed_argument_transaction(&self) {
        self.rollback_scoped_constraint_transaction();
    }

    fn constrain_expected_value(
        &self,
        scope: crate::fir::SignatureScope,
        expected: crate::fir::ResolvedTy,
        actual: crate::fir::ResolvedTy,
    ) {
        self.record_expected_value_constraints(scope, expected.get(), actual.get());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_postponed_probe_restores_active_parent_constraints() {
        let parent = crate::fir::DeclarationId::from_raw(1);
        let child = crate::fir::DeclarationId::from_raw(2);
        let mut parent_constraints = crate::symbol_resolver::GSigBinds::default();
        parent_constraints.insert("T".into(), Ty::String);
        let mut active =
            crate::fx_hash::HashMap::from_iter([(parent, vec![parent_constraints.clone()])]);
        let completed = CompletedFrames::default();
        let active_before = active.clone();
        let completed_before = completed.clone();
        let checkpoint = ScopedConstraintCheckpoint::capture(&active, &completed);

        active
            .get_mut(&parent)
            .unwrap()
            .last_mut()
            .unwrap()
            .insert("Leaked".into(), Ty::Int);
        let mut completed_after_failure = completed;
        completed_after_failure
            .pending
            .insert(child, parent_constraints.clone());
        completed_after_failure
            .by_argument
            .insert((0, crate::diag::Span::new(4, 9)), parent_constraints);

        checkpoint.restore(&mut active, &mut completed_after_failure);

        assert_eq!(active, active_before);
        assert_eq!(completed_after_failure, completed_before);
    }

    #[test]
    fn a_failed_value_flow_probe_does_not_bind_the_selected_builder_candidate() {
        let builder = crate::fir::DeclarationId::from_raw(1);
        let mut active = crate::fx_hash::HashMap::from_iter([(builder, vec![Default::default()])]);
        let mut completed = CompletedFrames::default();
        let checkpoint = ScopedConstraintCheckpoint::capture(&active, &completed);

        // A rejected postponed candidate can evaluate a typed local or invoked function value
        // before its later expression fails. That provisional value flow must not constrain the
        // builder frame used by the candidate that is ultimately selected.
        active
            .get_mut(&builder)
            .unwrap()
            .last_mut()
            .unwrap()
            .insert("E".into(), Ty::String);
        checkpoint.restore(&mut active, &mut completed);
        active
            .get_mut(&builder)
            .unwrap()
            .last_mut()
            .unwrap()
            .insert("E".into(), Ty::Int);

        assert_eq!(
            active,
            crate::fx_hash::HashMap::from_iter([(
                builder,
                vec![crate::symbol_resolver::GSigBinds::from_iter([(
                    "E".into(),
                    Ty::Int,
                )])],
            )])
        );
        assert_eq!(completed, CompletedFrames::default());
    }
}
