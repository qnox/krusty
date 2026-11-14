//! Complete annotation-construction declaration defaults after all checked constructor-default
//! bodies in this source file have been lowered, decide where each construction is evaluated,
//! and publish the closed element defaults of the annotation classes this file declares.

use std::collections::BTreeSet;

use crate::ir::{AnnoValue, AppliedAnnotation, ExprId, IrExpr, IrFile, IrTypeOp};
use crate::types::Ty;

use super::FirFileLoweringFailure;

mod declaration_evaluation;

pub(super) fn normalize_declaration_defaults(ir: &mut IrFile) {
    declaration_evaluation::normalize(ir);
}

pub(super) fn finalize_defaults(ir: &mut IrFile) -> Result<(), FirFileLoweringFailure> {
    let declaration_defaults = ir
        .annotation_constructions
        .values()
        .filter_map(|construction| {
            ir.class_ctor_defaults_name(construction.interface)
                .cloned()
                .map(|defaults| (construction.interface, defaults))
        })
        .collect::<crate::fx_hash::HashMap<_, _>>();

    for construction in ir.annotation_constructions.values_mut() {
        if let Some(defaults) = declaration_defaults.get(&construction.interface) {
            if defaults.len() != construction.members.len() {
                return Err(FirFileLoweringFailure::IncompleteAnnotationConstruction(
                    construction.interface,
                ));
            }
            construction.defaults.clone_from(defaults);
        }
    }

    for (&expression, construction) in &ir.annotation_constructions {
        let IrExpr::New { defaults, .. } = ir.expr(expression) else {
            return Err(FirFileLoweringFailure::IncompleteAnnotationConstruction(
                construction.interface,
            ));
        };
        if defaults.iter().any(|parameter| {
            construction
                .defaults
                .get(*parameter as usize)
                .is_none_or(Option::is_none)
        }) {
            return Err(FirFileLoweringFailure::IncompleteAnnotationConstruction(
                construction.interface,
            ));
        }
    }
    Ok(())
}

/// Decide where each construction is evaluated. kotlinc substitutes an omitted element's
/// declaration default at the construction, so a construction inside that default runs in the
/// scopes of every construction omitting the element, transitively; one inside a default nothing
/// here omits runs nowhere in this source.
pub(super) fn assign_evaluation_scopes(ir: &mut IrFile) {
    let mut roots = ir
        .annotation_constructions
        .values()
        .flat_map(|construction| construction.defaults.iter().flatten().copied())
        .collect::<Vec<_>>();
    for class in ir.classes.iter().filter(|class| class.is_annotation) {
        if let Some(defaults) = ir.class_ctor_defaults_name(class.fq_name_id()) {
            roots.extend(defaults.iter().flatten().copied());
        }
    }
    let in_defaults = constructions_under(ir, roots);
    for expression in &in_defaults {
        if let Some(construction) = ir.annotation_constructions.get_mut(expression) {
            construction.scopes.clear();
        }
    }
    let mut pending = ir
        .annotation_constructions
        .keys()
        .copied()
        .filter(|expression| !in_defaults.contains(expression))
        .collect::<Vec<_>>();
    while let Some(consumer) = pending.pop() {
        let IrExpr::New { defaults, .. } = ir.expr(consumer) else {
            continue;
        };
        let construction = &ir.annotation_constructions[&consumer];
        let omitted = defaults
            .iter()
            .filter_map(|parameter| construction.defaults.get(*parameter as usize).copied())
            .flatten()
            .collect::<Vec<_>>();
        let scopes = construction.scopes.clone();
        for nested in constructions_under(ir, omitted) {
            let Some(construction) = ir.annotation_constructions.get_mut(&nested) else {
                continue;
            };
            let before = construction.scopes.len();
            for scope in &scopes {
                if !construction.scopes.contains(scope) {
                    construction.scopes.push(*scope);
                }
            }
            if construction.scopes.len() != before {
                pending.push(nested);
            }
        }
    }
}

/// Every annotation construction in the expression trees under `roots`.
fn constructions_under(ir: &IrFile, mut roots: Vec<ExprId>) -> BTreeSet<ExprId> {
    let mut seen = BTreeSet::new();
    let mut found = BTreeSet::new();
    while let Some(expression) = roots.pop() {
        if !seen.insert(expression) {
            continue;
        }
        if ir.annotation_constructions.contains_key(&expression) {
            found.insert(expression);
        }
        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| roots.push(child));
    }
    found
}

/// Record the closed declaration default of each element of every annotation class this source
/// declares, from its lowered checked defaults. Only constants, enum entries, class literals,
/// arrays of those, and annotation constructions are annotation element values.
pub(super) fn publish_element_defaults(ir: &mut IrFile) {
    let mut published = Vec::new();
    for class in ir.classes.iter().filter(|class| class.is_annotation) {
        let Some(defaults) = ir.class_ctor_defaults_name(class.fq_name_id()) else {
            continue;
        };
        let elements = class
            .ctor_args
            .iter()
            .zip(defaults)
            .filter_map(|(parameter, default)| {
                let field = &class.fields[parameter.field_index? as usize];
                let value = element_value(ir, (*default)?, field.ty)?;
                Some((field.name.clone(), value))
            })
            .collect::<Vec<_>>();
        published.push((class.fq_name_id(), elements));
    }
    ir.annotation_element_defaults.extend(published);
}

fn element_value(ir: &IrFile, expression: ExprId, declared: Ty) -> Option<AnnoValue> {
    element_value_with_bindings(ir, expression, declared, &[])
}

fn element_value_with_bindings(
    ir: &IrFile,
    expression: ExprId,
    declared: Ty,
    bindings: &[(u32, AnnoValue)],
) -> Option<AnnoValue> {
    Some(match ir.expr(expression) {
        IrExpr::Const(_) => AnnoValue::Const(super::constructors::checked_default_constant(
            ir, expression, declared,
        )?),
        IrExpr::TypeOp {
            op: IrTypeOp::ImplicitCoercion,
            arg,
            ..
        } => element_value_with_bindings(ir, *arg, declared, bindings)?,
        IrExpr::GetValue(index) => bindings
            .iter()
            .rev()
            .find(|(bound, _)| bound == index)?
            .1
            .clone(),
        IrExpr::Block {
            stmts,
            value: Some(value),
        } => operand_bound_value(ir, stmts, *value, declared, bindings)?,
        IrExpr::EnumEntry { classifier, name } => AnnoValue::Enum(*classifier, name.to_string()),
        IrExpr::KClassLiteral {
            classifier: Some(classifier),
            value: None,
            type_argument: false,
        } => AnnoValue::Class(*classifier),
        IrExpr::Vararg {
            array_type,
            spreads,
            elements,
        } => {
            let element = declared.non_null().array_read_elem()?;
            let mut values = Vec::new();
            for (index, expression) in elements.iter().copied().enumerate() {
                if spreads.get(index).copied().unwrap_or(false) {
                    let AnnoValue::Array(spread) =
                        element_value_with_bindings(ir, expression, *array_type, bindings)?
                    else {
                        return None;
                    };
                    values.extend(spread);
                } else {
                    values.push(element_value_with_bindings(
                        ir, expression, element, bindings,
                    )?);
                }
            }
            AnnoValue::Array(values)
        }
        IrExpr::New { .. } => {
            let construction = ir.annotation_constructions.get(&expression)?;
            let mut values = Vec::new();
            for member in supplied_members(ir, expression)? {
                // Metadata omission does not remove a supplied runtime operand. Prove that value
                // closed too, before projecting only the members metadata actually records.
                let value = element_value_with_bindings(ir, member.value, member.ty, bindings)?;
                if member.written {
                    values.push((member.name.to_owned(), value));
                }
            }
            AnnoValue::Annotation(AppliedAnnotation {
                written_order: construction
                    .arguments
                    .applied_order(construction.members.len())?,
                internal: construction.interface,
                values,
            })
        }
        _ => return None,
    })
}

struct SuppliedMember<'a> {
    name: &'a str,
    ty: Ty,
    value: ExprId,
    written: bool,
}

/// Align compact runtime operands with the checked declaration members. Runtime omissions and
/// metadata omissions are distinct: a replay can supply a member whose metadata remains omitted.
fn supplied_members(ir: &IrFile, expression: ExprId) -> Option<Vec<SuppliedMember<'_>>> {
    let IrExpr::New { args, defaults, .. } = ir.expr(expression) else {
        return None;
    };
    let construction = ir.annotation_constructions.get(&expression)?;
    let _ = construction
        .arguments
        .applied_order(construction.members.len())?;
    let mut omitted = BTreeSet::new();
    for &ordinal in defaults.iter() {
        if ordinal as usize >= construction.members.len()
            || !omitted.insert(ordinal)
            || !construction.arguments.omits(ordinal)
        {
            return None;
        }
    }
    let mut supplied = args.iter();
    let mut members = Vec::new();
    for (ordinal, (name, ty)) in construction.members.iter().enumerate() {
        let ordinal = u32::try_from(ordinal).ok()?;
        if !omitted.contains(&ordinal) {
            members.push(SuppliedMember {
                name,
                ty: *ty,
                value: *supplied.next()?,
                written: !construction.arguments.omits(ordinal),
            });
        }
    }
    supplied.next().is_none().then_some(members)
}

/// Named-argument evaluation introduces checked operand holders. Only their closed initializers
/// participate in declaration defaults; arbitrary runtime locals remain opaque.
fn operand_bound_value(
    ir: &IrFile,
    statements: &[ExprId],
    result: ExprId,
    declared: Ty,
    bindings: &[(u32, AnnoValue)],
) -> Option<AnnoValue> {
    let mut scoped = bindings.to_vec();
    for statement in statements {
        let IrExpr::Variable {
            index,
            ty,
            init: Some(init),
            named: false,
        } = ir.expr(*statement)
        else {
            return None;
        };
        if !ir.call_operand_bindings.contains(statement) {
            return None;
        }
        let value = element_value_with_bindings(ir, *init, *ty, &scoped)?;
        scoped.push((*index, value));
    }
    element_value_with_bindings(ir, result, declared, &scoped)
}

#[cfg(test)]
mod tests {
    mod argument_provenance;

    use super::*;
    use crate::types::{type_name, Ty};

    #[test]
    fn only_closed_call_operand_holders_are_values() {
        let mut ir = IrFile::default();
        let constant = ir.add_expr(IrExpr::Const(crate::ir::IrConst::Int(7)));
        let binding = ir.add_expr(IrExpr::Variable {
            index: 3,
            ty: Ty::Int,
            init: Some(constant),
            named: false,
        });
        let read = ir.add_expr(IrExpr::GetValue(3));
        let block = ir.add_expr(IrExpr::Block {
            stmts: vec![binding],
            value: Some(read),
        });
        assert!(element_value(&ir, block, Ty::Int).is_none());
        ir.call_operand_bindings.insert(binding);
        assert!(matches!(
            element_value(&ir, block, Ty::Int),
            Some(AnnoValue::Const(crate::ir::IrConst::Int(7)))
        ));
        assert!(element_value(&ir, read, Ty::Int).is_none());
        let IrExpr::Variable { named, .. } = &mut ir.exprs[binding as usize] else {
            panic!("operand binding");
        };
        *named = true;
        assert!(element_value(&ir, block, Ty::Int).is_none());
    }

    #[test]
    fn closed_array_spreads_flatten_in_order_without_dropping_unmarked_elements() {
        let mut ir = IrFile::default();
        let array = Ty::array(Ty::Int);
        let elements = (1..=3)
            .map(|value| ir.add_expr(IrExpr::Const(crate::ir::IrConst::Int(value))))
            .collect::<Vec<_>>();
        let middle = ir.add_expr(IrExpr::Vararg {
            array_type: array,
            elements: vec![elements[1]],
            spreads: Vec::new(),
        });
        let outer = ir.add_expr(IrExpr::Vararg {
            array_type: array,
            elements: vec![elements[0], middle, elements[2]],
            spreads: vec![false, true, false],
        });
        assert_eq!(
            format!("{:?}", element_value(&ir, outer, array)),
            "Some(Array([Const(Int(1)), Const(Int(2)), Const(Int(3))]))"
        );
        let malformed = ir.add_expr(IrExpr::Vararg {
            array_type: array,
            elements: vec![elements[0]],
            spreads: vec![true],
        });
        assert!(element_value(&ir, malformed, array).is_none());
    }

    #[test]
    fn checked_declaration_defaults_replace_an_incomplete_earlier_site() {
        let interface = type_name("sample/A");
        let mut ir = IrFile::default();
        let closed = ir.add_expr(IrExpr::Const(crate::ir::IrConst::Int(1)));
        let checked = ir.add_expr(IrExpr::Const(crate::ir::IrConst::Int(2)));
        ir.insert_class_ctor_defaults_name(interface, vec![Some(checked)]);
        let construction = ir.add_expr(IrExpr::New {
            internal: interface,
            args: Vec::new(),
            ctor_params: None,
            declared_params: None,
            ctor_desc: None,
            external_target: None,
            defaults: Box::new([0]),
            default_prefix_count: 0,
        });
        ir.annotation_constructions.insert(
            construction,
            crate::ir::IrAnnotationConstruction {
                arguments: crate::libraries::AnnotationArgumentProvenance::from_selected(
                    1,
                    &[],
                    &[0],
                )
                .unwrap(),
                interface,
                members: vec![("value".into(), Ty::Int)],
                defaults: vec![Some(closed)],
                scopes: vec![None],
            },
        );

        finalize_defaults(&mut ir).unwrap();

        assert_eq!(
            ir.annotation_constructions[&construction].defaults,
            [Some(checked)]
        );
    }

    #[test]
    fn an_omitted_unavailable_default_is_rejected() {
        let interface = type_name("dependency/A");
        let mut ir = IrFile::default();
        let construction = ir.add_expr(IrExpr::New {
            internal: interface,
            args: Vec::new(),
            ctor_params: Some(vec![Ty::Int]),
            declared_params: None,
            ctor_desc: None,
            external_target: None,
            defaults: Box::new([0]),
            default_prefix_count: 0,
        });
        ir.annotation_constructions.insert(
            construction,
            crate::ir::IrAnnotationConstruction {
                arguments: crate::libraries::AnnotationArgumentProvenance::from_selected(
                    1,
                    &[],
                    &[0],
                )
                .unwrap(),
                interface,
                members: vec![("value".into(), Ty::Int)],
                defaults: vec![None],
                scopes: vec![None],
            },
        );

        assert_eq!(
            finalize_defaults(&mut ir),
            Err(FirFileLoweringFailure::IncompleteAnnotationConstruction(
                interface
            ))
        );
    }
}
