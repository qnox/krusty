//! A data class's synthesized `copy`: its visibility and callable signature.
//!
//! `copy` is public unless the class opts into the primary constructor's visibility, either for the
//! whole compilation (`+DataClassCopyRespectsConstructorVisibility`) or per class with
//! `kotlin.ConsistentCopyVisibility`; `kotlin.ExposedCopyVisibility` keeps it public under the
//! feature. Both annotations are matched by their resolved identity, so a same-named user class or
//! a type alias is read for what it resolves to. The checks and their positions follow kotlinc's
//! `FirDataClassConsistentDataCopyAnnotationChecker`: each diagnostic sits on the annotation entry,
//! starting at its `@`.

use crate::ast::ClassDecl;
use crate::diag::{DiagSink, Span};

use super::*;

/// The copy-visibility annotations written on one class declaration, by entry span.
pub(super) struct DataCopyAnnotations {
    consistent: Option<Span>,
    exposed: Option<Span>,
}

impl DataCopyAnnotations {
    pub(super) fn of(table: &SymbolTable, file_index: u32, class: &ClassDecl) -> Self {
        let entry = |identity: TypeName| {
            class
                .annotations
                .iter()
                .find(|annotation| {
                    table.resolved_annotation(file_index, annotation) == Some(identity)
                })
                .map(|annotation| Span::new(annotation.at, annotation.span.hi))
        };
        Self {
            consistent: entry(type_name("kotlin/ConsistentCopyVisibility")),
            exposed: entry(type_name("kotlin/ExposedCopyVisibility")),
        }
    }

    /// Report the annotations' misuse on a classifier that is (`is_data`) or is not a data class.
    /// `respects_ctor_visibility` is the compilation-wide language feature.
    pub(super) fn check(
        &self,
        is_data: bool,
        respects_ctor_visibility: bool,
        diags: &mut DiagSink,
    ) {
        if !is_data {
            if let Some(entry) = self.consistent.or(self.exposed) {
                diags.error(
                    entry,
                    "'@ConsistentCopyVisibility' and '@ExposedCopyVisibility' can only be applied \
                     to data classes.",
                );
            }
            return;
        }
        if let (Some(consistent), Some(exposed)) = (self.consistent, self.exposed) {
            for entry in [consistent, exposed] {
                diags.error(
                    entry,
                    "'@ConsistentCopyVisibility' and '@ExposedCopyVisibility' are incompatible.",
                );
            }
        } else if let Some(consistent) = self.consistent.filter(|_| respects_ctor_visibility) {
            diags.warning_named(
                consistent,
                crate::diag::names::REDUNDANT_ANNOTATION,
                "Annotation 'kotlin.ConsistentCopyVisibility' is redundant.",
            );
        }
    }

    /// The visibility of the data class's `copy`, given its primary constructor's.
    pub(super) fn copy_visibility(
        &self,
        respects_ctor_visibility: bool,
        primary_ctor_visibility: Visibility,
    ) -> Visibility {
        let consistent = if self.exposed.is_some() {
            false
        } else {
            respects_ctor_visibility || self.consistent.is_some()
        };
        if consistent {
            primary_ctor_visibility
        } else {
            Visibility::Public
        }
    }
}

/// The written shape `copy` takes from the primary-constructor properties.
pub(super) struct DataCopyShape {
    /// Each property's name, type and declared generic shape, in constructor order.
    pub(super) properties: Vec<(String, Ty, Ty)>,
    pub(super) ret: Ty,
    pub(super) ret_shape: Ty,
    pub(super) visibility: Visibility,
    pub(super) stable_declaration: Option<crate::fir::DeclarationId>,
}

impl DataCopyShape {
    /// Every `copy` parameter has a default (the receiver's property), so `required` is 0 and any
    /// subset may be passed, by name or position.
    pub(super) fn signature(self) -> Signature {
        let arity = self.properties.len();
        Signature {
            params: self.properties.iter().map(|(_, ty, _)| *ty).collect(),
            ret: self.ret,
            generic_sig: Some(GenericSig {
                formals: Vec::new(),
                formal_bounds: Vec::new(),
                receiver: None,
                params: self.properties.iter().map(|(_, _, shape)| *shape).collect(),
                ret: self.ret_shape,
                return_policy: GenericReturnPolicy::Exact,
            }),
            projected_return_hazard: false,
            flags: SigFlags::default(),
            annotations: Vec::new(),
            equality_bound: None,
            vararg_index: None,
            required: 0,
            param_defaults: vec![true; arity].into(),
            exact_params: vec![false; arity].into(),
            no_infer_params: vec![false; arity].into(),
            implicit_integer_coercion: vec![false; arity].into(),
            param_default_values: Vec::new(),
            param_names: self
                .properties
                .into_iter()
                .map(|(name, _, _)| name)
                .collect(),
            parameter_identities: Vec::new(),
            lambda_param_types: Vec::new(),
            lambda_recv: Vec::new().into(),
            inline_modifiers: Vec::new(),
            visibility: self.visibility,
            context_count: 0,
            source_decl: None,
            stable_declaration: self.stable_declaration,
            source_file: None,
            source_member: None,
            source_receiver: None,
            package: TypeName::ROOT,
            contract: None,
            plugin_expression: None,
        }
    }
}
