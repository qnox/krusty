//! Complete annotation-construction declaration defaults after all checked constructor-default
//! bodies in this source file have been lowered.

use crate::ir::{IrExpr, IrFile};

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
                enclosing_class: None,
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
                enclosing_class: None,
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
