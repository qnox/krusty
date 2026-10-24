//! Checking annotation argument values and their contextual array syntax.
//! Constructor selection and generic inference remain ordinary checked-call operations.

use super::*;

type AnnotationElements = Vec<(String, Ty)>;
type AnnotationShape = (AnnotationElements, ParamList, AnnotationParameterPolicy);

impl Checker<'_> {
    pub(super) fn applied_annotation_shape(&self, owner: Ty) -> Option<AnnotationShape> {
        let classifier = self.resolver().classifier(owner.kotlin_class_internal()?)?;
        let application = classifier.annotation_application()?;
        let bindings = crate::symbol_resolver::classifier_bindings(&classifier, owner);
        let mut parameters = application.parameters;
        for parameter in &mut parameters.types {
            *parameter = crate::types::ty_subst_keep_unbound(*parameter, &bindings);
        }
        let elements = parameters
            .names
            .iter()
            .cloned()
            .zip(parameters.types.iter().copied())
            .collect();
        Some((elements, parameters, application.policy))
    }

    pub(super) fn annotation_shape(&self, internal: TypeName) -> Option<AnnotationShape> {
        let classifier = self.resolver().classifier(internal)?;
        let application = classifier.annotation_application()?;
        let elements = application
            .parameters
            .names
            .iter()
            .cloned()
            .zip(application.parameters.types.iter().copied())
            .collect();
        Some((elements, application.parameters, application.policy))
    }

    pub(super) fn annotation_argument_names(
        &self,
        arguments: &[ExprId],
        nested_call: Option<ExprId>,
    ) -> Vec<Option<String>> {
        // A TOP-LEVEL annotation's labels are recorded per argument in `annotation_arg_names`. A
        // NESTED annotation (`@Outer(Inner(b = "BB", a = "AA"))`) is parsed as an ordinary call, so
        // its labels live in the call's own `call_arg_names` instead. Reading only the former bound
        // every nested argument positionally and silently swapped the values.
        match nested_call {
            // A nested annotation's labels are the CALL's, positionally parallel to its arguments.
            // An unlabelled call records no names at all, which is all-positional — never a reason
            // to consult the annotation map, whose keys are direct `@Ann(...)` arguments.
            Some(call) => {
                let names = self.file.call_arg_names.get(&call.0);
                (0..arguments.len())
                    .map(|position| names.and_then(|names| names.get(position).cloned().flatten()))
                    .collect::<Vec<_>>()
            }
            None => arguments
                .iter()
                .map(|argument| self.file.annotation_arg_names.get(&argument.0).cloned())
                .collect::<Vec<_>>(),
        }
    }

    pub(super) fn annotation_argument_parameter_indices(
        &self,
        parameters: &ParamList,
        arguments: &[ExprId],
        argument_names: &[Option<String>],
    ) -> Result<Vec<usize>, CallArgMappingFailure> {
        let signature = CallSig {
            param_names: parameters.names.clone(),
            param_defaults: parameters.defaults,
            required: required_arity(parameters.names.len(), &parameters.defaults),
            vararg: parameters.vararg.is_some(),
            vararg_index: parameters.vararg,
            ..CallSig::default()
        };
        call_argument_parameter_indices_result(
            arguments.len(),
            parameters.names.len(),
            Some(argument_names),
            false,
            &signature,
        )
    }

    fn report_annotation_mapping_failure(
        &mut self,
        application_span: Span,
        arguments: &[ExprId],
        failure: CallArgMappingFailure,
    ) {
        for error in failure.errors {
            let argument = match &error {
                CallArgMappingError::NoParameterNamed { argument, .. }
                | CallArgMappingError::AlreadyPassed { argument }
                | CallArgMappingError::PositionalAfterNamed { argument }
                | CallArgMappingError::TooManyArguments { argument, .. }
                | CallArgMappingError::TrailingLambdaOnVararg { argument } => Some(*argument),
                CallArgMappingError::MissingRequired { .. } => None,
            };
            let span = argument
                .and_then(|index| arguments.get(index).copied())
                .map_or(application_span, |argument| self.span(argument));
            self.diags.error(span, error.to_string());
        }
    }

    pub(super) fn check_annotation_arguments(
        &mut self,
        scope: &CheckerScope<'_>,
        application_span: Span,
        shape: &AnnotationShape,
        arguments: &[ExprId],
        nested_call: Option<ExprId>,
    ) -> bool {
        let (elements, parameters, policy) = shape;
        let mut unsupported = false;
        for &argument in arguments {
            let Expr::UnsupportedAnnotationArgument(kind) = self.file.expr(argument) else {
                continue;
            };
            let description = match kind {
                crate::ast::UnsupportedAnnotationArgument::NestedAnnotation => "nested-annotation",
            };
            self.diags.error(
                self.span(argument),
                format!("{description} annotation argument is not yet supported"),
            );
            unsupported = true;
        }
        if unsupported {
            return false;
        }
        let argument_names = self.annotation_argument_names(arguments, nested_call);
        let parameter_indices = match self.annotation_argument_parameter_indices(
            parameters,
            arguments,
            &argument_names,
        ) {
            Ok(indices) => indices,
            Err(failure) => {
                self.report_annotation_mapping_failure(application_span, arguments, failure);
                return false;
            }
        };
        let mut forbidden_positional = false;
        for ((&argument, &index), name) in arguments
            .iter()
            .zip(&parameter_indices)
            .zip(&argument_names)
        {
            let permitted = match policy.positional {
                AnnotationPositionalPolicy::Constructor => true,
                AnnotationPositionalPolicy::NamedOnly => false,
                AnnotationPositionalPolicy::Value | AnnotationPositionalPolicy::ValueVararg => {
                    index == 0
                }
            };
            if name.is_none() && !permitted {
                self.diags.error(
                    self.span(argument),
                    "only named arguments are available for Java annotations.".to_string(),
                );
                forbidden_positional = true;
            }
        }
        if forbidden_positional {
            return false;
        }
        for ((&argument, index), name) in
            arguments.iter().zip(parameter_indices).zip(&argument_names)
        {
            let Some((_, declared)) = elements.get(index) else {
                self.diags.error(
                    self.span(argument),
                    "annotation parameter metadata does not match its element declarations"
                        .to_string(),
                );
                return false;
            };
            // A vararg element takes its ELEMENT type only when the argument is passed
            // POSITIONALLY: kotlinc accepts `@Filt("a")` but rejects `@Filt(value = "a")`, where
            // naming the parameter demands the array itself.
            let named = name.is_some();
            let expected =
                if parameters.vararg == Some(index) && !named && !self.file.is_spread_arg(argument)
                {
                    declared.array_read_elem().unwrap_or(*declared)
                } else {
                    *declared
                };
            // An annotation array literal has no type independently of its selected element. Check
            // each ELEMENT against that declared element type: otherwise a class literal or enum
            // entry inside one is never resolved, and an element-typed expectation (a vararg element
            // passed positionally) could accept an array and write the wrong constant tag.
            self.check_annotation_argument_value(scope, argument, expected);
        }
        true
    }

    /// Check one annotation argument against its declared type. An array literal has no independent
    /// factory/type, so each element is checked against the selected element type — RECURSIVELY,
    /// because the fold recurses too: a literal one level deeper (`xs = [[1, 2]]`) would otherwise
    /// reach the fold unchecked and write an array where a scalar element belongs.
    pub(super) fn check_annotation_argument_value(
        &mut self,
        scope: &CheckerScope<'_>,
        argument: ExprId,
        expected: Ty,
    ) {
        if let Expr::AnnotationArrayLiteral(elements) = self.file.expr(argument).clone() {
            let Some(element_ty) = expected.non_null().array_elem() else {
                self.expect_assignable(
                    expected,
                    Ty::array(Ty::obj("kotlin/Any")),
                    self.span(argument),
                    "annotation argument",
                );
                return;
            };
            for element in elements {
                self.check_annotation_argument_value(scope, element, element_ty);
            }
            self.set(argument, expected);
            return;
        }
        let actual = self.check_annotation_value_expression(scope, argument, expected);
        self.expect_assignable(expected, actual, self.span(argument), "annotation argument");
    }

    fn check_annotation_value_expression(
        &mut self,
        scope: &CheckerScope<'_>,
        expression: ExprId,
        expected: Ty,
    ) -> Ty {
        if let Expr::Call { callee, args } = self.file.expr(expression).clone() {
            if let Some(expected_internal) = expected.kotlin_class_internal() {
                // Some declaration formats publish an application policy without a constructor.
                // That policy must not become an ordinary callable. Constructor-backed calls use
                // the existing selector below, including explicit arguments and result inference.
                let application_only =
                    self.resolver()
                        .classifier(expected_internal)
                        .is_some_and(|class| {
                            class.constructors.is_empty()
                                && class.constructor_named_params(0).is_none()
                        });
                let shape = if application_only {
                    self.annotation_shape(expected_internal)
                } else {
                    None
                };
                if let Some(shape) = shape {
                    let selected = self
                        .qualifier(scope, QualifierInput::Expression(callee))
                        .ok();
                    if selected == Some(ResolvedQualifier::Classifier(expected_internal)) {
                        if self.check_annotation_arguments(
                            scope,
                            self.span(expression),
                            &shape,
                            &args,
                            Some(expression),
                        ) {
                            return self.set(expression, Ty::obj_name(expected_internal));
                        }
                        return Ty::Error;
                    }
                }
            }
        }
        self.expr_expected(scope, expression, expected)
    }

    /// A fixed declaration parameter supplies literal context before overload selection. A
    /// symbolic parameter keeps its inference inputs, even when the constructor's result has an
    /// expected application. No caller may reconstruct declaration context from that result.
    pub(super) fn annotation_array_declaration_context(
        &mut self,
        scope: &CheckerScope<'_>,
        argument: ExprId,
        declared: Ty,
    ) -> Option<Ty> {
        if !matches!(self.file.expr(argument), Expr::AnnotationArrayLiteral(_))
            || declared.non_null().array_elem().is_none()
            || declared.mentions_ty_param()
        {
            return None;
        }
        self.check_annotation_argument_value(scope, argument, declared);
        Some(self.expr_types[argument.0 as usize])
    }

    /// A literal used as an ordinary constructor argument contributes its inferred array type.
    /// Unlike a direct annotation slot, its nested generic calls do not receive the enclosing
    /// constructor result's expectation. Primitive literals need their declared scalar width;
    /// an empty literal has no element evidence until selection supplies an array.
    pub(super) fn check_annotation_array_expression(
        &mut self,
        scope: &CheckerScope<'_>,
        expression: ExprId,
        elements: &[ExprId],
        expected: Option<Ty>,
    ) -> Ty {
        let expected = expected.filter(|ty| *ty != Ty::Error).map(Ty::non_null);
        let array = expected.filter(|ty| ty.array_elem().is_some());
        if elements.is_empty() {
            return self.set(expression, array.unwrap_or(Ty::Error));
        }
        let primitive_element = array
            .filter(|ty| !ty.is_reference_array())
            .and_then(Ty::array_elem);
        let synthetic = match primitive_element {
            Some(element) => crate::synthetics::by_kind(
                crate::synthetics::SyntheticKind::PrimitiveVararg(element),
            ),
            None => crate::synthetics::lookup("arrayOf"),
        }
        .expect("annotation arrays use a declared compiler array operation");
        let ty = self
            .check_array_builtin(
                scope,
                expression,
                synthetic,
                elements,
                self.span(expression),
                primitive_element,
            )
            .expect("vararg array operations publish their checked result");
        self.set(expression, ty)
    }

    /// Only absence of elements or scalar-width context leaves an array probe provisional.
    /// Rechecking a nonempty reference array would replace its actual inference evidence with
    /// the selected parameter and accept contradictory nested constructor arguments.
    pub(super) fn contextual_annotation_array_argument(
        &mut self,
        scope: &CheckerScope<'_>,
        argument: ExprId,
        actual: Ty,
        expected: Ty,
    ) -> Option<Ty> {
        if actual == expected {
            return None;
        }
        let Expr::AnnotationArrayLiteral(elements) = self.file.expr(argument).clone() else {
            return None;
        };
        let array = expected.non_null();
        (array.array_elem().is_some() && (elements.is_empty() || !array.is_reference_array())).then(
            || self.check_annotation_array_expression(scope, argument, &elements, Some(expected)),
        )
    }
}

#[cfg(test)]
mod tests;
