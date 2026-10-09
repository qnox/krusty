//! The decoded form of one serialized KLIB IR declaration body.
//!
//! This is Kotlin's backend IR as a KLIB stores it, made addressable: every expression, type and
//! local declaration lives in one arena owned by the top-level declaration that holds it, and
//! refers to the others by index. Nothing here is interpreted. Symbols stay exact serialized
//! identities ([`KlibIrSymbol`]), origins stay the serializer's strings, and flags stay raw, so
//! a consumer joining this to a semantic model decides what each fact means and declines what it
//! does not model.

use super::symbols::KlibIrSymbol;
use super::KlibIrConstant;

macro_rules! arena_id {
    ($name:ident) => {
        #[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
        pub struct $name(pub(super) u32);

        impl $name {
            pub fn index(self) -> usize {
                self.0 as usize
            }
        }
    };
}

arena_id!(KlibIrExprId);
arena_id!(KlibIrTypeId);
arena_id!(KlibIrFunctionId);
arena_id!(KlibIrClassId);
arena_id!(KlibIrVariableId);

/// Every node of one top-level declaration's bodies.
#[derive(Debug, Default)]
pub struct KlibIrArena {
    pub(super) exprs: Vec<KlibIrExpr>,
    pub(super) types: Vec<KlibIrType>,
    pub(super) functions: Vec<KlibIrFunction>,
    pub(super) classes: Vec<KlibIrClass>,
    pub(super) variables: Vec<KlibIrVariable>,
}

impl KlibIrArena {
    pub fn expr(&self, id: KlibIrExprId) -> &KlibIrExpr {
        &self.exprs[id.index()]
    }

    pub fn ty(&self, id: KlibIrTypeId) -> &KlibIrType {
        &self.types[id.index()]
    }

    pub fn function(&self, id: KlibIrFunctionId) -> &KlibIrFunction {
        &self.functions[id.index()]
    }

    pub fn class(&self, id: KlibIrClassId) -> &KlibIrClass {
        &self.classes[id.index()]
    }

    pub fn variable(&self, id: KlibIrVariableId) -> &KlibIrVariable {
        &self.variables[id.index()]
    }

    pub fn expr_count(&self) -> usize {
        self.exprs.len()
    }

    pub fn function_count(&self) -> usize {
        self.functions.len()
    }

    /// The `index`th function of this arena, in decoding order.
    pub fn function_at(&self, index: usize) -> &KlibIrFunction {
        &self.functions[index]
    }
}

/// `IrType`.
#[derive(Clone, Debug, PartialEq)]
pub enum KlibIrType {
    Simple {
        classifier: KlibIrSymbol,
        nullability: KlibIrNullability,
        arguments: Vec<KlibIrTypeArgument>,
        /// Constructors of the type's annotations (`@ExtensionFunctionType`, ...).
        annotations: Vec<KlibIrSymbol>,
    },
    DefinitelyNotNull(KlibIrTypeId),
    Dynamic,
    Error,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KlibIrNullability {
    MarkedNullable,
    NotSpecified,
    DefinitelyNotNull,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KlibIrVariance {
    Invariant,
    In,
    Out,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KlibIrTypeArgument {
    Star,
    Type {
        variance: KlibIrVariance,
        ty: KlibIrTypeId,
    },
}

/// One expression and the type the serializer recorded for it, when it recorded one.
#[derive(Clone, Debug, PartialEq)]
pub struct KlibIrExpr {
    pub ty: Option<KlibIrTypeId>,
    pub kind: KlibIrExprKind,
}

/// Arguments of a member access, in whichever layout the serializer used.
#[derive(Clone, Debug, PartialEq)]
pub enum KlibIrArguments {
    /// Kotlin 2.4: one argument per callee parameter, in the callee's parameter order (dispatch
    /// receiver, context parameters, extension receiver, regular parameters). `None` is an
    /// argument the call does not supply.
    Flat(Vec<Option<KlibIrExprId>>),
    /// Before Kotlin 2.4: receivers apart from the value arguments.
    Split {
        dispatch_receiver: Option<KlibIrExprId>,
        extension_receiver: Option<KlibIrExprId>,
        values: Vec<Option<KlibIrExprId>>,
    },
}

/// A member access: the callee, its arguments and its explicit type arguments.
#[derive(Clone, Debug, PartialEq)]
pub struct KlibIrMemberAccess {
    pub symbol: KlibIrSymbol,
    pub arguments: KlibIrArguments,
    /// `None` is a type argument the serializer left unset.
    pub type_arguments: Vec<Option<KlibIrTypeId>>,
    pub origin: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KlibIrTypeOperator {
    Cast,
    ImplicitCast,
    ImplicitNotNull,
    ImplicitCoercionToUnit,
    ImplicitIntegerCoercion,
    SafeCast,
    InstanceOf,
    NotInstanceOf,
    SamConversion,
    ImplicitDynamicCast,
    ReinterpretCast,
}

/// `Loop`, shared by `while` and `do-while`.
#[derive(Clone, Debug, PartialEq)]
pub struct KlibIrLoop {
    /// File-unique identity that `break` and `continue` name.
    pub id: u32,
    pub condition: KlibIrExprId,
    pub body: Option<KlibIrExprId>,
    pub label: Option<String>,
    pub origin: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct KlibIrBranch {
    pub condition: KlibIrExprId,
    pub result: KlibIrExprId,
}

#[derive(Clone, Debug, PartialEq)]
pub struct KlibIrCatch {
    pub parameter: KlibIrVariableId,
    pub result: KlibIrExprId,
}

#[derive(Clone, Debug, PartialEq)]
pub enum KlibIrVarargElement {
    Expression(KlibIrExprId),
    Spread(KlibIrExprId),
}

/// A statement of a block or body.
#[derive(Clone, Debug, PartialEq)]
pub enum KlibIrStatement {
    Expression(KlibIrExprId),
    Variable(KlibIrVariableId),
    Function(KlibIrFunctionId),
    Class(KlibIrClassId),
    LocalDelegatedProperty(KlibIrLocalDelegatedProperty),
    TypeAlias(KlibIrSymbol),
}

#[derive(Clone, Debug, PartialEq)]
pub struct KlibIrLocalDelegatedProperty {
    pub symbol: KlibIrSymbol,
    pub name: String,
    pub ty: KlibIrTypeId,
    pub delegate: Option<KlibIrVariableId>,
    pub getter: Option<KlibIrFunctionId>,
    pub setter: Option<KlibIrFunctionId>,
    pub flags: u64,
}

/// `IrExpression`'s operation.
#[derive(Clone, Debug, PartialEq)]
pub enum KlibIrExprKind {
    Const(KlibIrConstant),
    GetValue {
        symbol: KlibIrSymbol,
        origin: Option<String>,
    },
    SetValue {
        symbol: KlibIrSymbol,
        value: KlibIrExprId,
        origin: Option<String>,
    },
    Call {
        access: KlibIrMemberAccess,
        super_qualifier: Option<KlibIrSymbol>,
    },
    ConstructorCall {
        access: KlibIrMemberAccess,
        constructor_type_arguments: u32,
    },
    DelegatingConstructorCall(KlibIrMemberAccess),
    EnumConstructorCall(KlibIrMemberAccess),
    FunctionReference {
        access: KlibIrMemberAccess,
        reflection_target: Option<KlibIrSymbol>,
    },
    PropertyReference {
        access: KlibIrMemberAccess,
        field: Option<KlibIrSymbol>,
        getter: Option<KlibIrSymbol>,
        setter: Option<KlibIrSymbol>,
    },
    RichFunctionReference {
        bound_values: Vec<KlibIrExprId>,
        reflection_target: Option<KlibIrSymbol>,
        overridden: KlibIrSymbol,
        invoke: KlibIrFunctionId,
        flags: u64,
        origin: Option<String>,
    },
    RichPropertyReference {
        bound_values: Vec<KlibIrExprId>,
        reflection_target: Option<KlibIrSymbol>,
        getter: KlibIrFunctionId,
        setter: Option<KlibIrFunctionId>,
        origin: Option<String>,
    },
    LocalDelegatedPropertyReference {
        symbol: KlibIrSymbol,
        delegate: Option<KlibIrSymbol>,
        getter: Option<KlibIrSymbol>,
        setter: Option<KlibIrSymbol>,
        origin: Option<String>,
    },
    Block {
        statements: Vec<KlibIrStatement>,
        origin: Option<String>,
    },
    Composite {
        statements: Vec<KlibIrStatement>,
        origin: Option<String>,
    },
    ReturnableBlock {
        symbol: KlibIrSymbol,
        statements: Vec<KlibIrStatement>,
        origin: Option<String>,
    },
    InlinedFunctionBlock {
        inlined_function: Option<KlibIrSymbol>,
        statements: Vec<KlibIrStatement>,
        origin: Option<String>,
    },
    Return {
        target: KlibIrSymbol,
        value: KlibIrExprId,
    },
    When {
        branches: Vec<KlibIrBranch>,
        origin: Option<String>,
    },
    TypeOperator {
        operator: KlibIrTypeOperator,
        operand: KlibIrTypeId,
        argument: KlibIrExprId,
    },
    GetField {
        symbol: KlibIrSymbol,
        super_qualifier: Option<KlibIrSymbol>,
        receiver: Option<KlibIrExprId>,
        origin: Option<String>,
    },
    SetField {
        symbol: KlibIrSymbol,
        super_qualifier: Option<KlibIrSymbol>,
        receiver: Option<KlibIrExprId>,
        value: KlibIrExprId,
        origin: Option<String>,
    },
    GetObject(KlibIrSymbol),
    GetClass(KlibIrExprId),
    ClassReference {
        class: KlibIrSymbol,
        class_type: KlibIrTypeId,
    },
    GetEnumValue(KlibIrSymbol),
    Break {
        loop_id: u32,
        label: Option<String>,
    },
    Continue {
        loop_id: u32,
        label: Option<String>,
    },
    While(KlibIrLoop),
    DoWhile(KlibIrLoop),
    InstanceInitializerCall(KlibIrSymbol),
    StringConcat(Vec<KlibIrExprId>),
    Throw(KlibIrExprId),
    Try {
        result: KlibIrExprId,
        catches: Vec<KlibIrCatch>,
        finally: Option<KlibIrExprId>,
    },
    Vararg {
        element_type: KlibIrTypeId,
        elements: Vec<KlibIrVarargElement>,
    },
    FunctionExpression {
        function: KlibIrFunctionId,
        origin: Option<String>,
    },
    DynamicMember {
        member: String,
        receiver: KlibIrExprId,
    },
    DynamicOperator {
        operator: u32,
        receiver: KlibIrExprId,
        arguments: Vec<KlibIrExprId>,
    },
    Error {
        description: String,
    },
    ErrorCall {
        description: String,
        receiver: Option<KlibIrExprId>,
        arguments: Vec<KlibIrExprId>,
    },
    /// An argument slot the call leaves to its callee's default.
    Missing,
}

/// A value parameter or receiver of a function.
#[derive(Clone, Debug, PartialEq)]
pub struct KlibIrParameter {
    pub symbol: KlibIrSymbol,
    pub name: String,
    pub ty: KlibIrTypeId,
    pub vararg_element_type: Option<KlibIrTypeId>,
    pub default_value: Option<KlibIrExprId>,
    pub origin: String,
    pub flags: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct KlibIrTypeParameter {
    pub symbol: KlibIrSymbol,
    pub name: String,
    pub supertypes: Vec<KlibIrTypeId>,
    pub flags: u64,
}

/// What a function's body is.
#[derive(Clone, Debug, PartialEq)]
pub enum KlibIrBody {
    Block(Vec<KlibIrStatement>),
    /// `IrSyntheticBody`: an enum's `values`, `valueOf` or `entries`, generated by the backend.
    Synthetic(KlibIrSyntheticBody),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KlibIrSyntheticBody {
    EnumValues,
    EnumValueOf,
    EnumEntries,
}

/// A function, constructor or accessor, with its body when the KLIB carries one.
#[derive(Clone, Debug, PartialEq)]
pub struct KlibIrFunction {
    pub symbol: KlibIrSymbol,
    pub name: String,
    pub constructor: bool,
    pub origin: String,
    pub flags: u64,
    pub type_parameters: Vec<KlibIrTypeParameter>,
    pub dispatch_receiver: Option<KlibIrParameter>,
    pub context_parameters: Vec<KlibIrParameter>,
    pub extension_receiver: Option<KlibIrParameter>,
    pub regular_parameters: Vec<KlibIrParameter>,
    pub return_type: KlibIrTypeId,
    pub overridden: Vec<KlibIrSymbol>,
    pub body: Option<KlibIrBody>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct KlibIrVariable {
    pub symbol: KlibIrSymbol,
    pub name: String,
    pub ty: KlibIrTypeId,
    pub initializer: Option<KlibIrExprId>,
    pub origin: String,
    pub flags: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct KlibIrField {
    pub symbol: KlibIrSymbol,
    pub name: String,
    pub ty: KlibIrTypeId,
    pub initializer: Option<KlibIrExprId>,
    pub flags: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct KlibIrProperty {
    pub symbol: KlibIrSymbol,
    pub name: String,
    pub flags: u64,
    pub backing_field: Option<KlibIrField>,
    pub getter: Option<KlibIrFunctionId>,
    pub setter: Option<KlibIrFunctionId>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct KlibIrEnumEntry {
    pub symbol: KlibIrSymbol,
    pub name: String,
    pub initializer: Option<KlibIrExprId>,
    pub class: Option<KlibIrClassId>,
}

/// A declaration inside a class body.
#[derive(Clone, Debug, PartialEq)]
pub enum KlibIrMember {
    Function(KlibIrFunctionId),
    Property(KlibIrProperty),
    Field(KlibIrField),
    Class(KlibIrClassId),
    EnumEntry(KlibIrEnumEntry),
    AnonymousInitializer {
        symbol: KlibIrSymbol,
        statements: Vec<KlibIrStatement>,
    },
    TypeAlias(KlibIrSymbol),
}

#[derive(Clone, Debug, PartialEq)]
pub struct KlibIrClass {
    pub symbol: KlibIrSymbol,
    pub name: String,
    pub origin: String,
    pub flags: u64,
    pub this_receiver: Option<KlibIrParameter>,
    pub type_parameters: Vec<KlibIrTypeParameter>,
    pub supertypes: Vec<KlibIrTypeId>,
    pub members: Vec<KlibIrMember>,
    pub sealed_subclasses: Vec<KlibIrSymbol>,
}
