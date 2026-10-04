//! Complete annotation-construction declaration defaults after all checked constructor-default
//! bodies in this source file have been lowered, decide where each construction is evaluated,
//! and publish the closed element defaults of the annotation classes this file declares.

use std::collections::BTreeSet;

use crate::ir::{AnnoValue, AppliedAnnotation, ExprId, IrExpr, IrFile, IrTypeOp};
use crate::types::Ty;

use super::FirFileLoweringFailure;

pub(super) fn finalize_defaults(ir: &mut IrFile) -> Result<(), FirFileLoweringFailure> {
    let declaration_defaults = ir
        .annotation_constructions
        .values()
        .filter_map(|construction| {
            ir.class_ctor_defaults_name(construction.interface)
                .cloned()
                .map(|defaults| (construction.interface, defaults))
        })
        .collect::<std::collections::HashMap<_, _>>();

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
    Some(match ir.expr(expression) {
        IrExpr::Const(_) => AnnoValue::Const(super::constructors::checked_default_constant(
            ir, expression, declared,
        )?),
        IrExpr::TypeOp {
            op: IrTypeOp::ImplicitCoercion,
            arg,
            ..
        } => match element_value(ir, *arg, declared)? {
            AnnoValue::Const(_) => AnnoValue::Const(super::constructors::checked_default_constant(
                ir, expression, declared,
            )?),
            value => value,
        },
        IrExpr::EnumEntry { classifier, name } => AnnoValue::Enum(*classifier, name.to_string()),
        IrExpr::KClassLiteral {
            classifier: Some(classifier),
            value: None,
            type_argument: false,
        } => AnnoValue::Class(*classifier),
        IrExpr::Vararg {
            spreads, elements, ..
        } if !spreads.contains(&true) => {
            let element = declared.non_null().array_read_elem()?;
            AnnoValue::Array(
                elements
                    .iter()
                    .map(|value| element_value(ir, *value, element))
                    .collect::<Option<_>>()?,
            )
        }
        IrExpr::New { args, defaults, .. } => {
            let construction = ir.annotation_constructions.get(&expression)?;
            let mut supplied = args.iter();
            let mut values = Vec::new();
            for (ordinal, (name, ty)) in construction.members.iter().enumerate() {
                if defaults.contains(&u32::try_from(ordinal).ok()?) {
                    continue;
                }
                values.push((name.clone(), element_value(ir, *supplied.next()?, *ty)?));
            }
            if supplied.next().is_some() {
                return None;
            }
            AnnoValue::Annotation(AppliedAnnotation {
                internal: construction.interface,
                values,
            })
        }
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{type_name, Ty};

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
            ctor_desc: None,
            external_target: None,
            defaults: Box::new([0]),
            default_prefix_count: 0,
        });
        ir.annotation_constructions.insert(
            construction,
            crate::ir::IrAnnotationConstruction {
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
            ctor_desc: None,
            external_target: None,
            defaults: Box::new([0]),
            default_prefix_count: 0,
        });
        ir.annotation_constructions.insert(
            construction,
            crate::ir::IrAnnotationConstruction {
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
