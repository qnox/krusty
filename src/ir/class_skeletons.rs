//! How a common-IR class starts: the empty shape of a compiler-generated class and the
//! declaration-only skeleton of a source classifier, before declaration families enrich it.

use super::{
    DeclarationAnnotations, IrClass, IrConstructorTarget, IrCtorArg, IrCtorParameterProvenance,
    IrField, IrPrimaryCtorLines,
};
use crate::types::TypeName;

impl IrClass {
    /// Minimal backend-neutral shape for a compiler-generated class. The producer sets only the
    /// semantic payload it owns (for example `prop_ref`); target passes choose representation.
    pub fn synthetic(fq_name: TypeName) -> Self {
        Self {
            fq_name,
            is_source_declared: false,
            is_compiler_generated: false,
            is_anonymous_object: false,
            enclosure: None,
            is_inner_class: false,
            is_local_class: false,
            is_value: false,
            is_full_value: false,
            is_data: false,
            is_script: false,
            decl_line: 0,
            decl_start_line: 0,
            decl_end_line: 0,
            primary_ctor_lines: IrPrimaryCtorLines::default(),
            type_param_bounds: Vec::new(),
            type_params: Vec::new(),
            captured_type_params: Vec::new(),
            supertypes: Vec::new(),
            properties: Vec::new(),
            fields: Vec::new(),
            ctor_param_count: 0,
            constructor_prefix_count: 0,
            ctor_args: Vec::new(),
            ctor_param_annotations: Vec::new(),
            init_body: None,
            pre_super_param_fields: Vec::new(),
            explicit_param_stores: false,
            methods: Vec::new(),
            is_interface: false,
            is_fun_interface: false,
            is_annotation: false,
            annotation_impl_of: None,
            is_sealed: false,
            sealed_subclasses: Default::default(),
            is_abstract: false,
            is_open: false,
            superclass: crate::types::wk::any(),
            super_arg_prelude: Vec::new(),
            super_args: Vec::new(),
            super_ctor_params: Vec::new(),
            super_ctor: IrConstructorTarget::UNRESTRICTED_PRIMARY,
            is_enum: false,
            enum_entries: Vec::new(),
            is_enum_entry: false,
            prop_ref: None,
            func_ref: None,
            lambda: None,
            sam_wrapper: None,
            bridges: Vec::new(),
            interfaces: Default::default(),
            is_object: false,
            is_companion: false,
            companion_class: None,
            published_nested_classifiers: Vec::new(),
            secondary_ctors: Vec::new(),
            has_primary_ctor: true,
            applied_annotations: DeclarationAnnotations::default(),
            field_annotations: Vec::new(),
            property_annotations: Vec::new(),
            primary_ctor_annotations: DeclarationAnnotations::default(),
            annotation_retention: None,
        }
    }

    /// Build the declaration-only portion available from the pending-free module index. Other
    /// declaration families enrich this class through their own stable identities; this constructor
    /// performs no syntax lookup.
    pub fn source_skeleton(
        header: &crate::fir::ResolvedClassifierHeader,
        flags: crate::fir::DeclarationFlags,
    ) -> Self {
        let superclass = header
            .superclass
            .and_then(|ty| ty.get().non_null().obj_internal())
            .unwrap_or_else(crate::types::wk::any);
        let interfaces = header
            .interfaces
            .iter()
            .filter_map(|ty| ty.get().non_null().obj_internal())
            .collect::<Vec<_>>()
            .into();
        let mut supertypes = Vec::with_capacity(1 + header.interfaces.len());
        if let Some(superclass) = header.superclass {
            supertypes.push(superclass.get());
        }
        supertypes.extend(header.interfaces.iter().map(|interface| interface.get()));
        let script = flags.has(crate::fir::DeclarationFlags::SCRIPT);
        let context_fields = header
            .context_parameters
            .iter()
            .enumerate()
            .map(|(ordinal, parameter)| {
                let ty = crate::types::stored_value_ty(parameter.ty.get());
                if script {
                    script_implicit_receiver_field(ty)
                } else {
                    IrField::new(format!("$context_receiver_{ordinal}"), ty).with_is_final(true)
                }
            })
            .collect::<Vec<_>>();
        // A script's implicit receivers follow its template's constructor parameters, so the
        // constructor lowering appends them after the declared ones.
        let context_arguments = if script {
            Vec::new()
        } else {
            Self::context_constructor_arguments(header)
        };
        let context_count =
            u32::try_from(context_arguments.len()).expect("too many classifier context parameters");
        Self {
            fq_name: header.classifier,
            is_source_declared: true,
            is_compiler_generated: flags.has(crate::fir::DeclarationFlags::COMPILER_GENERATED),
            is_anonymous_object: flags.has(crate::fir::DeclarationFlags::ANONYMOUS_OBJECT),
            enclosure: None,
            is_inner_class: flags.has(crate::fir::DeclarationFlags::INNER),
            is_local_class: flags.has(crate::fir::DeclarationFlags::LOCAL_CLASS),
            is_value: flags.has(crate::fir::DeclarationFlags::VALUE),
            is_full_value: flags.has(crate::fir::DeclarationFlags::FULL_VALUE),
            is_data: flags.has(crate::fir::DeclarationFlags::DATA),
            is_script: script,
            decl_line: 0,
            decl_start_line: 0,
            decl_end_line: 0,
            primary_ctor_lines: IrPrimaryCtorLines::default(),
            type_param_bounds: Vec::new(),
            type_params: Vec::new(),
            captured_type_params: Vec::new(),
            supertypes,
            properties: Vec::new(),
            fields: context_fields,
            ctor_param_count: context_count,
            constructor_prefix_count: context_count,
            ctor_args: context_arguments,
            ctor_param_annotations: Vec::new(),
            init_body: None,
            pre_super_param_fields: Vec::new(),
            explicit_param_stores: false,
            methods: Vec::new(),
            is_interface: flags.has(crate::fir::DeclarationFlags::INTERFACE),
            is_fun_interface: flags.has(crate::fir::DeclarationFlags::FUN_INTERFACE),
            is_annotation: flags.has(crate::fir::DeclarationFlags::ANNOTATION_CLASS),
            annotation_impl_of: None,
            is_sealed: flags.has(crate::fir::DeclarationFlags::SEALED),
            sealed_subclasses: header.sealed_subclasses.to_vec().into(),
            is_abstract: flags.has(crate::fir::DeclarationFlags::ABSTRACT),
            is_open: flags.has(crate::fir::DeclarationFlags::OPEN),
            superclass,
            super_arg_prelude: Vec::new(),
            super_args: Vec::new(),
            super_ctor_params: Vec::new(),
            super_ctor: IrConstructorTarget::UNRESTRICTED_PRIMARY,
            is_enum: flags.has(crate::fir::DeclarationFlags::ENUM),
            enum_entries: Vec::new(),
            is_enum_entry: false,
            prop_ref: None,
            func_ref: None,
            lambda: None,
            sam_wrapper: None,
            bridges: Vec::new(),
            interfaces,
            is_object: flags.has(crate::fir::DeclarationFlags::SINGLETON),
            is_companion: flags.has(crate::fir::DeclarationFlags::COMPANION),
            companion_class: None,
            published_nested_classifiers: Vec::new(),
            secondary_ctors: Vec::new(),
            has_primary_ctor: true,
            applied_annotations: DeclarationAnnotations::default(),
            field_annotations: Vec::new(),
            property_annotations: Vec::new(),
            primary_ctor_annotations: DeclarationAnnotations::default(),
            annotation_retention: None,
        }
    }

    /// A class's context receivers as the leading constructor parameters that initialize their
    /// fields, which lead the field table.
    fn context_constructor_arguments(
        header: &crate::fir::ResolvedClassifierHeader,
    ) -> Vec<IrCtorArg> {
        header
            .context_parameters
            .iter()
            .enumerate()
            .map(|(field, parameter)| IrCtorArg {
                name: parameter.name.as_deref().map(str::to_owned),
                context_kind: parameter.kind,
                ty: crate::types::stored_value_ty(parameter.ty.get()),
                declared_ty: Some(parameter.ty.get()),
                is_field: true,
                field_index: Some(u32::try_from(field).expect("too many context fields")),
                has_default: false,
                is_vararg: false,
                type_param: None,
                check: None,
                anonymous_super_forward: None,
                capture: None,
                provenance: IrCtorParameterProvenance::Value,
                capture_identity: None,
            })
            .collect()
    }

    /// A script's implicit receivers as the constructor parameters kotlinc appends after its
    /// template's, each initializing the receiver's field, which leads the field table.
    pub(crate) fn script_receiver_arguments(
        header: &crate::fir::ResolvedClassifierHeader,
    ) -> impl Iterator<Item = IrCtorArg> + '_ {
        header
            .context_parameters
            .iter()
            .enumerate()
            .map(|(field, parameter)| IrCtorArg {
                name: None,
                context_kind: crate::types::ContextParameterKind::None,
                ty: crate::types::stored_value_ty(parameter.ty.get()),
                declared_ty: Some(parameter.ty.get()),
                is_field: true,
                field_index: Some(u32::try_from(field).expect("too many script receivers")),
                has_default: false,
                is_vararg: false,
                type_param: None,
                check: None,
                anonymous_super_forward: None,
                capture: None,
                provenance: IrCtorParameterProvenance::ScriptReceiver {
                    ordinal: u32::try_from(field).expect("too many script receivers"),
                },
                capture_identity: None,
            })
    }
}

/// The private field kotlinc keeps a script's implicit receiver in, named after its classifier:
/// `$$implicitReceiver_Project`.
fn script_implicit_receiver_field(ty: crate::types::Ty) -> IrField {
    let classifier = ty
        .non_null()
        .obj_internal()
        .expect("a script's implicit receiver is a classifier type");
    IrField::new(
        format!("$$implicitReceiver_{}", classifier.nested_segment_ref()),
        ty,
    )
    .with_is_final(true)
}
