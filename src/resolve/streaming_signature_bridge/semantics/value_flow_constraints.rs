//! Builder-inference constraints from values that flow into a fixed type during signature
//! inference.
//!
//! A call records its arguments against its parameters when it is selected. A value can also flow
//! into a type that no generic call owns: a typed local's declared type, or the parameters and
//! extension receiver of an invoked function value. kotlinc's PCLA adds those relations to the
//! enclosing postponed call's system too, so `build { val c: Chan<String> = this }` solves the
//! builder's variable to `String`. Without them a builder whose only evidence is such a flow
//! publishes the variable's default and the body check then rejects the source.

use super::*;

impl ProductionSignatureSemantics<'_> {
    /// Record `actual <: expected` for a value whose type is fixed by its declaration, not by the
    /// selection of a generic call.
    pub(super) fn record_expected_value_constraints(
        &self,
        scope: crate::fir::SignatureScope,
        expected: Ty,
        actual: Ty,
    ) {
        self.record_scoped_argument_constraints(scope, &[expected], &[actual]);
    }

    /// Record each argument of an invoked function value against its parameter. `explicit_shape`
    /// is the form that passes every context and receiver argument in source
    /// (`action(receiver, value)`, and `receiver.action(value)`, which compact evaluation passes
    /// the same way). Otherwise the implicit prefix comes from the scope: context parameters use
    /// the nearest matching receivers and the extension receiver is the innermost receiver whose
    /// type reaches the declared receiver's classifier, as the body check selects them.
    pub(super) fn record_function_value_invoke_constraints(
        &self,
        scope: crate::fir::SignatureScope,
        signature: &crate::types::FnSig,
        implicit_prefix: usize,
        explicit_shape: bool,
        arguments: &[crate::fir::ResolvedSigCallArgument<'_>],
    ) {
        let arguments = arguments
            .iter()
            .map(|argument| argument.ty.get())
            .collect::<Vec<_>>();
        if explicit_shape {
            self.record_scoped_argument_constraints(scope, &signature.params, &arguments);
            return;
        }
        self.record_scoped_argument_constraints(
            scope,
            &signature.params[implicit_prefix..],
            &arguments,
        );
        if signature.context_count == 0 && !signature.has_receiver {
            return;
        }
        let module = crate::module_symbols::ModuleSymbols::for_file(self.table, scope.source.raw());
        let source = crate::symbol_source::CompositeSource::new(vec![
            &module as &dyn crate::symbol_source::SymbolSource,
            &*self.table.libraries as &dyn crate::symbol_source::SymbolSource,
        ]);
        let oracle = crate::symbol_resolver::SourceOracle(&source);
        let receivers = self.implicit_receivers(scope);
        let active_variables = self.active_postponed_type_variables(scope);
        let context_parameters = &signature.params[..signature.context_count];
        let context_arguments = context_parameters
            .iter()
            .map(|expected| {
                receivers.iter().copied().find(|receiver| {
                    !crate::types::ty_mentions_param(*receiver, &active_variables)
                        && crate::resolve::context_argument_types(
                            &[*receiver],
                            &[*expected],
                            &oracle,
                        )
                        .is_some()
                })
            })
            .collect::<Option<Vec<_>>>();
        if let Some(arguments) = context_arguments {
            self.record_scoped_argument_constraints(scope, context_parameters, &arguments);
        }
        if !signature.has_receiver {
            return;
        }
        let Some(&declared_receiver) = signature.params.get(signature.context_count) else {
            return;
        };
        let receiver = receivers.into_iter().find(|receiver| {
            crate::assignable::applied_supertype(&oracle, *receiver, declared_receiver.non_null())
                .is_some()
        });
        if let Some(receiver) = receiver {
            self.record_scoped_argument_constraints(scope, &[declared_receiver], &[receiver]);
        }
    }

    /// Record a selected source callable's arguments against its declared parameters, leaving out
    /// relations to the selected normalized callable's own type variables. The callable's result
    /// is instantiated from the same arguments, so `f(wrap(this))` still relates `W<E>` to `f`'s
    /// parameter without re-reading an origin-specific symbol table.
    pub(super) fn record_selected_source_call_constraints(
        &self,
        scope: crate::fir::SignatureScope,
        callable: &crate::libraries::LibraryCallable,
        parameters: &[Ty],
        arguments: &[Ty],
    ) {
        let callee_formals = callable
            .generic_sig
            .as_deref()
            .map(|generic| generic.formals.as_slice())
            .unwrap_or_default();
        self.record_scoped_call_constraints(scope, parameters, arguments, callee_formals);
    }
}
