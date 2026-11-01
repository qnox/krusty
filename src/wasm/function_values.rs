//! Function values: what a lambda or a callable reference is, and how one is called.
//!
//! A function value is an object. Common lowering has already moved its body into a function of
//! the file whose LEADING parameters are the captured values and whose trailing ones are the
//! lambda's own, so the object only holds the captures. Its dispatch table extends `$AnyTable` with
//! one more field, the function a call enters.
//!
//! **That function takes and returns references**, as `FunctionN.invoke` does on the JVM: a call
//! site knows the value's arity and its Kotlin type, not which lambda it holds, so every function
//! value of one arity is called one way. The entry unboxes each argument to the type the body
//! declares and boxes the result (`super::boxes`); the body itself keeps its own types.

use crate::fx_hash::HashMap;

use super::encode::{CompositeType, Module, SubType, ValType};
use super::objects::{immutable, ITABLE_FIELD, REFERENCE};
use super::runtime::Runtime;

/// The dispatch table field holding a function value's entry: after the interface table.
pub(super) const INVOKE_FIELD: u32 = ITABLE_FIELD + 1;

/// The table type of every function value of one arity, and the type of its entry.
#[derive(Clone, Copy)]
pub(super) struct FunctionShape {
    pub(super) table: u32,
    pub(super) invoke: u32,
}

/// The function shapes this module has declared, by arity.
#[derive(Default)]
pub(super) struct FunctionShapes {
    by_arity: HashMap<usize, FunctionShape>,
}

impl Runtime {
    /// The shape of a function value taking `arity` arguments, declared on first use.
    pub(super) fn function_shape(&mut self, module: &mut Module, arity: usize) -> FunctionShape {
        if let Some(shape) = self.functions.by_arity.get(&arity) {
            return *shape;
        }
        let objects = &self.objects;
        // The receiver, which is the function value itself, then the arguments.
        let invoke = module.func_type(vec![REFERENCE; arity + 1], vec![REFERENCE]);
        let fields = objects
            .any_slot_types
            .iter()
            .map(|ty| immutable(ValType::reference(*ty)))
            .chain([
                immutable(ValType::nullable(objects.itable)),
                immutable(ValType::reference(invoke)),
            ])
            .collect();
        let table = module.subtype(SubType {
            composite: CompositeType::Struct(fields),
            supertype: Some(objects.any_table),
            open: false,
        });
        let shape = FunctionShape { table, invoke };
        self.functions.by_arity.insert(arity, shape);
        shape
    }
}
