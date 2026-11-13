//! Wasm realization of shared mutable-local cells.
//!
//! Common lowering moves a `var` that a lambda writes into a cell: its declaration initializes the
//! local with an [`IrExpr::RefNew`], and every use is an [`IrExpr::RefGet`] or [`IrExpr::RefSet`]
//! through that local. The IR keeps the element's Kotlin type and leaves the cell's shape to the
//! backend. Here a cell is a struct with one mutable field of the element's carrier, one struct
//! type per carrier, and the local holds a reference to it.
//!
//! A recorded shared-capture parameter carries that same cell across a body boundary. Ordinary
//! value operands still cannot read a holder, assign it, or manufacture a cell from its contents.

use super::*;
use crate::encode::{CompositeType, FieldType, StorageType};

/// The nullable reference a shared-capture field or constructor parameter carries.
pub(in crate::codegen) fn cell_reference(
    module: &mut Module,
    element: Ty,
) -> Result<ValType, Unsupported> {
    Ok(ValType::nullable(cell_type(module, element)?))
}

/// One physical cell form, shared by local allocation, parameters and closure fields.
fn cell_type(module: &mut Module, elem: Ty) -> Result<u32, Unsupported> {
    let carried = carrier(elem)?.ok_or_else(|| "a shared cell of `Unit`".to_string())?;
    Ok(module.ty(CompositeType::Struct(vec![FieldType {
        storage: StorageType::Val(carried),
        mutable: true,
    }])))
}

/// The parameter's physical ABI. A mutable capture keeps its recorded holder type even when a
/// specialization changes the logical parameter type; the contents are not the argument.
pub(in crate::codegen) fn parameter_carrier(
    module: &mut Module,
    ir: &IrFile,
    function: FunId,
    parameter: u32,
) -> Result<Option<ValType>, Unsupported> {
    let declared = ir
        .functions
        .get(function as usize)
        .and_then(|function| function.params.get(parameter as usize))
        .ok_or("a shared capture parameter outside its function")?;
    match ir.shared_capture_parameters.get(&(function, parameter)) {
        Some(&elem) => Ok(Some(ValType::nullable(cell_type(module, elem)?))),
        None => carrier(*declared),
    }
}

impl Body<'_> {
    /// The common parameter ordinal excludes the dispatch receiver; the body slot includes it.
    pub(super) fn shared_cell_parameters(&mut self, function: FunId) -> Result<(), Unsupported> {
        let declaration = &self.ir.functions[function as usize];
        let first = u32::from(declaration.dispatch_receiver.is_some());
        let parameters = declaration.params.len();
        let cells = self
            .ir
            .shared_capture_parameters
            .iter()
            .filter(|((owner, _), _)| *owner == function)
            .map(|((_, parameter), elem)| (*parameter, *elem))
            .collect::<Vec<_>>();
        for (parameter, elem) in cells {
            if parameter as usize >= parameters {
                return Err("a shared capture parameter outside its function".into());
            }
            let cell = self.cell_type(elem)?;
            self.cells.insert(first + parameter, cell);
        }
        Ok(())
    }

    /// Only an authenticated receiving parameter permits passing the holder itself.
    pub(super) fn function_argument(
        &mut self,
        function: FunId,
        parameter: u32,
        argument: u32,
    ) -> Result<(), Unsupported> {
        let declared = *self.ir.functions[function as usize]
            .params
            .get(parameter as usize)
            .ok_or("a shared capture parameter outside its function")?;
        match self
            .ir
            .shared_capture_parameters
            .get(&(function, parameter))
        {
            Some(&elem) => self.cell_holder(argument, elem).map(|_| ()),
            None => self.value(argument, declared),
        }
    }

    /// Declare value `index` as the cell `init` (an [`IrExpr::RefNew`]) creates.
    pub(super) fn shared_cell_variable(
        &mut self,
        index: u32,
        init: u32,
    ) -> Result<(), Unsupported> {
        let IrExpr::RefNew { elem, init } = self.ir.expr(init).clone() else {
            return Err("a shared cell declared without its cell".to_string());
        };
        let cell = self.cell_type(elem)?;
        let local = self.local(ValType::nullable(cell));
        match init {
            Some(value) => {
                self.value(value, elem)?;
                self.code.struct_new(cell);
            }
            None => {
                self.code.struct_new_default(cell);
            }
        }
        self.code.local_set(local);
        self.values.insert(index, (local, elem));
        self.cells.insert(index, cell);
        Ok(())
    }

    /// `holder.element`.
    pub(super) fn shared_cell_read(&mut self, holder: u32, elem: Ty) -> Result<(), Unsupported> {
        let cell = self.cell_holder(holder, elem)?;
        self.code.struct_get(cell, 0);
        Ok(())
    }

    /// `holder.element = value` (the write `id`). The write leaves the stored value on the stack
    /// only when its own type carries one: a source assignment is checked as `Unit` and leaves
    /// nothing, while an unchecked write evaluates to what it stored (see `expression_type`).
    pub(super) fn shared_cell_write(
        &mut self,
        id: u32,
        holder: u32,
        elem: Ty,
        value: u32,
    ) -> Result<(), Unsupported> {
        let cell = self.cell_holder(holder, elem)?;
        let carried = carrier(elem)?.ok_or_else(|| "a shared cell of `Unit`".to_string())?;
        match self.carrier_of(id)? {
            None => {
                self.value(value, elem)?;
                self.code.struct_set(cell, 0);
            }
            Some(result) if result == carried => {
                let stored = self.local(carried);
                self.value(value, elem)?;
                self.code.local_tee(stored);
                self.code.struct_set(cell, 0);
                self.code.local_get(stored);
            }
            Some(_) => {
                return Err("a shared-cell write answering another type than its element".into());
            }
        }
        Ok(())
    }

    /// Pass the cell `argument` into a constructor parameter that stores it.
    pub(super) fn pass_shared_capture(
        &mut self,
        argument: u32,
        element: Ty,
    ) -> Result<(), Unsupported> {
        self.cell_holder(argument, element)?;
        Ok(())
    }

    /// Push the cell `holder` reads, returning its struct type.
    fn cell_holder(&mut self, holder: u32, elem: Ty) -> Result<u32, Unsupported> {
        if let IrExpr::GetField {
            receiver,
            class,
            index,
        } = self.ir.expr(holder)
        {
            let recorded = self
                .ir
                .shared_class_capture_fields
                .get(&(*class, *index))
                .copied();
            if recorded != Some(elem) {
                return Err("a shared cell reached through another holder".to_string());
            }
            let class = *class;
            let index = *index;
            let receiver = *receiver;
            self.field_read(receiver, class, index)?;
            return self.cell_type(elem);
        }
        let IrExpr::GetValue(slot) = self.ir.expr(holder) else {
            return Err("a shared cell reached through another holder".to_string());
        };
        let cell =
            self.cells.get(slot).copied().ok_or_else(|| {
                format!("a shared-cell access of value {slot}, which holds no cell")
            })?;
        if cell != self.cell_type(elem)? {
            return Err(format!(
                "a shared-cell access of value {slot} at another element type"
            ));
        }
        let (local, _) = self.values[slot];
        self.code.local_get(local);
        Ok(cell)
    }

    /// The struct type of a cell holding `elem`.
    fn cell_type(&mut self, elem: Ty) -> Result<u32, Unsupported> {
        cell_type(self.module, elem)
    }
}

#[cfg(test)]
mod tests {
    use crate::target::WasmTarget;
    use krusty::ir::{Callee, IrConst, IrExpr, IrFile, IrFunction};
    use krusty::types::Ty;

    /// The encoded module of one function whose body declares the cell `0` holding an `Int`,
    /// writes `1` into it, and either ends there or answers the write: a source assignment
    /// statement is checked as `Unit`, while an unchecked write evaluates to what it stored.
    fn module_of_one_write(checked: Option<Ty>, ret: Ty) -> Vec<u8> {
        let mut ir = IrFile::default();
        let zero = ir.add_expr(IrExpr::Const(IrConst::Int(0)));
        let cell = ir.add_expr(IrExpr::RefNew {
            elem: Ty::Int,
            init: Some(zero),
        });
        let declaration = ir.add_expr(IrExpr::Variable {
            index: 0,
            ty: Ty::Int,
            init: Some(cell),
            named: true,
        });
        let holder = ir.add_expr(IrExpr::GetValue(0));
        let one = ir.add_expr(IrExpr::Const(IrConst::Int(1)));
        let write = ir.add_expr(IrExpr::RefSet {
            holder,
            elem: Ty::Int,
            value: one,
        });
        if let Some(checked) = checked {
            ir.set_logical_type(write, checked);
        }
        let (stmts, value) = if ret == Ty::Unit {
            (vec![declaration, write], None)
        } else {
            (vec![declaration], Some(write))
        };
        let body = ir.add_expr(IrExpr::Block { stmts, value });
        ir.add_fun(IrFunction {
            name: "write".to_string(),
            params: Vec::new(),
            ret,
            body: Some(body),
            is_static: true,
            dispatch_receiver: None,
            param_checks: Vec::new(),
        });
        let mut started = super::super::super::Started::new(WasmTarget::Wasi);
        let properties = krusty::backend::local_properties::realize(&mut ir)
            .expect("the body declares no local delegated property");
        super::super::lower_alone(&ir, &properties, &mut started).expect("the cell body lowers");
        started.module.encode()
    }

    /// Whether `bytes` holds `struct.set <any type> 0` directly followed by `tail`.
    fn has_struct_set_followed_by(bytes: &[u8], tail: impl Fn(&[u8]) -> bool) -> bool {
        (0..bytes.len().saturating_sub(4)).any(|at| {
            bytes[at..].starts_with(&[0xfb, 0x05])
                && bytes[at + 3] == 0x00
                && tail(&bytes[at + 4..])
        })
    }

    #[test]
    fn a_write_checked_as_unit_leaves_nothing_on_the_stack() {
        let bytes = module_of_one_write(Some(Ty::Unit), Ty::Unit);
        assert!(has_struct_set_followed_by(&bytes, |rest| rest.first() == Some(&0x0b)));
        assert!(!has_struct_set_followed_by(&bytes, |rest| rest.first() == Some(&0x20)));
    }

    #[test]
    fn an_unchecked_write_answers_the_value_it_stored() {
        let bytes = module_of_one_write(None, Ty::Int);
        assert!(has_struct_set_followed_by(&bytes, |rest| {
            rest.first() == Some(&0x20) && rest.get(2) == Some(&0x0b)
        }));
    }

    /// Backend ABI fixture: one source cell passed to a local function, or a function value
    /// whose explicit argument is incorrectly marked as a capture. No source-shape inference
    /// supplies the parameter fact.
    fn lower_handoff(
        recorded: Option<Ty>,
        read_cell: bool,
        uncaptured_lambda: bool,
    ) -> Result<(), String> {
        let mut ir = IrFile::default();
        let parameter = ir.add_expr(IrExpr::GetValue(0));
        let body = if read_cell {
            ir.add_expr(IrExpr::RefGet {
                holder: parameter,
                elem: Ty::Int,
            })
        } else {
            ir.add_expr(IrExpr::Const(IrConst::Int(7)))
        };
        let function = ir.add_fun(IrFunction {
            name: "read".into(),
            params: vec![Ty::Int],
            ret: Ty::Int,
            body: Some(body),
            is_static: true,
            dispatch_receiver: None,
            param_checks: Vec::new(),
        });
        if let Some(elem) = recorded {
            ir.shared_capture_parameters.insert((function, 0), elem);
        }
        let initial = ir.add_expr(IrExpr::Const(IrConst::Int(7)));
        let cell = ir.add_expr(IrExpr::RefNew {
            elem: Ty::Int,
            init: Some(initial),
        });
        let variable = ir.add_expr(IrExpr::Variable {
            index: 0,
            ty: Ty::Int,
            init: Some(cell),
            named: true,
        });
        let holder = ir.add_expr(IrExpr::GetValue(0));
        let call = if uncaptured_lambda {
            ir.add_expr(IrExpr::Lambda {
                impl_fn: function,
                arity: 1,
                captures: Vec::new(),
                sam: None,
                inline_body: None,
            })
        } else {
            ir.add_expr(IrExpr::Call {
                callee: Box::new(Callee::Local(function)),
                dispatch_receiver: None,
                args: vec![holder],
            })
        };
        ir.set_logical_type(
            call,
            if uncaptured_lambda {
                Ty::fun(vec![Ty::Int], Ty::Int)
            } else {
                Ty::Int
            },
        );
        let body = ir.add_expr(IrExpr::Block {
            stmts: vec![variable, call],
            value: None,
        });
        ir.add_fun(IrFunction {
            name: "caller".into(),
            params: Vec::new(),
            ret: Ty::Unit,
            body: Some(body),
            is_static: true,
            dispatch_receiver: None,
            param_checks: Vec::new(),
        });
        let properties = krusty::backend::local_properties::realize(&mut ir)
            .expect("no local delegated properties");
        let mut started = super::super::super::Started::new(WasmTarget::Wasi);
        super::super::lower_alone(&ir, &properties, &mut started).map(|_| ())
    }

    #[test]
    fn a_recorded_parameter_receives_the_existing_cell_through_a_direct_call() {
        assert_eq!(lower_handoff(Some(Ty::Int), true, false), Ok(()));
    }

    #[test]
    fn an_unmarked_parameter_does_not_admit_a_cell_argument() {
        assert_eq!(
            lower_handoff(None, false, false).unwrap_err(),
            "a shared cell read as a value"
        );
    }

    #[test]
    fn a_recorded_parameter_cannot_reinterpret_another_cell_carrier() {
        assert_eq!(
            lower_handoff(Some(Ty::Long), false, false).unwrap_err(),
            "a shared-cell access of value 0 at another element type"
        );
    }

    #[test]
    fn a_unit_cell_parameter_keeps_the_existing_capability_boundary() {
        assert_eq!(
            lower_handoff(Some(Ty::Unit), false, false).unwrap_err(),
            "a shared cell of `Unit`"
        );
    }

    #[test]
    fn a_function_values_own_argument_cannot_silently_take_a_cell_abi() {
        assert_eq!(
            lower_handoff(Some(Ty::Int), true, true).unwrap_err(),
            "a function value with a shared parameter outside its captures"
        );
    }
}
