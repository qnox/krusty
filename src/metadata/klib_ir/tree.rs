//! The decoded form of one serialized KLIB IR declaration body.
//!
//! This is Kotlin's backend IR as a KLIB stores it, made addressable: every expression, type and
//! local declaration lives in one arena owned by the top-level declaration that holds it, and
//! refers to the others by index. Nothing here is interpreted. Symbols stay exact serialized
//! identities ([`KlibIrSymbol`]), origins stay the serializer's strings, and flags stay raw, so
//! a consumer joining this to a semantic model decides what each fact means and declines what it
//! does not model.
//!
//! Every serialized declaration and expression fact is kept except source coordinates (the
//! `global_coordinates`/`local_coordinates` offsets on declarations, statements and
//! expressions). Those feed debug information only, and the native target emits none yet; a
//! consumer that needs them extends the decoder rather than recovering positions elsewhere.
//! The facts of the `IrFile` that holds a declaration (its package name, file annotations and
//! file entry) belong to the file rather than to any tree and are not part of this model.

use super::symbols::KlibIrSymbol;
use super::KlibIrConstant;

mod emitted_callables;
mod lexical_values;

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
arena_id!(KlibIrTypeAbbreviationId);

/// Every node of one top-level declaration's bodies.
#[derive(Debug, Default)]
pub struct KlibIrArena {
    pub(super) exprs: Vec<KlibIrExpr>,
    pub(super) types: Vec<KlibIrType>,
    pub(super) functions: Vec<KlibIrFunction>,
    pub(super) classes: Vec<KlibIrClass>,
    pub(super) variables: Vec<KlibIrVariable>,
    pub(super) abbreviations: Vec<KlibIrTypeAbbreviation>,
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

    pub fn abbreviation(&self, id: KlibIrTypeAbbreviationId) -> &KlibIrTypeAbbreviation {
        &self.abbreviations[id.index()]
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

    /// Serialized call identities reachable from exactly `function`'s body. Nested declarations
    /// share their top-level declaration's arena but are not part of the enclosing function's
    /// expression graph, so their bodies are deliberately not traversed.
    pub(crate) fn call_symbols_in_body<'a>(
        &'a self,
        function: &'a KlibIrFunction,
    ) -> impl Iterator<Item = &'a KlibIrSymbol> {
        let mut pending = Vec::new();
        if let Some(KlibIrBody::Block(statements)) = &function.body {
            push_statement_expressions(self, statements, &mut pending);
        }
        let mut calls = Vec::new();
        while let Some(id) = pending.pop() {
            let expression = self.expr(id);
            push_child_expressions(self, &expression.kind, &mut pending);
            if let KlibIrExprKind::Call { access, .. } = &expression.kind {
                calls.push(&access.symbol);
            }
        }
        calls.into_iter()
    }

    /// Calls and constructions reached by executing exactly this body. Parameter defaults and
    /// nested declaration bodies are separate execution roots, not edges of this graph.
    pub(crate) fn callable_symbols_in_body<'a>(
        &'a self,
        function: &'a KlibIrFunction,
    ) -> impl Iterator<Item = &'a KlibIrSymbol> {
        let mut pending = Vec::new();
        if let Some(KlibIrBody::Block(statements)) = &function.body {
            push_statement_expressions(self, statements, &mut pending);
        }
        self.callable_symbols_from(pending)
    }

    /// Calls and constructions in a prepared body's executable graph, including defaults.
    /// This broader inventory is only for reference binding, not intrinsic-root authority.
    pub(crate) fn callable_symbols<'a>(
        &'a self,
        function: &'a KlibIrFunction,
    ) -> impl Iterator<Item = &'a KlibIrSymbol> {
        let mut pending = Vec::new();
        if let Some(KlibIrBody::Block(statements)) = &function.body {
            push_statement_expressions(self, statements, &mut pending);
        }
        pending.extend(
            function
                .context_parameters
                .iter()
                .chain(&function.extension_receiver)
                .chain(&function.regular_parameters)
                .filter_map(|parameter| parameter.default_value),
        );
        self.callable_symbols_from(pending)
    }

    fn callable_symbols_from(
        &self,
        mut pending: Vec<KlibIrExprId>,
    ) -> impl Iterator<Item = &KlibIrSymbol> {
        let mut calls = Vec::new();
        while let Some(id) = pending.pop() {
            let expression = self.expr(id);
            push_child_expressions(self, &expression.kind, &mut pending);
            match &expression.kind {
                KlibIrExprKind::Call { access, .. }
                | KlibIrExprKind::ConstructorCall { access, .. }
                | KlibIrExprKind::DelegatingConstructorCall(access)
                | KlibIrExprKind::EnumConstructorCall(access) => calls.push(&access.symbol),
                _ => {}
            }
        }
        calls.into_iter()
    }

    /// The number of `return`s naming `target` among `statements` and the expressions nested in
    /// them.
    pub(crate) fn returns_to(
        &self,
        statements: &[KlibIrStatement],
        target: &KlibIrSymbol,
    ) -> usize {
        let mut pending = Vec::new();
        push_statement_expressions(self, statements, &mut pending);
        let mut count = 0;
        while let Some(id) = pending.pop() {
            let expression = self.expr(id);
            if matches!(&expression.kind, KlibIrExprKind::Return { target: named, .. } if named == target)
            {
                count += 1;
            }
            push_child_expressions(self, &expression.kind, &mut pending);
        }
        count
    }

    /// Whether exactly `function`'s body reads `symbol` as a value. Nested declarations share the
    /// arena but are not part of the function's expression graph, matching `call_symbols_in_body`.
    pub(crate) fn body_reads_value(
        &self,
        function: &KlibIrFunction,
        symbol: &KlibIrSymbol,
    ) -> bool {
        let mut pending = Vec::new();
        if let Some(KlibIrBody::Block(statements)) = &function.body {
            push_statement_expressions(self, statements, &mut pending);
        }
        while let Some(id) = pending.pop() {
            let expression = self.expr(id);
            if matches!(
                &expression.kind,
                KlibIrExprKind::GetValue { symbol: read, .. } if read == symbol
            ) {
                return true;
            }
            push_child_expressions(self, &expression.kind, &mut pending);
        }
        false
    }
}

fn push_access_arguments(access: &KlibIrMemberAccess, pending: &mut Vec<KlibIrExprId>) {
    match &access.arguments {
        KlibIrArguments::Flat(arguments) => pending.extend(arguments.iter().flatten().copied()),
        KlibIrArguments::Split {
            dispatch_receiver,
            extension_receiver,
            values,
        } => {
            pending.extend(dispatch_receiver.iter().copied());
            pending.extend(extension_receiver.iter().copied());
            pending.extend(values.iter().flatten().copied());
        }
    }
}

fn push_statement_expressions(
    arena: &KlibIrArena,
    statements: &[KlibIrStatement],
    pending: &mut Vec<KlibIrExprId>,
) {
    for statement in statements {
        match statement {
            KlibIrStatement::Expression(expression) => pending.push(*expression),
            KlibIrStatement::Variable(variable) => {
                pending.extend(arena.variable(*variable).initializer.iter().copied());
            }
            KlibIrStatement::LocalDelegatedProperty(property) => {
                if let Some(delegate) = property.delegate {
                    pending.extend(arena.variable(delegate).initializer.iter().copied());
                }
            }
            KlibIrStatement::Function(_)
            | KlibIrStatement::Class(_)
            | KlibIrStatement::TypeAlias(_) => {}
        }
    }
}

fn push_child_expressions(
    arena: &KlibIrArena,
    kind: &KlibIrExprKind,
    pending: &mut Vec<KlibIrExprId>,
) {
    use KlibIrExprKind as K;
    match kind {
        K::SetValue { value, .. } | K::Return { value, .. } => pending.push(*value),
        K::Call { access, .. }
        | K::ConstructorCall { access, .. }
        | K::DelegatingConstructorCall(access)
        | K::EnumConstructorCall(access)
        | K::FunctionReference { access, .. }
        | K::PropertyReference { access, .. } => push_access_arguments(access, pending),
        K::RichFunctionReference { bound_values, .. }
        | K::RichPropertyReference { bound_values, .. }
        | K::StringConcat(bound_values) => pending.extend(bound_values.iter().copied()),
        K::Block { statements, .. }
        | K::Composite { statements, .. }
        | K::ReturnableBlock { statements, .. }
        | K::InlinedFunctionBlock { statements, .. } => {
            push_statement_expressions(arena, statements, pending);
        }
        K::When { branches, .. } => {
            for branch in branches {
                pending.push(branch.condition);
                pending.push(branch.result);
            }
        }
        K::TypeOperator { argument, .. } | K::GetClass(argument) | K::Throw(argument) => {
            pending.push(*argument)
        }
        K::GetField { receiver, .. } => pending.extend(receiver.iter().copied()),
        K::SetField {
            receiver, value, ..
        } => {
            pending.extend(receiver.iter().copied());
            pending.push(*value);
        }
        K::While(loop_) | K::DoWhile(loop_) => {
            pending.push(loop_.condition);
            pending.extend(loop_.body.iter().copied());
        }
        K::Try {
            result,
            catches,
            finally,
        } => {
            pending.push(*result);
            pending.extend(catches.iter().map(|catch| catch.result));
            pending.extend(finally.iter().copied());
        }
        K::Vararg { elements, .. } => {
            pending.extend(elements.iter().map(|element| match element {
                KlibIrVarargElement::Expression(expression)
                | KlibIrVarargElement::Spread(expression) => *expression,
            }));
        }
        K::DynamicMember { receiver, .. } => pending.push(*receiver),
        K::DynamicOperator {
            receiver,
            arguments,
            ..
        } => {
            pending.push(*receiver);
            pending.extend(arguments.iter().copied());
        }
        K::ErrorCall {
            receiver,
            arguments,
            ..
        } => {
            pending.extend(receiver.iter().copied());
            pending.extend(arguments.iter().copied());
        }
        K::Const(_)
        | K::GetValue { .. }
        | K::LocalDelegatedPropertyReference { .. }
        | K::GetObject(_)
        | K::ClassReference { .. }
        | K::GetEnumValue(_)
        | K::Break { .. }
        | K::Continue { .. }
        | K::InstanceInitializerCall(_)
        | K::FunctionExpression { .. }
        | K::Error { .. }
        | K::Missing => {}
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
        annotations: Vec<KlibIrAnnotation>,
        /// The type alias this type was written as, recorded by KLIBs older than Kotlin 2.4.
        abbreviation: Option<KlibIrTypeAbbreviationId>,
    },
    DefinitelyNotNull(KlibIrTypeId),
    Dynamic {
        annotations: Vec<KlibIrAnnotation>,
    },
    Error {
        annotations: Vec<KlibIrAnnotation>,
    },
}

/// `IrTypeAbbreviation`: a type alias application that expanded to the enclosing type.
#[derive(Clone, Debug, PartialEq)]
pub struct KlibIrTypeAbbreviation {
    pub type_alias: KlibIrSymbol,
    pub marked_nullable: bool,
    pub arguments: Vec<KlibIrTypeArgument>,
    pub annotations: Vec<KlibIrAnnotation>,
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
    TypeAlias(KlibIrTypeAlias),
}

#[derive(Clone, Debug, PartialEq)]
pub struct KlibIrLocalDelegatedProperty {
    pub base: KlibIrDeclarationBase,
    pub name: String,
    pub ty: KlibIrTypeId,
    pub delegate: Option<KlibIrVariableId>,
    pub getter: Option<KlibIrFunctionId>,
    pub setter: Option<KlibIrFunctionId>,
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
        /// The file that declared the inlined function, and its offsets there.
        file: KlibIrFileEntry,
        start_offset: i32,
        end_offset: i32,
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

/// `IrDeclarationBase`: what every declaration carries besides its own fields.
#[derive(Clone, Debug, PartialEq)]
pub struct KlibIrDeclarationBase {
    pub symbol: KlibIrSymbol,
    /// The serializer's `IrDeclarationOrigin` name (`DEFINED`, `FAKE_OVERRIDE`, ...).
    pub origin: String,
    /// The serialized flag word, bit layout per declaration kind, kept undecoded.
    pub flags: u64,
    pub annotations: Vec<KlibIrAnnotation>,
}

/// An annotation application: a constructor call of the annotation class.
#[derive(Clone, Debug, PartialEq)]
pub struct KlibIrAnnotation {
    pub access: KlibIrMemberAccess,
    pub constructor_type_arguments: u32,
}

/// Where an inlined function's offsets point: an entry of the KLIB's `fileEntries.knf` table,
/// or, before Kotlin 2.3, a file name stored in place.
#[derive(Clone, Debug, PartialEq)]
pub enum KlibIrFileEntry {
    Id(u32),
    Named(String),
}

/// A value parameter or receiver of a function.
#[derive(Clone, Debug, PartialEq)]
pub struct KlibIrParameter {
    pub base: KlibIrDeclarationBase,
    pub name: String,
    pub ty: KlibIrTypeId,
    pub vararg_element_type: Option<KlibIrTypeId>,
    pub default_value: Option<KlibIrExprId>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct KlibIrTypeParameter {
    pub base: KlibIrDeclarationBase,
    pub name: String,
    pub supertypes: Vec<KlibIrTypeId>,
}

impl KlibIrTypeParameter {
    /// IrFlags stores HAS_ANNOTATIONS at bit 0, VARIANCE at bits 1..3, then IS_REIFIED.
    /// This is the serialized IR layout, not metadata.proto's separate reified field.
    pub(crate) fn is_reified(&self) -> bool {
        self.base.flags & (1 << 3) != 0
    }

    pub(crate) fn variance(&self) -> Option<KlibIrVariance> {
        match (self.base.flags >> 1) & 3 {
            0 => Some(KlibIrVariance::In),
            1 => Some(KlibIrVariance::Out),
            2 => Some(KlibIrVariance::Invariant),
            _ => None,
        }
    }
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
    pub base: KlibIrDeclarationBase,
    pub name: String,
    pub constructor: bool,
    pub type_parameters: Vec<KlibIrTypeParameter>,
    pub dispatch_receiver: Option<KlibIrParameter>,
    pub context_parameters: Vec<KlibIrParameter>,
    pub extension_receiver: Option<KlibIrParameter>,
    pub regular_parameters: Vec<KlibIrParameter>,
    pub return_type: KlibIrTypeId,
    pub overridden: Vec<KlibIrSymbol>,
    /// The class whose companion this extension's receiver denotes (`companionExtensionClass`).
    pub companion_extension_class: Option<KlibIrSymbol>,
    /// For an inline function prepared for cross-module inlining, the file entry its body's
    /// offsets refer to.
    pub prepared_inline_file: Option<u32>,
    pub body: Option<KlibIrBody>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct KlibIrVariable {
    pub base: KlibIrDeclarationBase,
    pub name: String,
    pub ty: KlibIrTypeId,
    pub initializer: Option<KlibIrExprId>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct KlibIrField {
    pub base: KlibIrDeclarationBase,
    pub name: String,
    pub ty: KlibIrTypeId,
    pub initializer: Option<KlibIrExprId>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct KlibIrProperty {
    pub base: KlibIrDeclarationBase,
    pub name: String,
    pub backing_field: Option<KlibIrField>,
    pub getter: Option<KlibIrFunctionId>,
    pub setter: Option<KlibIrFunctionId>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct KlibIrEnumEntry {
    pub base: KlibIrDeclarationBase,
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
        base: KlibIrDeclarationBase,
        statements: Vec<KlibIrStatement>,
    },
    TypeAlias(KlibIrTypeAlias),
}

#[derive(Clone, Debug, PartialEq)]
pub struct KlibIrTypeAlias {
    pub base: KlibIrDeclarationBase,
    pub name: String,
    pub expanded: KlibIrTypeId,
    pub type_parameters: Vec<KlibIrTypeParameter>,
}

/// How a value class stores its value.
#[derive(Clone, Debug, PartialEq)]
pub enum KlibIrValueClassRepresentation {
    /// One underlying property (`IrInlineClassRepresentation`).
    Inline {
        underlying_property: String,
        underlying_type: KlibIrTypeId,
    },
    /// Several underlying properties, in declaration order
    /// (`IrMultiFieldValueClassRepresentation`, serialized by Kotlin 2.4.0 and 2.4.10 only).
    MultiField {
        underlying_properties: Vec<(String, KlibIrTypeId)>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct KlibIrClass {
    pub base: KlibIrDeclarationBase,
    pub name: String,
    pub this_receiver: Option<KlibIrParameter>,
    pub type_parameters: Vec<KlibIrTypeParameter>,
    pub supertypes: Vec<KlibIrTypeId>,
    pub members: Vec<KlibIrMember>,
    pub value_class_representation: Option<KlibIrValueClassRepresentation>,
    pub sealed_subclasses: Vec<KlibIrSymbol>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::klib_ir::KlibIrSymbolKind;

    fn local(kind: KlibIrSymbolKind, slot: u32) -> KlibIrSymbol {
        KlibIrSymbol {
            kind,
            signature: super::super::KlibIrSignature::FileLocal { file: 0, slot },
        }
    }

    fn function(
        symbol: KlibIrSymbol,
        return_type: KlibIrTypeId,
        expression: KlibIrExprId,
    ) -> KlibIrFunction {
        KlibIrFunction {
            base: KlibIrDeclarationBase {
                symbol,
                origin: "DEFINED".to_owned(),
                flags: 0,
                annotations: Vec::new(),
            },
            name: "fixture".to_owned(),
            constructor: false,
            type_parameters: Vec::new(),
            dispatch_receiver: None,
            context_parameters: Vec::new(),
            extension_receiver: None,
            regular_parameters: Vec::new(),
            return_type,
            overridden: Vec::new(),
            companion_extension_class: None,
            prepared_inline_file: None,
            body: Some(KlibIrBody::Block(vec![KlibIrStatement::Expression(
                expression,
            )])),
        }
    }

    #[test]
    fn body_call_inventory_excludes_sibling_function_bodies_in_the_same_arena() {
        let mut arena = KlibIrArena::default();
        let ty = arena.push_type(KlibIrType::Error {
            annotations: Vec::new(),
        });
        let selected_callee = local(KlibIrSymbolKind::Function, 1);
        let sibling_callee = local(KlibIrSymbolKind::Function, 2);
        let call = |symbol| KlibIrExprKind::Call {
            access: KlibIrMemberAccess {
                symbol,
                arguments: KlibIrArguments::Flat(Vec::new()),
                type_arguments: Vec::new(),
                origin: None,
            },
            super_qualifier: None,
        };
        let selected_call = arena.push_expr(Some(ty), call(selected_callee.clone()));
        let sibling_call = arena.push_expr(Some(ty), call(sibling_callee));
        let selected = arena.push_function(function(
            local(KlibIrSymbolKind::Function, 10),
            ty,
            selected_call,
        ));
        arena.push_function(function(
            local(KlibIrSymbolKind::Function, 11),
            ty,
            sibling_call,
        ));

        assert_eq!(
            arena
                .call_symbols_in_body(arena.function(selected))
                .cloned()
                .collect::<Vec<_>>(),
            [selected_callee]
        );
    }

    #[test]
    fn klib_constructor_inventory_excludes_unused_defaults_and_nested_declarations() {
        let mut arena = KlibIrArena::default();
        let ty = arena.push_type(KlibIrType::Error {
            annotations: Vec::new(),
        });
        let constructor = |symbol| KlibIrExprKind::ConstructorCall {
            access: KlibIrMemberAccess {
                symbol,
                arguments: KlibIrArguments::Flat(Vec::new()),
                type_arguments: Vec::new(),
                origin: None,
            },
            constructor_type_arguments: 0,
        };
        let executed = local(KlibIrSymbolKind::Constructor, 1);
        let unused_default = local(KlibIrSymbolKind::Constructor, 2);
        let nested = local(KlibIrSymbolKind::Constructor, 3);
        let executed_expr = arena.push_expr(Some(ty), constructor(executed.clone()));
        let default_expr = arena.push_expr(Some(ty), constructor(unused_default.clone()));
        let nested_expr = arena.push_expr(Some(ty), constructor(nested));
        let nested_function = arena.push_function(function(
            local(KlibIrSymbolKind::Function, 11),
            ty,
            nested_expr,
        ));
        let block = arena.push_expr(
            Some(ty),
            KlibIrExprKind::Block {
                statements: vec![
                    KlibIrStatement::Function(nested_function),
                    KlibIrStatement::Expression(executed_expr),
                ],
                origin: None,
            },
        );
        let mut selected = function(local(KlibIrSymbolKind::Function, 10), ty, block);
        selected.regular_parameters.push(KlibIrParameter {
            base: KlibIrDeclarationBase {
                symbol: local(KlibIrSymbolKind::ValueParameter, 12),
                origin: "DEFINED".into(),
                flags: 0,
                annotations: Vec::new(),
            },
            name: "unused".into(),
            ty,
            vararg_element_type: None,
            default_value: Some(default_expr),
        });
        let selected = arena.push_function(selected);
        let selected = arena.function(selected);
        assert_eq!(
            arena
                .callable_symbols_in_body(selected)
                .cloned()
                .collect::<Vec<_>>()
                .as_slice(),
            std::slice::from_ref(&executed)
        );
        assert_eq!(
            arena.call_symbols_in_body(selected).count(),
            0,
            "construction is not intrinsic call-root evidence"
        );
        assert_eq!(
            arena
                .callable_symbols(selected)
                .cloned()
                .collect::<Vec<_>>(),
            [unused_default, executed]
        );
    }

    #[test]
    fn emitted_callable_inventory_follows_only_selected_closure_roots_once() {
        let mut arena = KlibIrArena::default();
        let ty = arena.push_type(KlibIrType::Error {
            annotations: vec![],
        });
        let constructor = |symbol| KlibIrExprKind::ConstructorCall {
            access: KlibIrMemberAccess {
                symbol,
                arguments: KlibIrArguments::Flat(vec![]),
                type_arguments: vec![],
                origin: None,
            },
            constructor_type_arguments: 0,
        };
        let executed = local(KlibIrSymbolKind::Constructor, 1);
        let helper = local(KlibIrSymbolKind::Function, 2);
        let excluded = local(KlibIrSymbolKind::Constructor, 3);
        let call = arena.push_expr(Some(ty), constructor(executed.clone()));
        let excluded_call = arena.push_expr(Some(ty), constructor(excluded));
        let helper_call = arena.push_expr(
            Some(ty),
            KlibIrExprKind::Call {
                access: KlibIrMemberAccess {
                    symbol: helper.clone(),
                    arguments: KlibIrArguments::Flat(vec![]),
                    type_arguments: vec![],
                    origin: None,
                },
                super_qualifier: None,
            },
        );
        let block = arena.push_expr(
            Some(ty),
            KlibIrExprKind::Block {
                statements: vec![
                    KlibIrStatement::Expression(call),
                    KlibIrStatement::Expression(helper_call),
                ],
                origin: None,
            },
        );
        let leaf = arena.push_function(function(local(KlibIrSymbolKind::Function, 11), ty, block));
        let leaf_expression = arena.push_expr(
            Some(ty),
            KlibIrExprKind::FunctionExpression {
                function: leaf,
                origin: Some("LAMBDA".into()),
            },
        );
        let outer = arena.push_function(function(
            local(KlibIrSymbolKind::Function, 12),
            ty,
            leaf_expression,
        ));
        let outer_expression = arena.push_expr(
            Some(ty),
            KlibIrExprKind::FunctionExpression {
                function: outer,
                origin: Some("LAMBDA".into()),
            },
        );
        let unrelated = arena.push_function(function(
            local(KlibIrSymbolKind::Function, 13),
            ty,
            excluded_call,
        ));
        let root_block = arena.push_expr(
            Some(ty),
            KlibIrExprKind::Block {
                statements: vec![
                    KlibIrStatement::Function(unrelated),
                    KlibIrStatement::Expression(outer_expression),
                    KlibIrStatement::Expression(outer_expression),
                ],
                origin: None,
            },
        );
        let mut root = function(local(KlibIrSymbolKind::Function, 10), ty, root_block);
        root.regular_parameters.push(KlibIrParameter {
            base: KlibIrDeclarationBase {
                symbol: local(KlibIrSymbolKind::ValueParameter, 20),
                origin: "DEFINED".into(),
                flags: 0,
                annotations: vec![],
            },
            name: "unused".into(),
            ty,
            vararg_element_type: None,
            default_value: Some(excluded_call),
        });
        let root = arena.push_function(root);
        let selected = arena.function(root);
        assert_eq!(
            arena
                .emitted_callable_symbols(selected, |_, expression| match expression.kind {
                    KlibIrExprKind::FunctionExpression { function, .. } => Some(function),
                    _ => None,
                })
                .into_iter()
                .cloned()
                .collect::<Vec<_>>(),
            [helper, executed]
        );
        assert!(arena
            .emitted_callable_symbols(selected, |_, _| None)
            .is_empty());
        assert_eq!(arena.callable_symbols_in_body(selected).count(), 0);
        assert_eq!(arena.call_symbols_in_body(selected).count(), 0);
    }

    #[test]
    fn body_value_reads_are_exact_to_the_selected_function_graph() {
        let mut arena = KlibIrArena::default();
        let ty = arena.push_type(KlibIrType::Error {
            annotations: Vec::new(),
        });
        let selected_receiver = local(KlibIrSymbolKind::ReceiverParameter, 1);
        let sibling_receiver = local(KlibIrSymbolKind::ReceiverParameter, 2);
        let selected_read = arena.push_expr(
            Some(ty),
            KlibIrExprKind::GetValue {
                symbol: selected_receiver.clone(),
                origin: None,
            },
        );
        let sibling_read = arena.push_expr(
            Some(ty),
            KlibIrExprKind::GetValue {
                symbol: sibling_receiver.clone(),
                origin: None,
            },
        );
        let selected = arena.push_function(function(
            local(KlibIrSymbolKind::Function, 10),
            ty,
            selected_read,
        ));
        arena.push_function(function(
            local(KlibIrSymbolKind::Function, 11),
            ty,
            sibling_read,
        ));

        assert!(arena.body_reads_value(arena.function(selected), &selected_receiver));
        assert!(!arena.body_reads_value(arena.function(selected), &sibling_receiver));
    }
}
