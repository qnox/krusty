//! Structural executable bodies shared by targets, excluding cloning-only inline templates.

use super::{ExprId, IrFile};

/// Defaults are not structural bodies: targets append only defaults their actual entry/wrapper
/// policy emits. This inventory deliberately does not scan the expression or default arenas.
pub(crate) fn emitted_body_roots(ir: &IrFile) -> Vec<ExprId> {
    let mut roots = ir
        .functions
        .iter()
        .enumerate()
        .filter(|(function, _)| !ir.inline_only_fns.contains(&(*function as super::FunId)))
        .filter_map(|(_, function)| function.body)
        .chain(ir.statics.iter().filter_map(|property| property.init))
        .collect::<Vec<_>>();
    for class in &ir.classes {
        if class.has_primary_ctor {
            roots.extend(class.super_arg_prelude.iter().copied());
            roots.extend(class.super_args.iter().copied());
            roots.extend(class.init_body);
        }
        for constructor in &class.secondary_ctors {
            roots.extend(constructor.delegate_prelude.iter().copied());
            roots.extend(constructor.delegate_args.iter().copied());
            roots.extend(constructor.body);
        }
    }
    roots
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{IrConst, IrExpr, IrFunction};
    use crate::types::Ty;

    #[test]
    fn orphan_expressions_and_inline_templates_are_not_executable_roots() {
        let mut ir = IrFile::default();
        let body = ir.add_expr(IrExpr::Const(IrConst::Int(1)));
        let template = ir.add_expr(IrExpr::Const(IrConst::Int(2)));
        ir.add_expr(IrExpr::Const(IrConst::Int(3)));
        for (name, expression) in [("body", body), ("template", template)] {
            ir.functions.push(IrFunction {
                name: name.to_owned(),
                params: Vec::new(),
                ret: Ty::Int,
                body: Some(expression),
                is_static: true,
                dispatch_receiver: None,
                param_checks: Vec::new(),
            });
        }
        ir.inline_only_fns.insert(1);
        assert_eq!(emitted_body_roots(&ir), vec![body]);
    }

    #[test]
    fn primary_constructor_roots_require_an_emitted_primary_constructor() {
        let mut ir = IrFile::default();
        let prelude = ir.add_expr(IrExpr::Const(IrConst::Int(1)));
        let argument = ir.add_expr(IrExpr::Const(IrConst::Int(2)));
        let initializer = ir.add_expr(IrExpr::Const(IrConst::Int(3)));
        let mut class = super::super::test_support::blank_class("p/C");
        class.super_arg_prelude.push(prelude);
        class.super_args.push(argument);
        class.init_body = Some(initializer);
        ir.classes.push(class);
        assert_eq!(
            emitted_body_roots(&ir),
            vec![prelude, argument, initializer]
        );
        ir.classes[0].has_primary_ctor = false;
        assert!(emitted_body_roots(&ir).is_empty());
    }
}
