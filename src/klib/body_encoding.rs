//! The wire encoding of a [`super::body_tree`] body into one IR file's tables.
//!
//! Field numbers follow Kotlin 2.4's `KotlinIr.proto`, the layout [`crate::metadata::klib_ir`]
//! decodes: an `IrExpression` carries its type in field 2 and its operation in the field the
//! operation's kind owns.

use super::body_tree::{
    BodyExprId, BodyKind, BodyStatement, BodyVariable, CallTarget, FunctionBody, ValueRef,
};
use super::ir_tables::{lattice, symbol, IrTables, FUNCTION_SYMBOL};
use crate::fx_hash::HashMap;
use crate::ir::IrConst;
use crate::metadata::protobuf::Pb;
use crate::types::Ty;

const VARIABLE_SYMBOL: u64 = 8;

/// Writes the bodies of one function into `tables`.
pub(super) struct BodyEncoder<'t> {
    pub(super) tables: &'t mut IrTables,
    /// The function's own symbol, which its `return`s target.
    pub(super) function: u64,
    /// The symbol of each physical parameter slot.
    pub(super) parameters: &'t [u64],
    /// Each type parameter in scope with its symbol.
    pub(super) type_parameters: &'t [(Ty, u64)],
    locals: HashMap<u32, u64>,
    temporaries: u32,
}

impl<'t> BodyEncoder<'t> {
    pub(super) fn new(
        tables: &'t mut IrTables,
        function: u64,
        parameters: &'t [u64],
        type_parameters: &'t [(Ty, u64)],
    ) -> Self {
        Self {
            tables,
            function,
            parameters,
            type_parameters,
            locals: HashMap::default(),
            temporaries: 0,
        }
    }

    /// The block body `body`, as an entry of the body table.
    pub(super) fn block_body(&mut self, body: &FunctionBody) -> u32 {
        let mut block = Pb::new();
        for statement in &body.statements {
            let statement = self.statement(body, statement);
            block.repeated_message(1, &statement);
        }
        let mut body = Pb::new();
        body.field_message(4, &block);
        self.tables.body(body.canonical().into_bytes())
    }

    fn statement(&mut self, body: &FunctionBody, statement: &BodyStatement) -> Pb {
        let mut encoded = Pb::new();
        match statement {
            BodyStatement::Expression(expression) => {
                let expression = self.expression(body, *expression);
                encoded.field_message(3, &expression);
            }
            BodyStatement::Variable(variable) => {
                let variable = self.variable(body, variable);
                let mut declaration = Pb::new();
                declaration.field_message(9, &variable);
                encoded.field_message(2, &declaration);
            }
        }
        encoded
    }

    fn variable(&mut self, body: &FunctionBody, variable: &BodyVariable) -> Pb {
        let signature = self.tables.local_signature();
        let declared = symbol(signature, VARIABLE_SYMBOL);
        let initializer = variable
            .initializer
            .as_ref()
            .map(|initializer| self.expression(body, *initializer));
        // The initializer runs before the variable is in scope, so it is encoded first.
        self.locals.insert(variable.local, declared);
        let (name, origin) = match &variable.name {
            Some(name) => (name.clone(), "DEFINED"),
            None => {
                let name = format!("tmp{}", self.temporaries);
                self.temporaries += 1;
                (name, "IR_TEMPORARY_VARIABLE")
            }
        };
        let mut base = Pb::new();
        base.field_varint(1, declared);
        base.field_varint(2, u64::from(self.tables.string(origin)));
        if variable.mutable {
            base.field_varint(4, crate::klib_lowering::local_values::IS_VAR);
        }
        let ty = self.ty(variable.ty);
        let name = self.tables.string(&name);
        let mut encoded = Pb::new();
        encoded.field_message(1, &base);
        encoded.field_varint(2, lattice(name, ty));
        if let Some(initializer) = initializer {
            encoded.field_message(3, &initializer);
        }
        encoded
    }

    fn expression(&mut self, body: &FunctionBody, expression: BodyExprId) -> Pb {
        let expression = body.expr(expression);
        let ty = self.ty(expression.ty);
        let mut encoded = Pb::new();
        encoded.field_varint(2, u64::from(ty));
        let (field, operation) = self.operation(body, &expression.kind);
        encoded.field_message(field, &operation);
        encoded
    }

    fn operation(&mut self, body: &FunctionBody, kind: &BodyKind) -> (u32, Pb) {
        let mut operation = Pb::new();
        let field = match kind {
            BodyKind::Const(constant) => {
                self.constant(constant, &mut operation);
                5
            }
            BodyKind::GetValue(value) => {
                operation.field_varint(1, self.value(*value));
                6
            }
            BodyKind::SetValue { local, value } => {
                operation.field_varint(1, self.value(ValueRef::Local(*local)));
                let value = self.expression(body, *value);
                operation.field_message(2, &value);
                operation.field_varint(3, u64::from(self.tables.string("EQ")));
                7
            }
            BodyKind::Call {
                target,
                type_arguments,
                arguments,
                origin,
            } => {
                let signature = match target {
                    CallTarget::Public(signature) => self.tables.public_signature(signature),
                    CallTarget::Accessor(signature) => self.tables.accessor_signature(signature),
                };
                operation.field_varint(1, symbol(signature, FUNCTION_SYMBOL));
                if let Some(origin) = origin {
                    operation.field_varint(4, u64::from(self.tables.string(origin)));
                }
                for argument in arguments {
                    let argument = self.expression(body, *argument);
                    operation.repeated_message(5, &argument);
                }
                let type_arguments = type_arguments
                    .iter()
                    .map(|ty| u64::from(self.ty(*ty)))
                    .collect::<Vec<_>>();
                operation.field_packed_varints(6, &type_arguments);
                8
            }
            BodyKind::Block { statements, origin } => {
                for statement in statements {
                    let statement = self.statement(body, statement);
                    operation.repeated_message(1, &statement);
                }
                if let Some(origin) = origin {
                    operation.field_varint(2, u64::from(self.tables.string(origin)));
                }
                10
            }
            BodyKind::Return(value) => {
                operation.field_varint(1, self.function);
                let value = self.expression(body, *value);
                operation.field_message(2, &value);
                12
            }
            BodyKind::When { branches, origin } => {
                for (condition, result) in branches {
                    let mut branch = Pb::new();
                    let condition = self.expression(body, *condition);
                    branch.field_message(1, &condition);
                    let result = self.expression(body, *result);
                    branch.field_message(2, &result);
                    let mut statement = Pb::new();
                    statement.field_message(5, &branch);
                    operation.repeated_message(1, &statement);
                }
                if let Some(origin) = origin {
                    operation.field_varint(2, u64::from(self.tables.string(origin)));
                }
                13
            }
            BodyKind::TypeOperator {
                operator,
                operand,
                argument,
            } => {
                operation.field_varint(1, *operator as u64);
                let operand = self.ty(*operand);
                operation.field_varint(2, u64::from(operand));
                let argument = self.expression(body, *argument);
                operation.field_message(3, &argument);
                14
            }
            BodyKind::GetObject(class) => {
                operation.field_varint(1, self.tables.class_symbol(*class));
                17
            }
            BodyKind::StringConcat(parts) => {
                for part in parts {
                    let part = self.expression(body, *part);
                    operation.repeated_message(1, &part);
                }
                29
            }
        };
        (field, operation)
    }

    fn constant(&mut self, constant: &IrConst, encoded: &mut Pb) {
        match constant {
            IrConst::Null => encoded.field_varint(1, 0),
            IrConst::Boolean(value) => encoded.field_varint(2, u64::from(*value)),
            IrConst::Int(value) => encoded.field_varint(6, i64::from(*value) as u64),
            IrConst::Long(value) => encoded.field_varint(7, *value as u64),
            IrConst::Double(value) => encoded.field_fixed64(9, value.to_bits()),
            IrConst::String(value) => {
                let index = self.tables.string(
                    value
                        .as_str()
                        .expect("the tree admits only well-formed strings"),
                );
                encoded.field_varint(10, u64::from(index));
            }
            _ => unreachable!("the tree admits no other constant"),
        }
    }

    fn value(&self, value: ValueRef) -> u64 {
        match value {
            ValueRef::Parameter(slot) => self.parameters[slot as usize],
            ValueRef::Local(local) => self.locals[&local],
        }
    }

    pub(super) fn ty(&mut self, ty: Ty) -> u32 {
        let ty = if ty == Ty::Null {
            Ty::nullable(Ty::Nothing)
        } else {
            ty
        };
        self.tables.ty(ty, self.type_parameters)
    }
}
