//! Declaration consensus used to type constructor arguments before selection.

use super::*;

/// What `Name(args)` types its arguments against when the name denotes a constructible classifier.
///
/// Both parameter vectors are in SOURCE-ARGUMENT order — labels have already been mapped onto
/// declaration slots — so index `i` always describes the `i`th written argument.
pub(super) struct ConstructionExpectations {
    /// Per source argument: the parameter's semantic type and whether that type is a RECEIVER
    /// function type (`Scope.() -> Unit`, whose lambda binds `this`). `None` where same-arity
    /// constructors disagree, so the argument keeps its expectation-free shape.
    pub(super) params: Vec<Option<(Ty, bool)>>,
    /// The same parameters with the classifier's own type variables still symbolic, where the
    /// declaration provides them. This is the template call-site type arguments substitute into.
    pub(super) symbolic: Vec<Option<Ty>>,
    /// The constructed classifier's type parameters, in declaration order.
    pub(super) type_params: Vec<String>,
    /// Declaration bounds parallel to `type_params`, used when an expected applied supertype
    /// supplies the classifier arguments before a contextual constructor argument is checked.
    pub(super) type_param_bounds: Vec<Vec<Ty>>,
}
impl Checker<'_> {
    /// The constructor shapes each argument of `Name(args)` is typed against, in SOURCE-ARGUMENT
    /// order. Providers normalize every declaration origin into the classifier's constructor list;
    /// ordinary callable mapping handles labels, defaults, varargs, and trailing lambdas once.
    ///
    /// The symbolic vector is the same mapped parameter list before the classifier's type variables
    /// are substituted. A slot is exposed only when every applicable constructor agrees, so this
    /// pre-selection expectation cannot choose an overload.
    pub(super) fn construction_argument_expectations(
        &self,
        internal: TypeName,
        arity: usize,
        arg_names: Option<&[Option<String>]>,
        trailing_lambda: bool,
    ) -> Option<ConstructionExpectations> {
        let classifier = self.resolved_type_name(internal)?;
        let declarations = self.constructor_declarations(internal, &classifier);
        let candidates = declarations
            .iter()
            .filter_map(|constructor| {
                let slots = call_argument_parameter_indices(
                    arity,
                    constructor.params.len(),
                    arg_names,
                    trailing_lambda,
                    &constructor.call_sig,
                )?;
                let symbolic = constructor
                    .generic_sig
                    .as_ref()
                    .filter(|generic| generic.params.len() == constructor.params.len());
                Some(
                    slots
                        .into_iter()
                        .map(|slot| {
                            let parameter = (
                                constructor.params[slot],
                                constructor
                                    .call_sig
                                    .lambda_receiver_params
                                    .get(slot)
                                    .copied()
                                    .unwrap_or(false),
                            );
                            let symbolic = symbolic.map(|generic| generic.params[slot]);
                            (parameter, symbolic)
                        })
                        .collect::<Vec<_>>(),
                )
            })
            .collect::<Vec<_>>();
        // A functional interface exposes a language-level single-argument constructor even though
        // its classifier has no JVM `<init>` declaration. Publish that synthetic declaration through
        // the same expectation table as ordinary constructors so lambdas and callable references are
        // contextually checked before either can emit diagnostics from an expectation-free probe.
        if candidates.is_empty()
            && arity == 1
            && arg_names.is_none_or(|names| names.iter().all(Option::is_none))
        {
            let sam = self.semantic_sam_signature(Ty::obj_name(internal))?;
            let function = Ty::fun_with_shape(
                sam.params,
                sam.ret,
                sam.context_count,
                sam.has_receiver,
                sam.suspend,
            );
            return Some(ConstructionExpectations {
                params: vec![Some((function, false))],
                symbolic: vec![Some(function)],
                type_params: classifier.type_params.clone(),
                type_param_bounds: classifier.type_param_bounds().to_vec(),
            });
        }
        let first = candidates.first()?;
        Some(ConstructionExpectations {
            params: first
                .iter()
                .enumerate()
                .map(|(source, &(expectation, _))| {
                    candidates
                        .iter()
                        .all(|candidate| candidate[source].0 == expectation)
                        .then_some(expectation)
                })
                .collect(),
            symbolic: first
                .iter()
                .enumerate()
                .map(|(source, &(_, symbolic))| {
                    candidates
                        .iter()
                        .all(|candidate| candidate[source].1 == symbolic)
                        .then_some(symbolic)
                        .flatten()
                })
                .collect(),
            type_params: classifier.type_params.clone(),
            type_param_bounds: classifier.type_param_bounds().to_vec(),
        })
    }

    /// Context shared by the qualified source and provider constructor selectors. Only a fixed
    /// declaration parameter agreed by the whole constructor family supplies literal context.
    pub(super) fn contextualize_constructor_arrays(
        &mut self,
        scope: &CheckerScope<'_>,
        call: ExprId,
        internal: TypeName,
        arguments: &[ExprId],
        names: Option<&[Option<String>]>,
    ) -> Vec<(usize, Ty)> {
        let Some(expectations) = self.construction_argument_expectations(
            internal,
            arguments.len(),
            names,
            self.file.call_has_trailing_lambda.contains(&call.0),
        ) else {
            return Vec::new();
        };
        arguments
            .iter()
            .enumerate()
            .filter_map(|(index, &argument)| {
                let declared = expectations.symbolic[index]
                    .or_else(|| expectations.params[index].map(|(ty, _)| ty))?;
                let actual =
                    self.annotation_array_declaration_context(scope, argument, declared)?;
                Some((index, actual))
            })
            .collect()
    }
}
