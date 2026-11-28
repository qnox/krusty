//! The wire encoding of a [`super::body_tree`] body into one IR file's tables.
//!
//! Field numbers follow Kotlin 2.4's `KotlinIr.proto`, the layout [`crate::metadata::klib_ir`]
//! decodes: an `IrExpression` carries its type in field 2 and its operation in the field the
//! operation's kind owns.

use super::body_tree::{
    BodyCatch, BodyExprId, BodyKind, BodyLambda, BodyLoop, BodyStatement, BodyVariable, CallTarget,
    FunctionBody, JumpKind, ValueRef,
};
use super::ir_tables::{
    lattice, symbol, IrTables, CLASS_SYMBOL, CONSTRUCTOR_SYMBOL, FIELD_SYMBOL, FUNCTION_SYMBOL,
    VALUE_PARAMETER_SYMBOL,
};
use crate::fx_hash::HashMap;
use crate::ir::IrConst;
use crate::metadata::protobuf::Pb;
use crate::types::Ty;

const VARIABLE_SYMBOL: u64 = 8;
/// A local function's flags: `local` visibility, `final` modality.
const LOCAL_FUNCTION_FLAGS: u64 = 5 << 1;

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
    /// The file-wide id of each of the body's loops.
    loops: HashMap<u32, u32>,
    /// The symbol of each lambda's local function, and of each of its value parameters.
    lambdas: HashMap<u32, u64>,
    lambda_parameters: HashMap<(u32, u32), u64>,
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
            loops: HashMap::default(),
            lambdas: HashMap::default(),
            lambda_parameters: HashMap::default(),
            temporaries: 0,
        }
    }

    /// The block body `body`, as an entry of the body table.
    pub(super) fn block_body(&mut self, body: &FunctionBody) -> u32 {
        self.statements_body(body, &body.statements)
    }

    /// A block body of `statements`, as an entry of the body table.
    fn statements_body(&mut self, body: &FunctionBody, statements: &[BodyStatement]) -> u32 {
        let mut block = Pb::new();
        for statement in statements {
            let statement = self.statement(body, statement);
            block.repeated_message(1, &statement);
        }
        let mut encoded = Pb::new();
        encoded.field_message(4, &block);
        self.tables.body(encoded.canonical().into_bytes())
    }

    /// The expression `expression` of `body`, as an entry of the body table.
    pub(super) fn expression_body(&mut self, body: &FunctionBody, expression: BodyExprId) -> u32 {
        let expression = self.expression(body, expression);
        self.tables.body(expression.canonical().into_bytes())
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
        let origin = variable.origin;
        let name = match &variable.name {
            Some(name) => name.clone(),
            None => {
                let name = format!("tmp{}", self.temporaries);
                self.temporaries += 1;
                name
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
            BodyKind::GetFunctionValue(value) => {
                operation.field_varint(1, self.value(*value));
                let origin = self.tables.string("VARIABLE_AS_FUNCTION");
                operation.field_varint(2, u64::from(origin));
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
                operation.field_varint(1, self.callee(target, FUNCTION_SYMBOL));
                if let Some(origin) = origin {
                    operation.field_varint(4, u64::from(self.tables.string(origin)));
                }
                self.arguments(body, arguments, 5, &mut operation);
                self.type_arguments(type_arguments, 6, &mut operation);
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
            BodyKind::ConstructorCall {
                target,
                constructor_type_arguments,
                type_arguments,
                arguments,
                origin,
            } => {
                operation.field_varint(1, self.callee(target, CONSTRUCTOR_SYMBOL));
                operation.field_varint(2, u64::from(*constructor_type_arguments));
                if let Some(origin) = origin {
                    operation.field_varint(4, u64::from(self.tables.string(origin)));
                }
                self.arguments(body, arguments, 5, &mut operation);
                self.type_arguments(type_arguments, 6, &mut operation);
                9
            }
            BodyKind::DelegatingConstructorCall {
                target,
                type_arguments,
                arguments,
            } => {
                operation.field_varint(1, self.callee(target, CONSTRUCTOR_SYMBOL));
                self.arguments(body, arguments, 3, &mut operation);
                self.type_arguments(type_arguments, 4, &mut operation);
                26
            }
            BodyKind::InstanceInitializerCall(class) => {
                let signature =
                    crate::metadata::id_signature::KlibPublicIdSignature::classifier(*class);
                let class = self.tables.public_signature(&signature);
                operation.field_varint(1, symbol(class, CLASS_SYMBOL));
                28
            }
            BodyKind::GetField { property, receiver } => {
                let access = self.field_access(body, property, *receiver);
                operation.field_message(1, &access);
                15
            }
            BodyKind::SetField {
                property,
                receiver,
                value,
            } => {
                let access = self.field_access(body, property, *receiver);
                operation.field_message(1, &access);
                let value = self.expression(body, *value);
                operation.field_message(2, &value);
                16
            }
            BodyKind::StringConcat(parts) => {
                for part in parts {
                    let part = self.expression(body, *part);
                    operation.repeated_message(1, &part);
                }
                29
            }
            BodyKind::Loop(found) => {
                let encoded = self.loop_(body, found);
                operation.field_message(1, &encoded);
                if found.post_test {
                    25
                } else {
                    24
                }
            }
            BodyKind::Jump { kind, loop_id } => {
                operation.field_varint(1, u64::from(self.loops[loop_id]));
                match kind {
                    JumpKind::Break => 22,
                    JumpKind::Continue => 23,
                }
            }
            BodyKind::Vararg { element, elements } => {
                let element = self.ty(*element);
                operation.field_varint(1, u64::from(element));
                for (value, spread) in elements {
                    let value = self.expression(body, *value);
                    let mut encoded = Pb::new();
                    if *spread {
                        let mut spread = Pb::new();
                        spread.field_message(1, &value);
                        encoded.field_message(2, &spread);
                    } else {
                        encoded.field_message(1, &value);
                    }
                    operation.repeated_message(2, &encoded);
                }
                32
            }
            BodyKind::Lambda(lambda) => {
                let overridden = self.tables.public_signature(&lambda.overridden);
                if let Some(target) = &lambda.reflection_target {
                    let signature = match target {
                        CallTarget::Public(signature) => self.tables.public_signature(signature),
                        CallTarget::Accessor(signature) => {
                            self.tables.accessor_signature(signature)
                        }
                    };
                    operation.field_varint(2, symbol(signature, FUNCTION_SYMBOL));
                }
                operation.field_varint(3, symbol(overridden, FUNCTION_SYMBOL));
                let invoke = self.local_function(body, lambda);
                operation.field_message(4, &invoke);
                if let Some(origin) = lambda.origin {
                    operation.field_varint(6, u64::from(self.tables.string(origin)));
                }
                41
            }
            BodyKind::LambdaReturn { lambda, value } => {
                operation.field_varint(1, self.lambdas[lambda]);
                let value = self.expression(body, *value);
                operation.field_message(2, &value);
                12
            }
            BodyKind::Throw(value) => {
                let value = self.expression(body, *value);
                operation.field_message(1, &value);
                30
            }
            BodyKind::Try {
                body: result,
                catches,
                finally,
            } => {
                let result = self.expression(body, *result);
                operation.field_message(1, &result);
                for catch in catches {
                    let catch = self.catch(body, catch);
                    let mut statement = Pb::new();
                    statement.field_message(6, &catch);
                    operation.repeated_message(2, &statement);
                }
                if let Some(finally) = finally {
                    let finally = self.expression(body, *finally);
                    operation.field_message(3, &finally);
                }
                31
            }
        };
        (field, operation)
    }

    fn callee(&mut self, target: &CallTarget, kind: u64) -> u64 {
        let signature = match target {
            CallTarget::Public(signature) => self.tables.public_signature(signature),
            CallTarget::Accessor(signature) => self.tables.accessor_signature(signature),
        };
        symbol(signature, kind)
    }

    fn arguments(
        &mut self,
        body: &FunctionBody,
        arguments: &[BodyExprId],
        field: u32,
        into: &mut Pb,
    ) {
        for argument in arguments {
            let argument = self.expression(body, *argument);
            into.repeated_message(field, &argument);
        }
    }

    fn type_arguments(&mut self, type_arguments: &[Ty], field: u32, into: &mut Pb) {
        let type_arguments = type_arguments
            .iter()
            .map(|ty| u64::from(self.ty(*ty)))
            .collect::<Vec<_>>();
        into.field_packed_varints(field, &type_arguments);
    }

    /// `FieldAccessCommon`: the field's symbol and the instance it belongs to.
    fn field_access(
        &mut self,
        body: &FunctionBody,
        property: &crate::metadata::id_signature::KlibPublicIdSignature,
        receiver: Option<BodyExprId>,
    ) -> Pb {
        let field = self.tables.field_signature(property);
        let mut access = Pb::new();
        access.field_varint(1, symbol(field, FIELD_SYMBOL));
        if let Some(receiver) = receiver {
            let receiver = self.expression(body, receiver);
            access.field_message(3, &receiver);
        }
        access
    }

    /// `IrLoop`: its id, condition, body and origin. The condition is encoded after the body, whose
    /// variables a `do`-`while` condition reads.
    fn loop_(&mut self, body: &FunctionBody, found: &BodyLoop) -> Pb {
        let id = self.tables.loop_id();
        self.loops.insert(found.id, id);
        let loop_body = self.expression(body, found.body);
        let condition = self.expression(body, found.condition);
        let mut encoded = Pb::new();
        encoded.field_varint(1, u64::from(id));
        encoded.field_message(2, &condition);
        encoded.field_message(4, &loop_body);
        encoded.field_varint(5, u64::from(self.tables.string(found.origin)));
        encoded
    }

    /// The local function a lambda invokes: `IrFunction`, a local final function whose body
    /// returns to it.
    fn local_function(&mut self, body: &FunctionBody, lambda: &BodyLambda) -> Pb {
        let function = symbol(self.tables.local_signature(), FUNCTION_SYMBOL);
        self.lambdas.insert(lambda.id, function);
        let mut base = Pb::new();
        base.field_varint(1, function);
        base.field_varint(
            2,
            u64::from(self.tables.string("LOCAL_FUNCTION_FOR_LAMBDA")),
        );
        base.field_varint(4, LOCAL_FUNCTION_FLAGS);
        let result = self.ty(lambda.result);
        let name = self.tables.string(&lambda.name);
        let mut function_base = Pb::new();
        function_base.field_message(1, &base);
        function_base.field_varint(2, lattice(name, result));
        for (index, (name, ty)) in (0u32..).zip(&lambda.parameters) {
            let parameter = symbol(self.tables.local_signature(), VALUE_PARAMETER_SYMBOL);
            self.lambda_parameters.insert((lambda.id, index), parameter);
            let mut base = Pb::new();
            base.field_varint(1, parameter);
            base.field_varint(2, u64::from(self.tables.string("DEFINED")));
            let ty = self.ty(*ty);
            let name = self.tables.string(name);
            let mut encoded = Pb::new();
            encoded.field_message(1, &base);
            encoded.field_varint(2, lattice(name, ty));
            function_base.repeated_message(6, &encoded);
        }
        let entry = self.statements_body(body, &lambda.statements);
        function_base.field_varint(7, u64::from(entry));
        let mut encoded = Pb::new();
        encoded.field_message(1, &function_base);
        encoded
    }

    /// `IrCatch`: the parameter, declared before the result that reads it.
    fn catch(&mut self, body: &FunctionBody, catch: &BodyCatch) -> Pb {
        let parameter = self.variable(body, &catch.parameter);
        let result = self.expression(body, catch.result);
        let mut encoded = Pb::new();
        encoded.field_message(1, &parameter);
        encoded.field_message(2, &result);
        encoded
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
            ValueRef::LambdaParameter { lambda, index } => self.lambda_parameters[&(lambda, index)],
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
