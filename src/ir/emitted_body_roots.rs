//! Structural executable bodies shared by targets, excluding cloning-only inline templates, each
//! with the declaration it is emitted for.

use super::{ClassId, ExprId, FunId, IrFile};

/// The declaration an emitted body root belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EmittedDeclaration {
    /// A function's body.
    Function(FunId),
    /// The initializer of the static at this index of [`IrFile::statics`].
    Static(usize),
    /// A class's primary constructor: its delegation's prelude and arguments, and the class's
    /// initialization (property initializers and `init` blocks) it runs.
    PrimaryConstructor(ClassId),
    /// A secondary constructor: its delegation's prelude and arguments, and its body.
    SecondaryConstructor { class: ClassId, ordinal: usize },
}

/// One emitted body root and its declaration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct EmittedRoot {
    pub(crate) declaration: EmittedDeclaration,
    pub(crate) root: ExprId,
}

/// Every emitted body root with its declaration. Defaults are not structural bodies: targets
/// append only defaults their actual entry/wrapper policy emits. This inventory deliberately does
/// not scan the expression or default arenas.
pub(crate) fn emitted_declaration_roots(ir: &IrFile) -> Vec<EmittedRoot> {
    let mut roots = Vec::new();
    for (function, declaration) in ir.functions.iter().enumerate() {
        let function = FunId::try_from(function).expect("too many functions for a function id");
        if ir.inline_only_fns.contains(&function) {
            continue;
        }
        if let Some(root) = declaration.body {
            roots.push(EmittedRoot {
                declaration: EmittedDeclaration::Function(function),
                root,
            });
        }
    }
    for (index, property) in ir.statics.iter().enumerate() {
        if let Some(root) = property.init {
            roots.push(EmittedRoot {
                declaration: EmittedDeclaration::Static(index),
                root,
            });
        }
    }
    for (class_id, class) in ir.classes.iter().enumerate() {
        let class_id = ClassId::try_from(class_id).expect("too many classes for a class id");
        if class.has_primary_ctor {
            let declaration = EmittedDeclaration::PrimaryConstructor(class_id);
            roots.extend(
                class
                    .super_arg_prelude
                    .iter()
                    .chain(&class.super_args)
                    .chain(&class.init_body)
                    .map(|&root| EmittedRoot { declaration, root }),
            );
        }
        for (ordinal, constructor) in class.secondary_ctors.iter().enumerate() {
            let declaration = EmittedDeclaration::SecondaryConstructor {
                class: class_id,
                ordinal,
            };
            roots.extend(
                constructor
                    .delegate_prelude
                    .iter()
                    .chain(&constructor.delegate_args)
                    .chain(&constructor.body)
                    .map(|&root| EmittedRoot { declaration, root }),
            );
        }
    }
    roots
}

/// The roots of [`emitted_declaration_roots`] alone.
pub(crate) fn emitted_body_roots(ir: &IrFile) -> Vec<ExprId> {
    emitted_declaration_roots(ir)
        .into_iter()
        .map(|root| root.root)
        .collect()
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

    #[test]
    fn a_secondary_constructor_s_delegation_and_body_are_its_roots() {
        let mut ir = IrFile::default();
        let prelude = ir.add_expr(IrExpr::Const(IrConst::Int(1)));
        let argument = ir.add_expr(IrExpr::Const(IrConst::Int(2)));
        let body = ir.add_expr(IrExpr::Const(IrConst::Int(3)));
        let mut class = super::super::test_support::blank_class("p/C");
        class.has_primary_ctor = false;
        let mut constructor = super::super::test_support::blank_secondary_ctor();
        constructor.delegate_prelude.push(prelude);
        constructor.delegate_args.push(argument);
        constructor.body = Some(body);
        class.secondary_ctors.push(constructor);
        ir.classes.push(class);
        let declaration = EmittedDeclaration::SecondaryConstructor {
            class: 0,
            ordinal: 0,
        };
        assert_eq!(
            emitted_declaration_roots(&ir),
            [prelude, argument, body].map(|root| EmittedRoot { declaration, root })
        );
    }
}
