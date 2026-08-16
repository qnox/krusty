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
            if site.defaults.iter().any(Option::is_some) {
                ir.insert_class_ctor_defaults_name(implementation, site.defaults.clone());
            }
            implementation
        });

        let IrExpr::New { internal, .. } = &mut ir.exprs[expression as usize] else {
            panic!("annotation-construction fact must point at IrExpr::New");
        };
        assert_eq!(
            *internal, site.interface,
            "annotation-construction identity must match its New target"
        );
        *internal = implementation;
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
        enclosing_function: None,
        is_inner_class: false,
        is_local_class: false,
        is_value: false,
        is_data: false,
        decl_line: 0,
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
        ctor_param_count: members.len() as u32,
        ctor_args: members
            .iter()
            .map(|(name, ty)| IrCtorArg {
                name: Some(name.clone()),
                ty: *ty,
                is_field: true,
                has_default: false,
                is_vararg: false,
                type_param: None,
                check: None,
            })
            .collect(),
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
        super_args: Vec::new(),
        super_ctor_params: Vec::new(),
        enum_entries: Vec::new(),
        enum_entry_of: None,
        prop_ref: None,
        func_ref: None,
        bridges: Vec::new(),
        interfaces: vec![interface].into(),
        is_object: false,
        is_companion: false,
        companion_class: None,
        secondary_ctors: Vec::new(),
        has_primary_ctor: true,
        applied_annotations: Vec::new(),
        annotation_retention: None,
    }
}
