//! kotlinc's value-class declaration checker (`FirValueClassDeclarationChecker`) for the rules the
//! language settings decide beyond the primary constructor's shape: whether `equals` and
//! `hashCode` are reserved member names (`CustomEqualsInValueClasses`), the checks of each
//! primary-constructor parameter, which depend on whether the class is multi-field
//! (`JvmInlineMultiFieldValueClasses` in 2.4.0/2.4.10, `FullValueClasses` in 2.4.20), and the typed
//! `equals` of a class represented inline.
//!
//! The primary constructor's shape, and its diagnostics, belong to signature collection
//! (`signature_collection::value_class_declaration`), which decides the class's representation on
//! the compilation target; an `expect` value class's missing primary constructor belongs to header
//! validation (`frontend::expect_value_classes`). This checker consults the same constructor
//! verdict: a failed one ends the class's checking, as it does in kotlinc. Not modelled yet: the
//! checks of a value class's placement, modality, supertypes, members and delegation, and
//! recursive underlying types; and the constructor rules of a legacy `inline class`.

use super::signature_collection::value_class_declaration::{
    ConstructorCheck, ValueClassDeclaration,
};
use super::*;
use crate::ast::{ValueClassArity, ValueClassRules};
use crate::diagnostic_wording::{
    value_class_inapplicable_parameter_type, value_class_parameter_not_final_read_only,
};

/// The member names reserved in every value class represented inline, then the two
/// `CustomEqualsInValueClasses` releases, in kotlinc's order.
const BOX_AND_UNBOX: [&str; 2] = ["box", "unbox"];
const EQUALS_AND_HASH_CODE: [&str; 2] = ["equals", "hashCode"];

/// Whether `declaration` is a top-level `expect` declaration of `file`.
fn declares_expect(file: &File, declaration: DeclId) -> bool {
    file.expect_decls
        .iter()
        .any(|expect| expect.declaration == declaration)
}

/// What the checker needs to know about one value class.
struct ValueClass<'c> {
    declaration: DeclId,
    class: &'c ClassDecl,
    owner: Option<TypeName>,
    /// The resolved `kotlin.jvm.JvmInline` is applied.
    jvm_inline: bool,
    /// kotlinc's full (boxed) value class: `FullValueClasses`, the `value` keyword, no `@JvmInline`.
    full: bool,
    /// `FullValueClasses` is enabled, which changes the words of several messages.
    supports_full: bool,
}

impl ValueClass<'_> {
    /// kotlinc's `valueModifierPrefix`, as its command line prints it.
    fn value_prefix(&self) -> &'static str {
        if self.supports_full {
            "@JvmInline value"
        } else {
            "value"
        }
    }

    /// kotlinc's `isInlineClass`: the class is represented by its single underlying value.
    fn is_inline(&self) -> bool {
        !self.full && self.class.props.len() == 1
    }
}

impl Checker<'_> {
    /// Check a value class declaration as kotlinc's value-class declaration checker does, for the
    /// rules this module covers. `class_tparams` are the classifier's type parameters.
    pub(super) fn check_value_class_declaration(
        &mut self,
        scope: &CheckerScope<'_>,
        declaration: DeclId,
        class: &ClassDecl,
        owner: Option<TypeName>,
        class_tparams: &TParams,
    ) {
        if !class.is_value || class.kind != ClassKind::Class || class.is_singleton() {
            return;
        }
        let jvm_inline =
            self.has_resolved_annotation(scope, &class.annotations, "kotlin/jvm/JvmInline");
        let value_class = ValueClass {
            declaration,
            class,
            owner,
            jvm_inline,
            full: class.value_modifier_span.is_some()
                && !jvm_inline
                && self.file.full_value_classes,
            supports_full: self.file.full_value_classes,
        };
        let rules = self.file.language_gates.value_classes.clone();
        self.check_reserved_members(scope, &value_class, &rules);
        if !self.value_class_constructor_passed(&value_class, &rules) {
            return;
        }
        self.check_value_class_parameters(scope, &value_class, &rules, class_tparams);
        if rules.custom_equals {
            self.check_typed_equals(&value_class);
        }
    }

    /// `RESERVED_MEMBER_INSIDE_VALUE_CLASS` for each reserved name the class declares, and
    /// `RESERVED_MEMBER_FROM_INTERFACE_INSIDE_VALUE_CLASS` for each it inherits with a body from an
    /// interface.
    fn check_reserved_members(
        &mut self,
        scope: &CheckerScope<'_>,
        value_class: &ValueClass<'_>,
        rules: &ValueClassRules,
    ) {
        if value_class.full {
            return;
        }
        let equals_and_hash_code: &[&str] = if rules.custom_equals {
            &[]
        } else {
            &EQUALS_AND_HASH_CODE
        };
        let class = value_class.class;
        let source = self.fed_source();
        // The members' names and modality are all this reads, so the raw class type suffices.
        let class_ty = value_class.owner.map(|owner| Ty::Obj(owner, &[]));
        let mut reports = Vec::new();
        for name in BOX_AND_UNBOX.iter().chain(equals_and_hash_code) {
            for method in &class.methods {
                if method.name == *name && !method.is_abstract() {
                    reports.push((Some(method), method.name_span, reserved_member(name)));
                }
            }
            let (Some(owner), Some(class_ty)) = (value_class.owner, class_ty) else {
                continue;
            };
            let mut supertypes = Vec::new();
            for function in
                crate::symbol_resolver::members_in_hierarchy(&source, class_ty, name).functions()
            {
                let declaring = function.callable.owner;
                if declaring == owner
                    || function.flags.is_abstract
                    || supertypes.contains(&declaring)
                {
                    continue;
                }
                if source
                    .classifier(declaring)
                    .is_some_and(|classifier| classifier.is_interface())
                {
                    supertypes.push(declaring);
                    // kotlinc's `DECLARATION_NAME` of a class runs from its `class` keyword.
                    reports.push((
                        None,
                        Span::new(class.span.lo, class.name_span.hi),
                        format!(
                            "member name '{name}' is reserved for future releases but is implemented in supertype '{}'.",
                            declaring.nested_segment_ref()
                        ),
                    ));
                }
            }
        }
        drop(source);
        for (method, span, message) in reports {
            let depth = method.map(|method| {
                self.push_declaration_policies(scope, &method.annotations, &method.annotation_args)
            });
            let suppressed = self.suppresses_diagnostic(if method.is_some() {
                "RESERVED_MEMBER_INSIDE_VALUE_CLASS"
            } else {
                "RESERVED_MEMBER_FROM_INTERFACE_INSIDE_VALUE_CLASS"
            });
            if let Some(depth) = depth {
                self.active_lexical_policies.truncate(depth);
            }
            if !suppressed {
                self.diags.error(span, message);
            }
        }
    }

    /// kotlinc's primary-constructor verdict, which signature collection reported: whether
    /// checking continues with the parameters. A legacy `inline class` writes no `value` keyword,
    /// and its constructor rules are not modelled.
    fn value_class_constructor_passed(
        &self,
        value_class: &ValueClass<'_>,
        rules: &ValueClassRules,
    ) -> bool {
        let class = value_class.class;
        let Some(value_keyword) = class.value_modifier_span else {
            return false;
        };
        let declaration = ValueClassDeclaration {
            value_keyword,
            constructor: class.primary_constructor_span,
            parameter_count: class.props.len(),
            jvm_inline: value_class.jvm_inline,
            final_class: class.is_final(),
            expect: declares_expect(self.file, value_class.declaration),
        };
        matches!(
            declaration.constructor_check(rules, value_class.supports_full),
            ConstructorCheck::Passed
        )
    }

    /// The checks of each primary-constructor parameter, the first that applies to it.
    fn check_value_class_parameters(
        &mut self,
        scope: &CheckerScope<'_>,
        value_class: &ValueClass<'_>,
        rules: &ValueClassRules,
        class_tparams: &TParams,
    ) {
        let class = value_class.class;
        let header_scope = scope.child(ScopeKind::Block);
        header_scope.declare_tparams(&class.type_params, class_tparams, |_| false);
        // The rules of a multi-field `@JvmInline` class; a full value class follows its own.
        let inline_multi_field_rules =
            matches!(rules.arity, ValueClassArity::MultiFieldFeature { .. }) && !value_class.full;
        let multi_field = inline_multi_field_rules && class.props.len() > 1;
        let checks_finality = inline_multi_field_rules || class.is_final();
        let final_kind = if value_class.supports_full {
            "final value"
        } else {
            "value"
        };
        for parameter in &class.props {
            let not_final_read_only = !parameter.is_property
                || parameter.is_vararg
                || parameter.is_var
                || (parameter.is_open && !parameter.is_override);
            if checks_finality && not_final_read_only {
                self.diags.error(
                    parameter.declaration_span,
                    value_class_parameter_not_final_read_only(final_kind),
                );
                continue;
            }
            if value_class.full && parameter.is_property {
                if class.is_abstract() && !class.is_sealed() {
                    self.diags.error(
                        parameter.declaration_span,
                        "abstract value class primary constructor cannot have property parameters.",
                    );
                    continue;
                }
                if class.is_sealed() {
                    self.diags.error(
                        parameter.declaration_span,
                        "sealed value class primary constructor cannot have property parameters.",
                    );
                    continue;
                }
            }
            if !value_class.full {
                // The type was resolved, and any failure reported, with the classifier's header.
                let diagnostics = self.diags.diags.len();
                let ty = self.type_ref_ty(&header_scope, &parameter.ty);
                self.diags.diags.truncate(diagnostics);
                let rendered = match ty {
                    Ty::Unit => Some("kotlin/Unit"),
                    Ty::Nothing => Some("kotlin/Nothing"),
                    _ => None,
                };
                if let Some(rendered) = rendered {
                    self.diags.error(
                        parameter.ty.span,
                        value_class_inapplicable_parameter_type(
                            rendered,
                            value_class.value_prefix(),
                        ),
                    );
                    continue;
                }
            }
            if let Some(default) = parameter.default.filter(|_| multi_field) {
                self.diags.error(
                    self.span(default),
                    "default parameters are not supported in the primary constructor of a multi-field value class.",
                );
            }
        }
    }

    /// With `CustomEqualsInValueClasses`: the typed `equals` takes no type parameters and only
    /// star-projected type arguments, and overriding `Any.equals` without one is inefficient.
    fn check_typed_equals(&mut self, value_class: &ValueClass<'_>) {
        let class = value_class.class;
        let mut equals_of_any = None;
        let mut typed_equals = None;
        for (index, method) in class.methods.iter().enumerate() {
            match self.equals_member_kind(
                value_class.declaration,
                class,
                value_class.owner,
                index,
                value_class.is_inline(),
            ) {
                Some(EqualsKind::OfAny) => equals_of_any = Some(method),
                Some(EqualsKind::Typed) => typed_equals = Some(method),
                None => {}
            }
        }
        if let Some(typed) = typed_equals {
            if !typed.type_params.is_empty() {
                if let Some(&list) = self.file.type_parameter_lists.get(&typed.signature_span.lo) {
                    self.diags
                        .error(list, "type parameters are prohibited here.");
                }
            }
            let parameter = &typed.params[typed.context_count].ty;
            if parameter
                .targs
                .iter()
                .any(|argument| !argument.is_star_projection())
            {
                self.diags.error(
                    parameter.span,
                    "type arguments for typed value class equals must all be star projections.",
                );
            }
        } else if let Some(equals) = equals_of_any {
            self.diags.warning(
                equals.name_span,
                format!(
                    "overriding 'equals' from 'Any' in value class without operator 'equals(other: {}): Boolean' leads to boxing on every equality comparison.",
                    star_projected_class(class)
                ),
            );
        }
    }
}

impl Checker<'_> {
    /// Whether `class`'s method at `index` overrides `Any.equals` or is the typed equality of a
    /// value class represented inline (kotlinc's `isEquals` and `isTypedEqualsInValueClass`), from
    /// its published signature. `inline` says whether `class` is such a value class.
    pub(super) fn equals_member_kind(
        &self,
        declaration: DeclId,
        class: &ClassDecl,
        owner: Option<TypeName>,
        index: usize,
        inline: bool,
    ) -> Option<EqualsKind> {
        let method = &class.methods[index];
        if method.name != "equals"
            || method.receiver.is_some()
            || method.context_count != 0
            || method.params.len() != 1
        {
            return None;
        }
        let owner = owner?;
        let source_member = crate::libraries::SourceMember::Class {
            file: self.file_index,
            owner: declaration.0,
            method: index as u32,
        };
        // The production identity is the stable declaration; a transient view without one keeps
        // the source coordinate, as the member walk does.
        let stable_declaration = self
            .resolved_index
            .and_then(|resolved| {
                resolved.owned_declaration(
                    resolved.classifier_declaration(owner)?,
                    crate::fir::DeclarationKind::Function,
                    u32::try_from(index).expect("too many class methods"),
                )
            })
            .or_else(|| self.active_source_member_declaration(source_member));
        let classifier = self.fed_source().classifier(owner)?;
        let function = classifier
            .declared_callables
            .get(&method.name)?
            .functions()
            .iter()
            .find(|function| match stable_declaration {
                Some(declaration) => function.stable_declaration == Some(declaration),
                None => function.source_member == Some(source_member),
            })?
            .clone();
        let parameter = *function.callable.params.first()?;
        if parameter.is_nullable() && parameter.non_null().is_erased_top() {
            return Some(EqualsKind::OfAny);
        }
        let result = function.ret.apply(function.callable.ret);
        (inline
            && matches!(parameter, Ty::Obj(class, _) if class == owner)
            && matches!(result, Ty::Boolean | Ty::Nothing))
        .then_some(EqualsKind::Typed)
    }

    /// Whether `class` is a value class represented inline (kotlinc's `isInlineClass`).
    pub(super) fn is_inline_value_class(
        &self,
        scope: &CheckerScope<'_>,
        class: &ClassDecl,
    ) -> bool {
        class.is_value
            && class.props.len() == 1
            && !(class.value_modifier_span.is_some()
                && self.file.full_value_classes
                && !self.has_resolved_annotation(scope, &class.annotations, "kotlin/jvm/JvmInline"))
    }
}

/// The two `equals` shapes kotlinc's value-class checker distinguishes.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum EqualsKind {
    OfAny,
    Typed,
}

fn reserved_member(name: &str) -> String {
    format!("member name '{name}' is reserved for future releases.")
}

/// The class's type with every argument star-projected, as kotlinc renders it in a message.
pub(super) fn star_projected_class(class: &ClassDecl) -> String {
    if class.type_params.is_empty() {
        class.name.clone()
    } else {
        let stars = vec!["*"; class.type_params.len()].join(", ");
        format!("{}<{stars}>", class.name)
    }
}
