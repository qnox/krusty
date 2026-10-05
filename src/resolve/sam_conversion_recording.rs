//! The suspension of a value selected for functional-interface conversion.
//!
//! The target method's `suspend` bit and the value's own callable view are different facts. A
//! non-suspend property reference or fun interface adapted to a suspend method is stored as that
//! value's `FunctionN`; the continuation stays on the interface method.

use crate::ast::{Expr, ExprId};
use crate::types::{Ty, TypeName};

use super::*;

impl Checker<'_> {
    /// Check the language-defined single-argument construction of a selected classifier. `Ok(None)`
    /// means the classifier is not a SAM or the operand is inapplicable. `Err(())` means a provider
    /// claimed a SAM classifier but failed to publish a complete semantic contract; callers must
    /// fail closed instead of retrying an ordinary construction path.
    pub(super) fn check_selected_sam_constructor(
        &mut self,
        scope: &CheckerScope<'_>,
        call: ExprId,
        internal: TypeName,
        arguments: &[ExprId],
        label: Option<&str>,
        expected: Option<Ty>,
    ) -> Result<Option<Ty>, ()> {
        let [argument] = arguments else {
            return Ok(None);
        };
        let contextual_target = expected
            .map(Ty::non_null)
            .filter(|target| target.obj_internal() == Some(internal));
        let explicit_arguments = self.resolved_explicit_type_args(scope, call);
        let explicit_target = (!explicit_arguments.is_empty())
            .then(|| Ty::obj_args_name(internal, &explicit_arguments));
        let fixed_target = explicit_target.or(contextual_target);
        let target = fixed_target.unwrap_or_else(|| Ty::obj_name(internal));
        let Some(signature) = self.semantic_sam_signature(target) else {
            if self
                .fed_source()
                .classifier(internal)
                .is_some_and(|classifier| classifier.sam_eligible)
            {
                self.diags.error(
                    self.span(call),
                    "functional interface has no complete single-abstract-method contract"
                        .to_string(),
                );
                return Err(());
            }
            return Ok(None);
        };
        let expected_callable = Ty::fun_with_shape(
            signature.params.clone(),
            signature.ret,
            signature.context_count,
            signature.has_receiver,
            signature.suspend,
        );
        let operand_view =
            self.sam_source_nominal(scope, *argument, self.expr_types[argument.0 as usize]);
        let postpone_lambda_body = expected.is_none()
            && self.postponed_argument_depth != 0
            && matches!(self.file.expr(*argument), Expr::Lambda { .. })
            && matches!(expected_callable, Ty::Fun(shape) if self.contextual_lambda_accepts_function_shape(*argument, shape));
        crate::trace_compiler!(
            "lambda_apply",
            "SAM constructor call={call:?} expected={expected:?} postponed_depth={} postpone_body={postpone_lambda_body}",
            self.postponed_argument_depth,
        );
        let actual = if postpone_lambda_body {
            self.set(*argument, expected_callable)
        } else {
            let checked = self.expr_types[argument.0 as usize];
            self.expression_function_type(scope, *argument, checked)
                .filter(|function| *function == expected_callable)
                .unwrap_or_else(|| {
                    self.check_argument_expected(
                        scope,
                        *argument,
                        expected_callable,
                        signature.has_receiver,
                        label,
                    )
                })
        };
        let selected = if fixed_target.is_some() {
            select_fixed_sam_constructor(&self.fed_source(), signature, target, actual)
        } else {
            select_sam_constructor(&self.fed_source(), signature, actual)
        };
        let Some(selected) = selected else {
            return Ok(None);
        };
        let specialized_callable = Ty::fun_with_shape(
            selected.signature.params.clone(),
            selected.signature.ret,
            selected.signature.context_count,
            selected.signature.has_receiver,
            selected.signature.suspend,
        );
        let Some(source_suspend) =
            self.sam_source_suspend(scope, *argument, operand_view, &selected.signature)
        else {
            self.diags.error(
                self.span(*argument),
                "functional-interface conversion has no selected callable source view".to_string(),
            );
            return Err(());
        };
        let published = if source_suspend || operand_view == Ty::Error {
            specialized_callable
        } else {
            operand_view
        };
        self.set(*argument, published);
        self.expr_lowers.insert(
            call,
            ExprLowering::SamConstructor {
                result: selected.result,
                sam: Box::new(selected.signature),
                source_suspend,
            },
        );
        Ok(Some(self.set(call, selected.result)))
    }

    /// A contextually checked local may carry the target function shape in the expression cache.
    /// Recover its declaration through the already-resolved flow identity, never its spelling.
    fn sam_source_nominal(&self, scope: &CheckerScope<'_>, argument: ExprId, nominal: Ty) -> Ty {
        self.expr_access_path(argument)
            .filter(|path| path.segments.is_empty())
            .and_then(|path| match path.root {
                scope::PathRoot::Value(identity) => Some(identity),
                _ => None,
            })
            .and_then(|identity| self.visible_flow_value(scope, identity))
            .map(|(_, local)| local.declared_ty)
            .filter(|declared| {
                matches!(nominal.non_null(), Ty::Fun(_) | Ty::Error)
                    && !matches!(declared.non_null(), Ty::Fun(_))
            })
            .unwrap_or(nominal)
    }

    /// Suspension of the value adapted to `sam`, as distinct from the target method's suspension.
    ///
    /// A callable reference keeps its original function type after an expected suspend shape
    /// replaces the nominal type. A property reference or fun interface is not a `Ty::Fun`; its
    /// callable view is that classifier's function supertype or its own abstract method. Absence of
    /// an exact source view is invalid: the target method's suspension is not the source value's
    /// identity and must not be substituted for it.
    pub(super) fn sam_source_suspend(
        &self,
        scope: &CheckerScope<'_>,
        argument: ExprId,
        nominal: Ty,
        sam: &crate::symbol_resolver::SamSignature,
    ) -> Option<bool> {
        // Contextual checking may replace a local value's nominal type with the target function
        // shape. Recover its declaration only through the already-resolved value identity; never
        // perform a second spelling lookup after selection.
        let nominal = self.sam_source_nominal(scope, argument, nominal);
        if let Some(Ty::Fun(signature)) = self
            .callable_reference_types
            .get(&argument)
            .copied()
            .map(Ty::non_null)
        {
            if signature.params.len() == sam.params.len() && (!signature.suspend || sam.suspend) {
                return Some(signature.suspend);
            }
        }
        // A lambda has no callable source view before its selected expectation checks the body.
        // Read the exact function shape that check published for this expression; unlike a named
        // value, there is no earlier declaration shape for contextual checking to overwrite.
        if matches!(self.file.expr(argument), Expr::Lambda { .. }) {
            if let Ty::Fun(signature) = self.expr_types[argument.0 as usize].non_null() {
                if signature.params.len() == sam.params.len() && (!signature.suspend || sam.suspend)
                {
                    return Some(signature.suspend);
                }
            }
        }
        if let Some(function) = self.expression_function_type_for_sam(
            scope,
            argument,
            nominal.non_null(),
            sam.params.len(),
            sam.suspend,
        ) {
            return Some(matches!(function, Ty::Fun(signature) if signature.suspend));
        }
        self.semantic_sam_signature(nominal.non_null())
            .map(|source| source.suspend)
    }

    /// Whether `actual` is already the exact applied fun interface `expected`.
    ///
    /// Assignability is the comparison, so a different instantiation, variance, or projection is
    /// not this interface and may still convert. A function supertype on the same value is not a
    /// reason to build a fresh adapter. Lambdas and callable references stay conversions: their
    /// checked type may already be the interface.
    pub(super) fn sam_argument_already_implements(
        &self,
        argument: ExprId,
        actual: Ty,
        expected: Ty,
    ) -> bool {
        if matches!(
            self.file.expr(argument),
            Expr::Lambda { .. } | Expr::CallableRef { .. }
        ) {
            return false;
        }
        if matches!(actual, Ty::Error | Ty::Pending | Ty::Fun(_))
            || actual.mentions_pending()
            || !matches!(expected.non_null(), Ty::Obj(_, _))
        {
            return false;
        }
        self.receiver_is_assignable(actual, expected)
    }

    /// Whether generic inference should read `actual` rather than its function supertype.
    ///
    /// A solved parameter uses the applied interface, so `Worker<TokenA>` does not instantiate
    /// `Worker<TokenB>`. An unsolved `Worker<T>` has no application to compare yet; any
    /// instantiation of that classifier is the value's own constraint.
    pub(super) fn sam_argument_supplies_interface(
        &self,
        argument: ExprId,
        actual: Ty,
        parameter: Ty,
    ) -> bool {
        if parameter.mentions_ty_param() {
            let Some(classifier) = self
                .semantic_sam_signature(parameter)
                .map(|signature| signature.internal)
            else {
                return false;
            };
            return self.sam_argument_instantiates_classifier(argument, actual, classifier);
        }
        self.sam_argument_already_implements(argument, actual, parameter)
    }

    fn sam_argument_instantiates_classifier(
        &self,
        argument: ExprId,
        actual: Ty,
        classifier: TypeName,
    ) -> bool {
        if matches!(
            self.file.expr(argument),
            Expr::Lambda { .. } | Expr::CallableRef { .. }
        ) {
            return false;
        }
        let actual = actual.non_null();
        if matches!(actual, Ty::Error | Ty::Pending | Ty::Fun(_)) || actual.mentions_pending() {
            return false;
        }
        let mut pending = vec![actual];
        let mut seen = Vec::new();
        while let Some(ty) = pending.pop() {
            let ty = ty.non_null();
            if seen.contains(&ty) {
                continue;
            }
            seen.push(ty);
            if let Ty::Obj(name, _) = ty {
                if name == classifier {
                    return true;
                }
            }
            pending.extend(crate::assignable::TypeOracle::direct_supertypes(self, ty));
        }
        false
    }

    pub(super) fn sam_conversion_record(
        &mut self,
        scope: &CheckerScope<'_>,
        argument: ExprId,
        nominal: Ty,
        sam: crate::symbol_resolver::SamSignature,
    ) -> Option<ResolvedSamConversion> {
        let Some(source_suspend) = self.sam_source_suspend(scope, argument, nominal, &sam) else {
            self.diags.error(
                self.span(argument),
                "functional-interface conversion has no selected callable source view".to_string(),
            );
            return None;
        };
        Some(ResolvedSamConversion {
            source_suspend,
            signature: sam,
        })
    }

    /// Record a SAM conversion after overload selection has already admitted the argument. Some
    /// provider call paths contextually type lambdas during candidate selection and therefore do not
    /// revisit [`Self::expect_call_arg`]; this commit hook preserves the same exact target handoff.
    pub(super) fn record_selected_sam_conversion(
        &mut self,
        scope: &CheckerScope<'_>,
        expected: Ty,
        argument: ExprId,
    ) {
        let Some(sam) = self.semantic_sam_signature(expected) else {
            return;
        };
        let convertible = matches!(self.file.expr(argument), Expr::Lambda { .. })
            || self.callable_reference_types.contains_key(&argument)
            || matches!(self.expr_types[argument.0 as usize].non_null(), Ty::Fun(_));
        if convertible {
            let nominal = self.expr_types[argument.0 as usize];
            if let Some(conversion) = self.sam_conversion_record(scope, argument, nominal, sam) {
                self.resolved_sam_conversions.insert(argument, conversion);
            }
        }
    }

    pub(super) fn record_selected_sam_arguments(
        &mut self,
        scope: &CheckerScope<'_>,
        args: &[ExprId],
        params: &[Ty],
    ) {
        for (&argument, &parameter) in args.iter().zip(params) {
            self.record_selected_sam_conversion(scope, parameter, argument);
        }
    }

    /// Expected type of each source argument, using the vararg element when the argument is one
    /// packed value. `Ty::Error` means the mapping is unavailable; that is not evidence the value
    /// already is the target.
    pub(super) fn sam_argument_expected_types(
        &self,
        call: ExprId,
        args: &[ExprId],
        argument_names: Option<&[Option<String>]>,
        params: &[Ty],
        call_sig: &CallSig,
    ) -> Vec<Ty> {
        let Some(parameters) = call_argument_parameter_indices(
            args.len(),
            params.len(),
            argument_names,
            self.file.call_has_trailing_lambda.contains(&call.0),
            call_sig,
        ) else {
            return vec![Ty::Error; args.len()];
        };
        args.iter()
            .enumerate()
            .zip(parameters)
            .map(|((source, &argument), parameter)| {
                let Some(declared) = params.get(parameter).copied() else {
                    return Ty::Error;
                };
                let named = argument_names
                    .and_then(|names| names.get(source))
                    .is_some_and(Option::is_some);
                if call_sig.vararg_index == Some(parameter)
                    && !named
                    && !self.file.is_spread_arg(argument)
                {
                    declared.array_read_elem().unwrap_or(declared)
                } else {
                    declared
                }
            })
            .collect()
    }

    pub(super) fn record_selected_sam_signatures(
        &mut self,
        scope: &CheckerScope<'_>,
        args: &[ExprId],
        signatures: &[Option<crate::symbol_resolver::SamSignature>],
        expected_types: &[Ty],
    ) {
        for ((&argument, signature), &expected) in args.iter().zip(signatures).zip(expected_types) {
            if let Some(signature) = signature {
                let nominal = self.expr_types[argument.0 as usize];
                if self.sam_argument_already_implements(argument, nominal, expected) {
                    continue;
                }
                if let Some(conversion) =
                    self.sam_conversion_record(scope, argument, nominal, signature.clone())
                {
                    self.resolved_sam_conversions.insert(argument, conversion);
                }
            }
        }
    }

    pub(super) fn record_selected_sam_vararg_arguments(
        &mut self,
        scope: &CheckerScope<'_>,
        args: &[ExprId],
        params: &[Ty],
        vararg_index: Option<usize>,
    ) {
        for (index, &argument) in args.iter().enumerate() {
            let Some(mut parameter) = params
                .get(index)
                .copied()
                .or_else(|| vararg_index.and_then(|vararg| params.get(vararg).copied()))
            else {
                continue;
            };
            if vararg_index.is_some_and(|vararg| index >= vararg)
                && !self.file.is_spread_arg(argument)
            {
                parameter = parameter.array_read_elem().unwrap_or(parameter);
            }
            self.record_selected_sam_conversion(scope, parameter, argument);
        }
    }

    pub(super) fn record_resolved_extension_sam_arguments(
        &mut self,
        scope: &CheckerScope<'_>,
        call: ExprId,
        args: &[ExprId],
    ) {
        let selected_parameters = match self.resolved_calls.get(&call) {
            Some(ResolvedCall::Extension(extension)) => {
                Some((extension.params.clone(), extension.vararg_index))
            }
            _ => None,
        };
        if let Some((parameters, vararg_index)) = selected_parameters {
            self.record_selected_sam_vararg_arguments(scope, args, &parameters, vararg_index);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{rc::Rc, sync::Arc};

    use crate::ast::Decl;
    use crate::diag::{DiagnosticKind, Severity, Span};
    use crate::libraries::{LibraryType, ResolvedSymbols, SemanticPlatform, TypeKind};
    use crate::symbol_source::{SymbolNamespace, SymbolSource};

    use super::*;

    fn parsed_probe() -> (Vec<crate::ast::File>, ExprId, crate::diag::DiagSink) {
        let source = "val probe = 1";
        let mut diagnostics = crate::diag::DiagSink::new();
        let tokens = crate::lexer::lex(source, &mut diagnostics);
        let file = crate::parser::parse(source, &tokens, &mut diagnostics);
        let Decl::Property(property) = file.decl(file.decls[0]) else {
            panic!("probe must parse as a property")
        };
        let expression = property.init.expect("probe initializer");
        (vec![file], expression, diagnostics)
    }

    /// One diagnostic as a comparable row.
    type DiagnosticRow<'a> = (
        u32,
        Span,
        Option<Span>,
        Severity,
        DiagnosticKind,
        &'a str,
        Option<crate::diag::DiagnosticIdentity>,
    );

    fn diagnostic_rows(diagnostics: &crate::diag::DiagSink) -> Vec<DiagnosticRow<'_>> {
        diagnostics
            .diags
            .iter()
            .map(|diagnostic| {
                (
                    diagnostic.file,
                    diagnostic.span,
                    diagnostic.editor_span,
                    diagnostic.severity,
                    diagnostic.kind,
                    diagnostic.msg.as_str(),
                    diagnostic.identity,
                )
            })
            .collect()
    }

    fn target_signature() -> crate::symbol_resolver::SamSignature {
        crate::symbol_resolver::SamSignature {
            internal: crate::types::type_name("fixture/Target"),
            method: "apply".into(),
            declaration: Some(crate::symbol_resolver::SamMethodDeclaration::FunctionTypeInvoke),
            params: Vec::new(),
            ret: Ty::Unit,
            declared_params: Vec::new(),
            declared_ret: Ty::Unit,
            context_count: 0,
            has_receiver: false,
            suspend: false,
            overridden_results: Vec::new(),
            kotlin_interface: true,
            parameter_identities: Box::new([]),
        }
    }

    #[test]
    fn missing_selected_source_view_reports_one_exact_diagnostic() {
        let (files, expression, mut diagnostics) = parsed_probe();
        let symbols = super::super::collect_signatures(&files, &mut diagnostics);
        assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
        let mut probe_diagnostics = crate::diag::DiagSink::new();
        let mut checker = super::super::make_checker(
            &files[0],
            0,
            Some(&files),
            &symbols,
            &mut probe_diagnostics,
        );

        let record = checker.sam_conversion_record(
            &super::super::CheckerScope::root(),
            expression,
            Ty::obj("fixture/NotCallable"),
            target_signature(),
        );
        assert!(record.is_none());
        drop(checker);
        assert_eq!(
            diagnostic_rows(&probe_diagnostics),
            vec![(
                0,
                Span::new(12, 13),
                None,
                Severity::Error,
                DiagnosticKind::Compiler,
                "functional-interface conversion has no selected callable source view",
                None,
            )]
        );
    }

    struct IncompleteSamPlatform {
        internal: TypeName,
        classifier: Arc<LibraryType>,
    }

    impl IncompleteSamPlatform {
        fn new() -> Self {
            let internal = crate::types::type_name("fixture/Broken");
            let mut classifier = LibraryType::declaration_header();
            classifier.kind = TypeKind::Interface;
            classifier.is_kotlin = true;
            classifier.sam_eligible = true;
            Self {
                internal,
                classifier: Arc::new(classifier),
            }
        }
    }

    impl SymbolSource for IncompleteSamPlatform {
        fn symbols(&self, namespace: SymbolNamespace, name: &str) -> Rc<ResolvedSymbols> {
            if namespace == SymbolNamespace::Package(crate::types::type_name("fixture"))
                && name == "Broken"
            {
                Rc::new(ResolvedSymbols {
                    classifier_name: Some(self.internal),
                    classifier: Some(self.classifier.clone()),
                    ..ResolvedSymbols::default()
                })
            } else {
                Rc::new(ResolvedSymbols::default())
            }
        }
    }

    impl SemanticPlatform for IncompleteSamPlatform {}

    #[test]
    fn selected_incomplete_provider_sam_fails_closed_with_one_exact_diagnostic() {
        let (files, expression, mut diagnostics) = parsed_probe();
        let platform = IncompleteSamPlatform::new();
        let internal = platform.internal;
        let symbols =
            super::super::collect_signatures_with_cp(&files, Box::new(platform), &mut diagnostics);
        assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
        let mut probe_diagnostics = crate::diag::DiagSink::new();
        let mut checker = super::super::make_checker(
            &files[0],
            0,
            Some(&files),
            &symbols,
            &mut probe_diagnostics,
        );

        assert_eq!(
            checker.check_selected_sam_constructor(
                &super::super::CheckerScope::root(),
                expression,
                internal,
                &[expression],
                None,
                None,
            ),
            Err(()),
        );
        drop(checker);
        assert_eq!(
            diagnostic_rows(&probe_diagnostics),
            vec![(
                0,
                Span::new(12, 13),
                None,
                Severity::Error,
                DiagnosticKind::Compiler,
                "functional interface has no complete single-abstract-method contract",
                None,
            )]
        );
    }
}
