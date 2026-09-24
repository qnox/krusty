//! JVM realization of semantic annotation constructor calls.
//!
//! Common IR keeps an ordinary `New` plus a sparse checked declaration shape. A JVM annotation is an
//! interface and cannot itself be allocated, so this pass creates kotlinc's one implementation class
//! per annotation per source file and retargets every tagged construction to that physical class.

use std::collections::HashMap;

use crate::ir::{IrClass, IrCtorArg, IrExpr, IrField, IrFile};
use crate::types::{type_name, Ty, TypeName};

pub(crate) fn lower_annotation_constructions(ir: &mut IrFile, facade: &str) {
    let sites = std::mem::take(&mut ir.annotation_constructions);
    if sites.is_empty() {
        return;
    }

    // kotlinc prefers a classifier scope over the file facade even when a top-level construction
    // appears earlier in source/arena order, then chooses the earliest emitted classifier scope.
    let class_order = ir
        .classes
        .iter()
        .enumerate()
        .map(|(index, class)| (class.fq_name_id(), index))
        .collect::<HashMap<_, _>>();
    let mut lexical_owners = HashMap::<TypeName, Option<TypeName>>::new();
    for site in sites.values() {
        let candidate_rank = site
            .enclosing_class
            .and_then(|owner| class_order.get(&owner).copied())
            .unwrap_or(usize::MAX);
        let current_rank = lexical_owners
            .get(&site.interface)
            .copied()
            .flatten()
            .and_then(|owner| class_order.get(&owner).copied())
            .unwrap_or(usize::MAX);
        if !lexical_owners.contains_key(&site.interface) || candidate_rank < current_rank {
            lexical_owners.insert(site.interface, site.enclosing_class);
        }
    }

    // A construction records its annotation's parameter defaults only when the annotation class was
    // lowered before it, so one site can carry them while another does not. The implementation class
    // serves every site, so it takes the defaults any site recorded, else the annotation's own.
    let mut declared_defaults = HashMap::<TypeName, Vec<Option<u32>>>::new();
    for expression in 0..ir.exprs.len() as u32 {
        let Some(site) = sites.get(&expression) else {
            continue;
        };
        if site.defaults.iter().any(Option::is_some) {
            declared_defaults
                .entry(site.interface)
                .or_insert_with(|| site.defaults.clone());
        }
    }
    for site in sites.values() {
        if let Some(defaults) = ir.class_ctor_defaults_name(site.interface) {
            if defaults.iter().any(Option::is_some) {
                declared_defaults
                    .entry(site.interface)
                    .or_insert_with(|| defaults.clone());
            }
        }
    }

    let mut implementations = HashMap::<TypeName, TypeName>::new();
    let mut generated = Vec::new();
    for expression in 0..ir.exprs.len() as u32 {
        let Some(site) = sites.get(&expression) else {
            continue;
        };
        let implementation = *implementations.entry(site.interface).or_insert_with(|| {
            let lexical_owner = lexical_owners
                .get(&site.interface)
                .copied()
                .flatten()
                .map(TypeName::render)
                .unwrap_or_else(|| facade.to_string());
            let interface_fragment = site.interface.render().replace(['/', '$'], "_");
            let implementation = type_name(&format!(
                "{lexical_owner}$annotationImpl${interface_fragment}$0"
            ));
            generated.push(annotation_implementation(
                implementation,
                site.interface,
                &site.members,
            ));
            if let Some(defaults) = declared_defaults.get(&site.interface) {
                ir.insert_class_ctor_defaults_name(implementation, defaults.clone());
            }
            implementation
        });

        let IrExpr::New {
            internal,
            external_target,
            ..
        } = &mut ir.exprs[expression as usize]
        else {
            panic!("annotation-construction fact must point at IrExpr::New");
        };
        assert_eq!(
            *internal, site.interface,
            "annotation-construction identity must match its New target"
        );
        *internal = implementation;
        // The checked dependency constructor identified the annotation declaration, not a JVM
        // allocation target. This pass has replaced it with a generated in-file implementation;
        // leaving the provider identity attached would make external-call realization attempt to
        // invoke the annotation interface's nonexistent constructor/default stub.
        *external_target = None;
    }
    for class in generated {
        ir.add_class(class);
    }
}

fn annotation_implementation(
    fq_name: TypeName,
    interface: TypeName,
    members: &[(String, Ty)],
) -> IrClass {
    IrClass {
        fq_name,
        is_source_declared: false,
        is_anonymous_object: false,
        enclosure: None,
        is_inner_class: false,
        is_local_class: false,
        is_value: false,
        is_data: false,
        decl_line: 0,
        decl_start_line: 0,
        decl_end_line: 0,
        type_param_bounds: Vec::new(),
        type_params: Vec::new(),
        captured_type_params: Vec::new(),
        supertypes: Vec::new(),
        properties: Vec::new(),
        fields: members
            .iter()
            .map(|(name, ty)| IrField::new(name.clone(), *ty))
            .collect(),
        field_annotations: Vec::new(),
        primary_ctor_annotations: crate::ir::DeclarationAnnotations::default(),
        property_annotations: Vec::new(),
        ctor_param_count: members.len() as u32,
        constructor_prefix_count: 0,
        ctor_args: members
            .iter()
            .map(|(name, ty)| IrCtorArg {
                name: Some(name.clone()),
                context_kind: crate::types::ContextParameterKind::None,
                ty: *ty,
                declared_ty: None,
                is_field: true,
                field_index: None,
                has_default: false,
                is_vararg: false,
                type_param: None,
                check: None,
            })
            .collect(),
        ctor_param_annotations: Vec::new(),
        init_body: None,
        pre_super_param_fields: Vec::new(),
        explicit_param_stores: false,
        methods: Vec::new(),
        is_interface: false,
        is_fun_interface: false,
        is_annotation: false,
        annotation_impl_of: Some(interface),
        is_sealed: false,
        sealed_subclasses: Default::default(),
        is_abstract: false,
        is_open: false,
        superclass: type_name("kotlin/Any"),
        super_arg_prelude: Vec::new(),
        super_args: Vec::new(),
        super_ctor_params: Vec::new(),
        super_ctor_is_primary: true,
        enum_entries: Vec::new(),
        enum_entry_of: None,
        prop_ref: None,
        func_ref: None,
        bridges: Vec::new(),
        interfaces: vec![interface].into(),
        is_object: false,
        is_companion: false,
        companion_class: None,
        published_nested_classifiers: Vec::new(),
        secondary_ctors: Vec::new(),
        has_primary_ctor: true,
        applied_annotations: crate::ir::DeclarationAnnotations::default(),
        annotation_retention: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fir::ExternalCallableId;

    #[test]
    fn external_annotation_construction_becomes_an_internal_implementation_allocation() {
        let interface = type_name("dependency/Marker");
        let mut ir = IrFile::default();
        let value = ir.add_expr(IrExpr::Const(crate::ir::IrConst::String("OK".into())));
        let construction = ir.add_expr(IrExpr::New {
            internal: interface,
            args: vec![value],
            ctor_params: Some(vec![Ty::String]),
            ctor_desc: None,
            external_target: Some(ExternalCallableId::from_raw(7)),
            defaults: Box::new([]),
            default_prefix_count: 0,
        });
        ir.annotation_constructions.insert(
            construction,
            crate::ir::IrAnnotationConstruction {
                interface,
                members: vec![("value".into(), Ty::String)],
                defaults: vec![None],
                enclosing_class: None,
            },
        );

        lower_annotation_constructions(&mut ir, "sample/MainKt");

        let IrExpr::New {
            internal,
            external_target,
            ..
        } = ir.expr(construction)
        else {
            panic!("annotation construction remains an allocation")
        };
        assert!(internal
            .render()
            .contains("$annotationImpl$dependency_Marker$0"));
        assert_eq!(*external_target, None);
        assert!(ir.classes.iter().any(|class| {
            class.fq_name_id() == *internal && class.annotation_impl_of == Some(interface)
        }));
    }

    #[test]
    fn an_implementation_takes_its_annotations_defaults_when_the_first_site_recorded_none() {
        let interface = type_name("sample/A");
        let mut ir = IrFile::default();
        let default_value = ir.add_expr(IrExpr::Const(crate::ir::IrConst::Int(1)));
        ir.insert_class_ctor_defaults_name(interface, vec![Some(default_value)]);
        let construction = ir.add_expr(IrExpr::New {
            internal: interface,
            args: vec![default_value],
            ctor_params: Some(vec![Ty::Int]),
            ctor_desc: None,
            external_target: None,
            defaults: Box::new([]),
            default_prefix_count: 0,
        });
        // Lowered before the annotation class, so the site saw no defaults.
        ir.annotation_constructions.insert(
            construction,
            crate::ir::IrAnnotationConstruction {
                interface,
                members: vec![("x".into(), Ty::Int)],
                defaults: vec![None],
                enclosing_class: None,
            },
        );

        lower_annotation_constructions(&mut ir, "sample/MainKt");

        let IrExpr::New { internal, .. } = ir.expr(construction) else {
            panic!("annotation construction remains an allocation")
        };
        assert_eq!(
            ir.class_ctor_defaults_name(*internal),
            Some(&vec![Some(default_value)])
        );
    }
}
