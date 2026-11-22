//! The entries constructing a class of the file or answering one of its values.
//!
//! An entry is an ordinary compiler-generated static function of the file. A target records it so
//! another file or module constructs the class through a call (see
//! `crate::backend::construction_entries`), and a dependency unit builds one for a value class's
//! box, whose construction is its whole constructor.

use super::{ClassId, ExprId, FunId, IrExpr, IrFile, IrFunction};
use crate::types::Ty;

/// A class's construction entry and the construction its body answers.
#[derive(Clone, Copy)]
pub(crate) struct ConstructionEntry {
    pub(crate) function: FunId,
    pub(crate) construction: ExprId,
}

impl IrFile {
    /// Build the entry constructing `class` through its primary constructor, or through its
    /// secondary constructor of that index, and record it as the class's construction entry.
    /// None when the file does not lay out the class's object.
    pub(crate) fn add_construction_entry(
        &mut self,
        class: ClassId,
        secondary: Option<usize>,
    ) -> Option<ConstructionEntry> {
        let declaration = &self.classes[class as usize];
        let internal = declaration.fq_name;
        let result = self.declared_class_self_type(internal)?;
        let (parameters, selected, declared) = match secondary {
            None => (
                declaration
                    .ctor_args
                    .iter()
                    .map(|argument| argument.ty)
                    .collect(),
                None,
                declaration
                    .ctor_args
                    .iter()
                    .map(|argument| argument.declared_ty.unwrap_or(argument.ty))
                    .collect::<Vec<_>>(),
            ),
            Some(ordinal) => {
                let secondary = declaration.secondary_ctors.get(ordinal)?;
                // An inner class's constructors lead with the enclosing instance, as its primary's
                // `ctor_args` do; the selection spells the whole list.
                let mut all = secondary.prefix_params.clone();
                all.extend(secondary.params.iter().copied());
                (all.clone(), Some(all.clone()), all)
            }
        };
        let arguments = parameters
            .iter()
            .enumerate()
            .map(|(slot, &ty)| self.parameter_read(slot, ty))
            .collect();
        let construction = self.add_expr(IrExpr::New {
            internal,
            args: arguments,
            ctor_params: selected,
            declared_params: Some(declared.into_boxed_slice()),
            ctor_desc: None,
            external_target: None,
            defaults: Box::default(),
            default_prefix_count: 0,
        });
        self.set_logical_type(construction, result);
        let entry = self.add_generated_entry("<init>$new", parameters, result, construction);
        self.construction_entries.insert(entry, class);
        Some(ConstructionEntry {
            function: entry,
            construction,
        })
    }

    /// Build the entry answering whether its one reference is an instance of `class`, and record
    /// it as that class's type-test entry. `None` when the file does not lay out the class.
    pub(crate) fn add_type_test_entry(&mut self, class: ClassId) -> Option<FunId> {
        let internal = self.classes[class as usize].fq_name;
        let class_type = self.declared_class_self_type(internal)?;
        let any = Ty::nullable(Ty::obj_name(crate::types::wk::any()));
        let reference = self.parameter_read(0, any);
        let test = self.add_expr(IrExpr::TypeOp {
            op: super::IrTypeOp::InstanceOf,
            arg: reference,
            type_operand: class_type,
        });
        self.set_logical_type(test, Ty::Boolean);
        Some(self.add_generated_entry("<is>$test", vec![any], Ty::Boolean, test))
    }

    /// A compiler-generated static function of the file returning `value`.
    pub(crate) fn add_generated_entry(
        &mut self,
        name: &str,
        parameters: Vec<Ty>,
        result: Ty,
        value: ExprId,
    ) -> FunId {
        let last = self.add_expr(IrExpr::Return(Some(value)));
        let body = self.add_expr(IrExpr::Block {
            stmts: vec![last],
            value: None,
        });
        let entry = self.add_fun(IrFunction {
            name: name.to_owned(),
            params: parameters,
            ret: result,
            body: Some(body),
            is_static: true,
            dispatch_receiver: None,
            param_checks: Vec::new(),
        });
        // Compiler-generated: no source declaration of its own, and no public name.
        self.synthetic_methods.insert(entry);
        entry
    }

    fn parameter_read(&mut self, slot: usize, ty: Ty) -> ExprId {
        let read = self.add_expr(IrExpr::GetValue(
            u32::try_from(slot).expect("too many parameters"),
        ));
        self.set_logical_type(read, ty);
        read
    }
}
