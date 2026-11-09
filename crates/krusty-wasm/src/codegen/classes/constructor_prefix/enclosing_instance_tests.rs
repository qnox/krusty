use super::enclosing_field;
use krusty::ir::{IrClass, IrCtorParameterProvenance, IrExpr, IrField, IrFile};
use krusty::types::{type_name, Ty, TypeName};

fn whole_file() -> IrFile {
    krusty::fir_lower::single_source_lowering::lower_single_source_with_jvm_stdlib(
        include_str!("../../../../../../tests/box_regressions/InnerEnclosingInstanceEdges.kt"),
        "InnerEnclosingInstanceEdges",
    )
}

fn edges(ir: &IrFile) -> Vec<(TypeName, TypeName)> {
    ir.exprs
        .iter()
        .filter_map(|node| match node {
            IrExpr::EnclosingInstance { inner, outer, .. } => Some((*inner, *outer)),
            _ => None,
        })
        .collect()
}

#[test]
fn whole_inner_program_projects_each_selected_edge_and_complete_generic_storage() {
    let ir = whole_file();
    let selected = edges(&ir);
    assert_eq!(selected.len(), 3, "two nested edges and one generic edge");
    let mut generic = 0;
    for (inner, outer) in selected {
        let (class, field) = enclosing_field(&ir, inner, outer).expect("checked enclosing edge");
        let storage = ir.classes[class as usize].fields[field as usize].ty;
        assert_eq!(storage, ir.declared_class_self_type(outer).unwrap());
        generic += usize::from(!storage.type_args().is_empty());
    }
    assert_eq!(
        generic, 1,
        "the generic outer keeps its declared formal application"
    );
}

#[test]
fn enclosing_role_selects_current_parameter_and_field_without_zero_or_name_assumptions() {
    let mut ir = whole_file();
    let (inner, outer) = edges(&ir)[0];
    let (class, field) = enclosing_field(&ir, inner, outer).unwrap();
    let declaration = &mut ir.classes[class as usize];
    declaration.fields[field as usize].name = "renamedOuterStorage".into();
    declaration
        .fields
        .insert(0, IrField::new("this$0".into(), Ty::Int));
    for argument in &mut declaration.ctor_args {
        if let Some(field) = &mut argument.field_index {
            *field += 1;
        }
    }
    for (parameter, field) in &mut declaration.pre_super_param_fields {
        *parameter += 1;
        *field += 1;
    }
    let mut unrelated = declaration.ctor_args[0].clone();
    unrelated.ty = Ty::Int;
    unrelated.provenance = IrCtorParameterProvenance::Value;
    unrelated.is_field = false;
    unrelated.field_index = None;
    declaration.ctor_args.insert(0, unrelated);
    declaration.constructor_prefix_count += 1;
    declaration.ctor_param_count += 1;
    assert_eq!(enclosing_field(&ir, inner, outer), Some((class, field + 1)));
}

#[test]
fn absent_ambiguous_or_mismatched_enclosing_storage_is_refused() {
    type Mutation = fn(&mut IrClass, u32);
    let mutations: &[(&str, Mutation)] = &[
        ("not inner", |class, _| class.is_inner_class = false),
        ("missing role", |class, _| {
            for argument in &mut class.ctor_args {
                argument.provenance = IrCtorParameterProvenance::Value;
            }
        }),
        ("role outside prefix", |class, _| {
            class.constructor_prefix_count = 0
        }),
        ("duplicate enclosing role", |class, _| {
            let argument = class.ctor_args[0].clone();
            class.ctor_args.push(argument);
            class.constructor_prefix_count += 1;
        }),
        ("unknown field", |class, _| {
            class.ctor_args[0].field_index = Some(u32::MAX)
        }),
        ("mutable field", |class, field| {
            class.fields[field as usize].flags =
                class.fields[field as usize].flags.with_is_final(false);
        }),
        ("wrong storage type", |class, field| {
            class.fields[field as usize].ty = Ty::String
        }),
        ("wrong parameter type", |class, _| {
            class.ctor_args[0].ty = Ty::String
        }),
        ("missing store", |class, _| {
            class.pre_super_param_fields.clear()
        }),
        ("duplicate store", |class, field| {
            class.pre_super_param_fields.push((0, field))
        }),
    ];
    let mut ir = whole_file();
    let (inner, outer) = edges(&ir)[0];
    let (class, field) = enclosing_field(&ir, inner, outer).unwrap();
    let original = ir.classes[class as usize].clone();
    for (label, mutation) in mutations {
        mutation(&mut ir.classes[class as usize], field);
        assert_eq!(enclosing_field(&ir, inner, outer), None, "{label}");
        ir.classes[class as usize] = original.clone();
    }
    assert_eq!(
        enclosing_field(&ir, inner, type_name("UnrelatedOuter")),
        None
    );
    assert_eq!(enclosing_field(&ir, type_name("AbsentInner"), outer), None);
    krusty::ir::test_support::forget_declared_class_self_types(&mut ir);
    assert_eq!(
        enclosing_field(&ir, inner, outer),
        None,
        "no raw-type fallback"
    );
}
