//! kotlinc's `FirClassLiteralChecker` for a class literal whose simple-name left-hand side is
//! written with type arguments that select nothing (`T<Int>::class`, `Str<Int>::class`).
//!
//! A reified type parameter has no type parameters of its own: "type arguments are not allowed for
//! type parameters." on the `<…>` list. A typealias the arguments do not fit reports kotlinc's
//! arity message on the list, and one they fit is not a class: "only classes are allowed on the
//! left-hand side of a class literal." on the literal. kotlinc accepted the useless forms (an alias
//! whose expansion has no type arguments, and an alias to a reference array) with a deprecation
//! warning until `ForbidUselessTypeArgumentsIn25`; any other alias was always the error.

use super::*;

const NOT_A_CLASS: &str = "only classes are allowed on the left-hand side of a class literal.";

/// The typealias a class literal's left-hand side names.
struct WrittenAlias {
    /// kotlinc's rendering: `typealias Yes2<T> = String`.
    display: String,
    formals: Vec<String>,
    expansion: Ty,
}

impl WrittenAlias {
    /// Preserve the exact alias binding selected by the classifier scope tower. Source spelling is
    /// retained only for a statement-local alias's diagnostic rendering; semantic decisions use
    /// the selected declaration record.
    fn selected(source_name: &str, alias: super::classifier_binding::SelectedTypeAlias) -> Self {
        let declared = match alias.identity {
            Some(identity) => identity.nested_segment_ref().to_string(),
            None => source_name.to_string(),
        };
        let rendered_formals = if alias.formals.is_empty() {
            String::new()
        } else {
            format!(
                "<{}>",
                alias
                    .formals
                    .iter()
                    .map(|formal| crate::types::type_parameter_source_name(formal))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        Self {
            display: format!(
                "typealias {declared}{rendered_formals} = {}",
                alias.expansion.source_name()
            ),
            formals: alias.formals,
            expansion: alias.expansion,
        }
    }
}

/// The type of a class literal through the typealias `formals`/`expansion` whose written type
/// arguments select nothing: `Some` when the expansion has no type arguments or is a reference
/// array (each alias formal then reads as its star projection), `None` when the arguments matter.
pub(super) fn useless_alias_type(
    formals: &[String],
    expansion: Ty,
    classifier: &mut dyn FnMut(TypeName) -> Option<std::sync::Arc<crate::libraries::LibraryType>>,
) -> Option<Ty> {
    if !expansion.type_args().is_empty() && !expansion.is_reference_array() {
        return None;
    }
    Some(super::alias_projection::star_projected_alias_type(
        formals, expansion, classifier,
    ))
}

impl Checker<'_> {
    /// Resolve one class-literal left-hand side as an unbound semantic type. A value root remains
    /// bound and is checked by the caller. A simple classifier is selected once by the ordinary
    /// scope tower; the special type-argument diagnostic consumes that selected alias identity.
    pub(super) fn class_literal_unbound_receiver(
        &mut self,
        scope: &CheckerScope<'_>,
        literal: ExprId,
        receiver: ExprId,
    ) -> Option<Ty> {
        let written = match self.file.expr(receiver).clone() {
            Expr::Name(name) => {
                self.class_literal_written_arguments(scope, literal, receiver, &name)
            }
            _ => None,
        };
        if written.is_some() {
            return written;
        }

        // The checker-visible representation of a reified `T::class` is its declaration-owned
        // semantic parameter. The lowering marker must not leak into generic inference.
        let (unbound, type_arguments_checked) =
            if let Expr::Name(name) = self.file.expr(receiver).clone() {
                if scope.tparam_contains(&name) {
                    (scope.reified_tparam(&name), false)
                } else {
                    (self.class_literal_unbound_ty(scope, receiver, &name), false)
                }
            } else {
                // A qualified type name is an unbound literal like a simple imported classifier.
                self.callable_ref_lhs_classifier_type(scope, receiver, literal)
                    .ok()
                    .flatten()
                    .map_or((None, false), |(_, ty, checked)| (Some(ty), checked))
            };
        unbound.map(|ty| {
            let ty = if type_arguments_checked {
                ty
            } else {
                self.validate_class_literal_type_arguments(literal, receiver, ty)
            };
            let Some((internal, [])) = ty.obj_parts() else {
                return ty;
            };
            let classifier = self
                .fed_source()
                .classifier(internal)
                .expect("a resolved class-literal classifier must retain its declaration");
            super::class_literal_receiver::star_projected_class_literal_receiver(ty, &classifier)
        })
    }

    /// The unbound type of the class literal `literal` whose left-hand side `receiver`, spelled
    /// `name`, is written with type arguments and names a reified type parameter or a typealias,
    /// after reporting those arguments. `None` when it names neither.
    pub(super) fn class_literal_written_arguments(
        &mut self,
        scope: &CheckerScope<'_>,
        literal: ExprId,
        receiver: ExprId,
        name: &str,
    ) -> Option<Ty> {
        let arguments = self
            .file
            .call_type_args
            .get(&receiver.0)
            .map_or(0, |arguments| arguments.len());
        if arguments == 0 {
            return None;
        }
        let list = self.file.call_type_args.get(&receiver.0)?.span;
        let forbid = self.file.forbid_useless_type_arguments;
        if self.qualifier_root_is_value(scope, name) {
            return None;
        }
        if let Some(reified) = scope.reified_tparam(name) {
            let message = "type arguments are not allowed for type parameters.";
            let warning = self
                .file
                .useless_type_arguments_warning_format
                .as_ref()
                .map(|format| format.message(message));
            self.report_deprecation_error(
                list,
                forbid,
                warning.as_deref(),
                crate::diag::names::TYPE_ARGUMENTS_NOT_ALLOWED_WARNING,
                message,
            );
            // The diagnostic leaves the literal's type alone, as in kotlinc.
            return Some(reified);
        }
        // A non-reified type parameter is still the selected declaration. It must not fall through
        // and attach an equally spelled alias from a lower scope-tower rung.
        if scope.tparam_contains(name) {
            return None;
        }
        let (selection, _, alias, _) = self.select_type_classifier_binding(scope, name);
        let InheritedNestedClassifier::Found(classifier) = selection else {
            return None;
        };
        let Some(alias) = alias else {
            // The selected declaration is an ordinary classifier. Apply the already-selected
            // identity directly; do not reopen classifier lookup merely to resolve its arguments.
            let reference = TypeRef {
                name: name.to_string(),
                flags: TrFlags::default().with_class_literal_lhs(true),
                arg: None,
                targs: self.file.call_type_args[&receiver.0].arguments.clone(),
                span: self.file.expr_spans[receiver.0 as usize],
                fun_params: Box::new([]),
                fun_context_count: 0,
                qualifier_arguments: Default::default(),
            };
            let ty = self.obj_with_targs_name(scope, classifier, &reference);
            return Some(self.validate_class_literal_type_arguments(literal, receiver, ty));
        };
        Some(self.check_class_literal_alias_type_arguments(scope, literal, receiver, name, alias))
    }

    /// Apply kotlinc's class-literal-only rule to the written arguments of the already-selected
    /// `alias`. The selected declaration travels from the scope/qualifier walk; this operation never
    /// retries lookup from `source_name`.
    pub(super) fn check_class_literal_alias_type_arguments(
        &mut self,
        scope: &CheckerScope<'_>,
        literal: ExprId,
        receiver: ExprId,
        source_name: &str,
        alias: super::classifier_binding::SelectedTypeAlias,
    ) -> Ty {
        let arguments = self
            .file
            .call_type_args
            .get(&receiver.0)
            .map_or(0, |arguments| arguments.len());
        debug_assert!(arguments != 0);
        let list = self
            .file
            .call_type_args
            .get(&receiver.0)
            .map(|arguments| arguments.span)
            .expect("written class-literal type arguments retain their list span");
        let forbid = self.file.forbid_useless_type_arguments;
        let alias = WrittenAlias::selected(source_name, alias);
        let source = self.fed_source();
        let useless_type = useless_alias_type(&alias.formals, alias.expansion, &mut |owner| {
            source.classifier(owner)
        });
        let useless = useless_type.is_some();
        let forbidden = forbid || !useless;
        if alias.formals.len() != arguments {
            let message = format!(
                "{} for '{}'.",
                candidate_display::type_argument_count_wording(alias.formals.len()),
                alias.display
            );
            let warning = self
                .file
                .useless_type_arguments_warning_format
                .as_ref()
                .map(|format| format.message(&message));
            self.report_deprecation_error(
                list,
                forbidden,
                warning.as_deref(),
                crate::diag::names::WRONG_NUMBER_OF_TYPE_ARGUMENTS_IN_GET_CLASS_WARNING,
                &message,
            );
            return useless_type.unwrap_or(Ty::Error);
        }
        let applied = useless.then(|| {
            let arguments = self.file.call_type_args[&receiver.0].arguments.clone();
            self.alias_application_ty(
                scope,
                alias.formals.clone(),
                alias.expansion,
                source_name,
                super::alias_projection::AliasArguments::Written(&arguments),
                self.file.expr_spans[receiver.0 as usize],
            )
            .ty
        });
        let warning = self
            .file
            .useless_type_arguments_warning_format
            .as_ref()
            .map(|format| format.message(NOT_A_CLASS));
        self.report_deprecation_error(
            self.span(literal),
            forbidden,
            warning.as_deref(),
            crate::diag::names::CLASS_LITERAL_LHS_NOT_A_CLASS_WARNING,
            NOT_A_CLASS,
        );
        if !useless {
            return Ty::Error;
        }
        applied.expect("a useless exact-arity alias application has a semantic type")
    }
}
