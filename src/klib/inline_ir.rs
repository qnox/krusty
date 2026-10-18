//! Standards-compatible KLIB IR for declaration-owned inline templates.
//!
//! Kotlin 2.4 owns a separate `default/ir_inlinable_functions/` component for prepared inline
//! copies. It is one synthetic root file whose declarations keep their original public
//! `IdSignature`; member functions therefore need no fabricated class wrapper. This module writes
//! that component for the exact common-IR shape Krusty can currently serialize completely:
//! `return value is/!is T` in an inline function with a reified own type parameter.

use crate::fx_hash::HashMap;
use crate::ir::{FunId, IrExpr, IrFile, IrParameterRole, IrTypeOp, IrTypeParameter};
use crate::metadata::id_signature::KlibPublicIdSignature;
use crate::metadata::protobuf::Pb;
use crate::types::{Ty, TypeName, TypeVariance};

const PREFIX: &str = "default/ir_inlinable_functions/";
const FUNCTION_SYMBOL: u64 = 0;
const CONSTRUCTOR_SYMBOL: u64 = 1;
const VALUE_PARAMETER_SYMBOL: u64 = 4;
const CLASS_SYMBOL: u64 = 6;
const TYPE_PARAMETER_SYMBOL: u64 = 7;
const RECEIVER_PARAMETER_SYMBOL: u64 = 11;

/// Exact metadata identities of declarations written into the KLIB fragment, including an
/// `@InlineOnly` template that deliberately has no standalone target realization.
#[derive(Default)]
pub(super) struct DeclarationSignatures {
    functions: HashMap<FunId, (KlibPublicIdSignature, Option<Ty>)>,
}

impl DeclarationSignatures {
    pub(super) fn record(
        &mut self,
        function: FunId,
        signature: KlibPublicIdSignature,
        member_receiver: Option<Ty>,
    ) {
        let previous = self
            .functions
            .insert(function, (signature, member_receiver));
        assert!(
            previous.is_none(),
            "one function has one KLIB declaration identity"
        );
    }

    fn get(&self, function: FunId) -> Option<&(KlibPublicIdSignature, Option<Ty>)> {
        self.functions.get(&function)
    }
}

#[derive(Clone)]
pub(super) struct PreparedFunction {
    signature: KlibPublicIdSignature,
    name: String,
    file_name: String,
    flags: u64,
    inline_only: bool,
    dispatch_receiver: Option<Ty>,
    parameters: Vec<PreparedParameter>,
    context_count: usize,
    extension_parameter: Option<usize>,
    result: Ty,
    type_parameters: Vec<PreparedTypeParameter>,
    body: ReifiedTypeTest,
}

#[derive(Clone)]
struct PreparedParameter {
    name: String,
    ty: Ty,
    default_null: bool,
}

#[derive(Clone)]
struct PreparedTypeParameter {
    name: String,
    ty: Ty,
    bounds: Vec<Ty>,
    variance: TypeVariance,
    reified: bool,
}

#[derive(Clone, Copy)]
struct ReifiedTypeTest {
    value_slot: u32,
    type_parameter: usize,
    negated: bool,
}

/// Serialize every exact supported inline template from one checked file.
///
/// Unsupported inline bodies remain metadata-only. The returned functions are later combined into
/// one official prepared-inline component; no partial ordinary `default/ir` file is produced.
pub(super) fn collect(
    ir: &IrFile,
    file_name: &str,
    declarations: &DeclarationSignatures,
) -> Vec<PreparedFunction> {
    let mut functions = Vec::new();
    let mut ids = ir
        .public_inline_functions
        .iter()
        .copied()
        .collect::<Vec<_>>();
    ids.sort_unstable();
    for function in ids {
        let Some(prepared) = prepared_function(ir, file_name, declarations, function) else {
            continue;
        };
        functions.push(prepared);
    }
    functions
}

fn prepared_function(
    ir: &IrFile,
    file_name: &str,
    declarations: &DeclarationSignatures,
    function: FunId,
) -> Option<PreparedFunction> {
    let (signature, dispatch_receiver) = declarations.get(function)?;
    let declaration = ir.functions.get(function as usize)?;
    // Prepared copies need the declaration's complete IR flag word. Common IR does not yet retain
    // the suspend bit that Kotlin's serializer writes, so a suspend-inline body is outside this
    // deliberately narrow writer instead of being published with false declaration semantics.
    if ir.suspend_funs.contains(&function) {
        return None;
    }
    if ir.fn_varargs.contains_key(&function)
        || ir
            .declared_inline_modifiers(function)
            .iter()
            .any(|modifier| *modifier != crate::types::InlineParameterModifier::None)
    {
        // Vararg element types and noinline/crossinline flags are declaration facts the narrow
        // parameter writer does not yet encode.
        return None;
    }
    let overrides_declaration = ir.function_overrides.values().flatten().any(|edge| {
        edge.implementation_function == Some(function)
            || matches!(
                edge.implementation,
                crate::fir::ResolvedFunctionOverrideTarget::Module(callable)
                    if ir.checked_callable_functions.get(&callable) == Some(&function)
            )
    });
    if ir.open_methods.contains(&function) || overrides_declaration {
        // The prepared wrapper's overridden-symbol list must be complete for virtual members.
        return None;
    }
    let generic = ir.signatures.get(&function)?;
    let body = reified_type_test(ir, function, declaration.body?, generic)?;
    let identities = ir.function_parameter_identities(function)?;
    if identities.len() != generic.params.len() || generic.params.len() != declaration.params.len()
    {
        return None;
    }
    let parameters = identities
        .iter()
        .zip(&generic.params)
        .enumerate()
        .map(|(index, (identity, &ty))| {
            let name = match (&identity.source_name, identity.role) {
                (Some(name), _) => name.clone(),
                (None, IrParameterRole::ExtensionReceiver) => "<this>".to_owned(),
                _ => return None,
            };
            let default_null = match ir
                .declared_param_defaults(function)
                .and_then(|defaults| defaults.get(index))
                .copied()
                .flatten()
            {
                None => false,
                Some(default) if is_null_default(ir, default) => true,
                Some(_) => return None,
            };
            supported_type(ty, &generic.type_params).then_some(PreparedParameter {
                name,
                ty,
                default_null,
            })
        })
        .collect::<Option<Vec<_>>>()?;
    if ir
        .declared_param_defaults(function)
        .is_some_and(|defaults| defaults.len() != parameters.len())
    {
        return None;
    }
    let result = generic.ret?;
    if !supported_type(result, &generic.type_params)
        || generic
            .type_params
            .iter()
            .flat_map(|parameter| parameter.bounds.iter().map(|(bound, _)| *bound))
            .any(|bound| !supported_type(bound, &generic.type_params))
    {
        return None;
    }
    if dispatch_receiver.is_some_and(|ty| !supported_type(ty, &generic.type_params)) {
        return None;
    }
    let context_count = ir.fn_context_counts.get(&function).copied().unwrap_or(0);
    let extension_parameter = ir
        .extension_receiver_fns
        .contains(&function)
        .then_some(context_count);
    if context_count + usize::from(extension_parameter.is_some()) > parameters.len() {
        return None;
    }
    let inline_only = ir.inline_only_fns.contains(&function);
    let annotated = ir
        .function_annotations
        .get(&function)
        .is_some_and(|annotations| annotations.declares_metadata_annotations());
    let exact_inline_only_annotation =
        ir.function_annotations
            .get(&function)
            .is_some_and(|annotations| {
                let applications = annotations.applications().collect::<Vec<_>>();
                matches!(applications.as_slice(), [annotation]
                    if annotation.internal.matches("kotlin/internal/InlineOnly")
                        && annotation.values.is_empty())
            });
    if (inline_only && !exact_inline_only_annotation) || (!inline_only && annotated) {
        // Other declaration annotations need their complete checked constructor/value payload.
        return None;
    }
    let mut flags = crate::metadata::declaration_flags::function_flags(ir, function, declaration)
        | u64::from(annotated);
    if inline_only {
        // Kotlin's prepared copy makes an `@InlineOnly` declaration private even when linkdata
        // exposes the source declaration. With HAS_ANNOTATIONS + PRIVATE + INLINE this is 1027,
        // exactly the standard serializer's flag word.
        flags = (flags & !(0x7 << 1)) | (1 << 1);
    }
    Some(PreparedFunction {
        signature: signature.clone(),
        name: ir.fn_source_names.get(&function)?.clone(),
        file_name: file_name.to_owned(),
        flags,
        inline_only,
        dispatch_receiver: *dispatch_receiver,
        parameters,
        context_count,
        extension_parameter,
        result,
        type_parameters: generic
            .type_params
            .iter()
            .map(|parameter| PreparedTypeParameter {
                name: parameter.name.clone(),
                ty: parameter.ty(),
                bounds: parameter.bounds.iter().map(|(bound, _)| *bound).collect(),
                variance: parameter.variance,
                reified: parameter.reified,
            })
            .collect(),
        body,
    })
}

/// Common lowering preserves the checked conversion from literal `null` to its declared nullable
/// parameter type. Both forms denote the same closed default and no other conversion is admitted.
fn is_null_default(ir: &IrFile, expression: u32) -> bool {
    match ir.expr(expression) {
        IrExpr::Const(crate::ir::IrConst::Null) => true,
        IrExpr::TypeOp {
            op: IrTypeOp::ImplicitCoercion,
            arg,
            ..
        } => matches!(ir.expr(*arg), IrExpr::Const(crate::ir::IrConst::Null)),
        _ => false,
    }
}

fn reified_type_test(
    ir: &IrFile,
    function: FunId,
    body: u32,
    generic: &crate::ir::IrGenericSig,
) -> Option<ReifiedTypeTest> {
    let IrExpr::Block { stmts, value: None } = ir.expr(body) else {
        return None;
    };
    let [returned] = stmts.as_slice() else {
        return None;
    };
    let IrExpr::Return(Some(operation)) = ir.expr(*returned) else {
        return None;
    };
    let IrExpr::TypeOp {
        op,
        arg,
        type_operand,
    } = ir.expr(*operation)
    else {
        return None;
    };
    let negated = match op {
        IrTypeOp::InstanceOf => false,
        IrTypeOp::NotInstanceOf => true,
        _ => return None,
    };
    let IrExpr::GetValue(value_slot) = ir.expr(*arg) else {
        return None;
    };
    let physical_arity = generic.params.len()
        + usize::from(ir.functions[function as usize].dispatch_receiver.is_some());
    if usize::try_from(*value_slot).ok()? >= physical_arity {
        return None;
    }
    let type_parameter = generic
        .type_params
        .iter()
        .position(|parameter| parameter.reified && parameter.ty() == *type_operand)?;
    Some(ReifiedTypeTest {
        value_slot: *value_slot,
        type_parameter,
        negated,
    })
}

fn supported_type(ty: Ty, parameters: &[IrTypeParameter]) -> bool {
    match ty {
        Ty::Unit | Ty::Nothing | Ty::Boolean => true,
        Ty::Obj(_, arguments) => arguments.iter().all(|argument| match argument {
            Ty::InProjection(inner) | Ty::OutProjection(inner) | Ty::StarProjection(inner) => {
                supported_type(**inner, parameters)
            }
            other => supported_type(*other, parameters),
        }),
        Ty::Nullable(inner) => supported_type(*inner, parameters),
        Ty::InProjection(_) | Ty::OutProjection(_) | Ty::StarProjection(_) => false,
        Ty::TyParam(..) => parameters.iter().any(|parameter| parameter.ty() == ty),
        Ty::Null
        | Ty::Error
        | Ty::Pending
        | Ty::Fun(_)
        | Ty::DefinitelyNotNull(_)
        | Ty::PlatformNullable(_)
        | Ty::Intersection(_) => false,
    }
}

/// Lay the collected functions out as Kotlin 2.4's official prepared-inline IR component.
pub(super) fn artifacts(functions: &[PreparedFunction]) -> Vec<(String, Vec<u8>)> {
    if functions.is_empty() {
        return Vec::new();
    }
    let mut writer = FileWriter::default();
    let mut declarations = Vec::with_capacity(functions.len());
    for function in functions {
        declarations.push(writer.function(function));
    }
    let ids = declarations
        .iter()
        .map(|(signature, _)| u64::from(*signature))
        .collect::<Vec<_>>();
    let mut file = Pb::new();
    file.field_packed_varints(1, &ids);
    let entries = [
        ("files.knf", per_file(file.into_bytes())),
        (
            "fileEntries.knf",
            per_file(entry_table(&writer.file_entries)),
        ),
        ("bodies.knb", per_file(entry_table(&writer.bodies))),
        (
            "irDeclarations.knd",
            per_file(declaration_table(&declarations)),
        ),
        ("types.knt", per_file(entry_table(&writer.types))),
        ("signatures.knt", per_file(entry_table(&writer.signatures))),
        (
            "strings.knt",
            per_file(entry_table(
                &writer
                    .strings
                    .iter()
                    .map(|value| value.as_bytes().to_vec())
                    .collect::<Vec<_>>(),
            )),
        ),
        ("debugInfo.knd", per_file(entry_table(&[]))),
    ];
    entries
        .into_iter()
        .map(|(name, bytes)| (format!("{PREFIX}{name}"), bytes))
        .collect()
}

#[derive(Default)]
struct FileWriter {
    strings: Vec<String>,
    signatures: Vec<Vec<u8>>,
    types: Vec<Vec<u8>>,
    bodies: Vec<Vec<u8>>,
    file_entries: Vec<Vec<u8>>,
}

impl FileWriter {
    fn function(&mut self, function: &PreparedFunction) -> (u32, Vec<u8>) {
        let declaration_signature = self.public_signature(&function.signature);
        let declaration_symbol = symbol(declaration_signature, FUNCTION_SYMBOL);
        let type_parameters = function
            .type_parameters
            .iter()
            .enumerate()
            .map(|(index, parameter)| {
                let signature = self.local_signature();
                let symbol = symbol(signature, TYPE_PARAMETER_SYMBOL);
                (index, parameter, symbol)
            })
            .collect::<Vec<_>>();
        let parameter_symbols = (0..function.parameters.len()
            + usize::from(function.dispatch_receiver.is_some()))
            .map(|slot| {
                let signature = self.local_signature();
                let logical_slot =
                    slot.checked_sub(usize::from(function.dispatch_receiver.is_some()));
                let receiver = function.dispatch_receiver.is_some_and(|_| slot == 0)
                    || logical_slot.is_some_and(|logical_slot| {
                        function.extension_parameter == Some(logical_slot)
                    });
                symbol(
                    signature,
                    if receiver {
                        RECEIVER_PARAMETER_SYMBOL
                    } else {
                        VALUE_PARAMETER_SYMBOL
                    },
                )
            })
            .collect::<Vec<_>>();
        let mut base = Pb::new();
        base.field_varint(1, declaration_symbol);
        base.field_varint(2, u64::from(self.string("DEFINED")));
        base.field_varint(4, function.flags);
        if function.inline_only {
            let annotation = self.inline_only_annotation();
            base.repeated_message(5, &annotation);
        }

        let result = self.ty(function.result, &type_parameters);
        let mut function_base = Pb::new();
        function_base.field_message(1, &base);
        let name = self.string(&function.name);
        function_base.field_varint(2, lattice(name, result));
        for (_, parameter, parameter_symbol) in &type_parameters {
            let mut declaration = Pb::new();
            declaration.field_varint(1, *parameter_symbol);
            declaration.field_varint(2, u64::from(self.string("DEFINED")));
            let variance = match parameter.variance {
                TypeVariance::In => 0,
                TypeVariance::Out => 1,
                TypeVariance::Invariant => 2,
            };
            declaration.field_varint(4, (variance << 1) | (u64::from(parameter.reified) << 3));
            let mut encoded = Pb::new();
            encoded.field_message(1, &declaration);
            encoded.field_varint(2, u64::from(self.string(&parameter.name)));
            let bounds = parameter
                .bounds
                .iter()
                .map(|bound| u64::from(self.ty(*bound, &type_parameters)))
                .collect::<Vec<_>>();
            encoded.field_packed_varints(3, &bounds);
            function_base.repeated_message(3, &encoded);
        }

        let mut physical = 0usize;
        if let Some(dispatch) = function.dispatch_receiver {
            let parameter = self.parameter(
                "<this>",
                dispatch,
                parameter_symbols[physical],
                false,
                &type_parameters,
            );
            function_base.field_message(4, &parameter);
            physical += 1;
        }
        for (index, parameter) in function.parameters.iter().enumerate() {
            let encoded = self.parameter(
                &parameter.name,
                parameter.ty,
                parameter_symbols[physical],
                parameter.default_null,
                &type_parameters,
            );
            if index < function.context_count {
                function_base.repeated_message(9, &encoded);
            } else if function.extension_parameter == Some(index) {
                function_base.field_message(5, &encoded);
            } else {
                function_base.repeated_message(6, &encoded);
            }
            physical += 1;
        }
        let body = self.body(
            function,
            declaration_symbol,
            &parameter_symbols,
            &type_parameters,
        );
        function_base.field_varint(7, u64::from(body));
        let mut function_wrapper = Pb::new();
        function_wrapper.field_message(1, &function_base);
        let file_entry = self.file_entry(&function.file_name);
        function_wrapper.field_varint(3, u64::from(file_entry));
        let mut declaration = Pb::new();
        declaration.field_message(6, &function_wrapper);
        (declaration_signature, declaration.canonical().into_bytes())
    }

    fn parameter(
        &mut self,
        name: &str,
        ty: Ty,
        symbol: u64,
        default_null: bool,
        parameters: &[(usize, &PreparedTypeParameter, u64)],
    ) -> Pb {
        let mut base = Pb::new();
        base.field_varint(1, symbol);
        base.field_varint(2, u64::from(self.string("DEFINED")));
        let ty = self.ty(ty, parameters);
        let name = self.string(name);
        let mut parameter = Pb::new();
        parameter.field_message(1, &base);
        parameter.field_varint(2, lattice(name, ty));
        if default_null {
            let default = self.null_expression(parameters);
            parameter.field_varint(4, u64::from(default));
        }
        parameter
    }

    fn inline_only_annotation(&mut self) -> Pb {
        use crate::metadata::id_signature::{
            callable_signature, CallableShape, ClassScope, DeclarationContainer, Placement,
        };
        use crate::metadata::semantic::KotlinType;

        let package = ["kotlin".to_owned(), "internal".to_owned()];
        let classes = [ClassScope::<KotlinType> {
            name: "InlineOnly",
            type_parameters: Vec::new(),
            expect: false,
        }];
        let signature = callable_signature(
            DeclarationContainer {
                package: &package,
                classes: &classes,
                native_interop_library: false,
            },
            &CallableShape::<KotlinType> {
                name: "<init>",
                contexts: Vec::new(),
                receiver: None,
                params: Vec::new(),
                vararg: None,
                type_parameters: Vec::new(),
                expect: false,
                placement: Placement::Ordinary,
            },
        )
        .expect("InlineOnly has an empty signable constructor");
        let constructor = symbol(self.public_signature(&signature), CONSTRUCTOR_SYMBOL);
        let mut annotation = Pb::new();
        annotation.field_varint(1, constructor);
        annotation.field_varint(2, 0);
        annotation
    }

    /// One exact `null` default. Its expression type is Kotlin's `Nothing?`, not the receiving
    /// parameter type; the checked declaration already owns the implicit conversion.
    fn null_expression(&mut self, parameters: &[(usize, &PreparedTypeParameter, u64)]) -> u32 {
        let ty = self.ty(Ty::nullable(Ty::Nothing), parameters);
        let mut constant = Pb::new();
        constant.field_varint(1, 0);
        let mut expression = Pb::new();
        expression.field_varint(2, u64::from(ty));
        expression.field_message(5, &constant);
        let id = u32::try_from(self.bodies.len()).expect("body table fits u32");
        self.bodies.push(expression.canonical().into_bytes());
        id
    }

    fn body(
        &mut self,
        function: &PreparedFunction,
        declaration: u64,
        values: &[u64],
        parameters: &[(usize, &PreparedTypeParameter, u64)],
    ) -> u32 {
        let boolean = self.ty(Ty::Boolean, parameters);
        let value_slot = usize::try_from(function.body.value_slot).expect("value slot fits usize");
        let argument_ty = if function.dispatch_receiver.is_some() && value_slot == 0 {
            function.dispatch_receiver.expect("slot zero is dispatch")
        } else {
            let index = value_slot - usize::from(function.dispatch_receiver.is_some());
            function.parameters[index].ty
        };
        let argument_ty = self.ty(argument_ty, parameters);
        let operand = self.ty(
            function.type_parameters[function.body.type_parameter].ty,
            parameters,
        );

        let mut get = Pb::new();
        get.field_varint(1, values[value_slot]);
        let mut argument = Pb::new();
        argument.field_varint(2, u64::from(argument_ty));
        argument.field_message(6, &get);

        let mut operation = Pb::new();
        operation.field_varint(1, if function.body.negated { 8 } else { 7 });
        operation.field_varint(2, u64::from(operand));
        operation.field_message(3, &argument);
        let mut tested = Pb::new();
        tested.field_varint(2, u64::from(boolean));
        tested.field_message(14, &operation);

        let mut returned = Pb::new();
        returned.field_varint(1, declaration);
        returned.field_message(2, &tested);
        let mut return_expression = Pb::new();
        return_expression.field_varint(2, u64::from(boolean));
        return_expression.field_message(12, &returned);
        let mut statement = Pb::new();
        statement.field_message(3, &return_expression);
        let mut block = Pb::new();
        block.repeated_message(1, &statement);
        let mut body = Pb::new();
        body.field_message(4, &block);
        let id = u32::try_from(self.bodies.len()).expect("body table fits u32");
        self.bodies.push(body.canonical().into_bytes());
        id
    }

    fn ty(&mut self, ty: Ty, parameters: &[(usize, &PreparedTypeParameter, u64)]) -> u32 {
        let (inner, nullability) = match ty {
            Ty::Nullable(inner) => (*inner, 0),
            _ => (ty, 1),
        };
        let (classifier, arguments) = match inner {
            Ty::Unit => (self.class_symbol(TypeName::from("kotlin/Unit")), &[][..]),
            Ty::Nothing => (self.class_symbol(TypeName::from("kotlin/Nothing")), &[][..]),
            Ty::Boolean => (self.class_symbol(TypeName::from("kotlin/Boolean")), &[][..]),
            Ty::Obj(classifier, arguments) => (self.class_symbol(classifier), arguments),
            Ty::TyParam(..) => {
                let (_, _, symbol) = parameters
                    .iter()
                    .find(|(_, parameter, _)| parameter.ty == inner)
                    .expect("supported type parameter is declared by the function");
                (*symbol, &[][..])
            }
            _ => unreachable!("unsupported types are filtered before serialization"),
        };
        let mut simple = Pb::new();
        simple.field_varint(2, classifier);
        // `MARKED_NULLABLE` is protobuf zero, so omission does not mean non-null. Kotlin writes
        // `NOT_SPECIFIED` explicitly for an ordinary type-parameter occurrence.
        simple.field_varint(3, nullability);
        let arguments = arguments
            .iter()
            .map(|argument| {
                let (variance, ty) = match argument {
                    Ty::InProjection(inner) => (2, **inner),
                    Ty::OutProjection(inner) => (3, **inner),
                    Ty::StarProjection(_) => return 0,
                    other => (1, *other),
                };
                (u64::from(self.ty(ty, parameters)) << 2) | variance
            })
            .collect::<Vec<_>>();
        simple.field_packed_varints(4, &arguments);
        let mut encoded = Pb::new();
        encoded.field_message(5, &simple);
        let bytes = encoded.canonical().into_bytes();
        if let Some(index) = self.types.iter().position(|existing| existing == &bytes) {
            return u32::try_from(index).expect("type table fits u32");
        }
        let index = u32::try_from(self.types.len()).expect("type table fits u32");
        self.types.push(bytes);
        index
    }

    fn class_symbol(&mut self, classifier: TypeName) -> u64 {
        let signature = KlibPublicIdSignature::classifier(classifier);
        let slot = self.public_signature(&signature);
        symbol(slot, CLASS_SYMBOL)
    }

    fn public_signature(&mut self, signature: &KlibPublicIdSignature) -> u32 {
        let package = signature
            .package()
            .segments()
            .iter()
            .map(|segment| u64::from(self.string(segment)))
            .collect::<Vec<_>>();
        let declaration = signature
            .declaration()
            .segments()
            .iter()
            .map(|segment| u64::from(self.string(segment)))
            .collect::<Vec<_>>();
        let mut common = Pb::new();
        common.field_packed_varints(1, &package);
        common.field_packed_varints(2, &declaration);
        if let Some(member) = signature.member_id() {
            common.field_fixed64(6, member);
        }
        if signature.mask() != 0 {
            common.field_varint(4, signature.mask());
        }
        let mut encoded = Pb::new();
        encoded.field_message(1, &common);
        self.signature(encoded.canonical().into_bytes())
    }

    fn local_signature(&mut self) -> u32 {
        let mut encoded = Pb::new();
        encoded.field_varint(4, self.signatures.len() as u64);
        self.signature(encoded.into_bytes())
    }

    fn signature(&mut self, bytes: Vec<u8>) -> u32 {
        if let Some(index) = self
            .signatures
            .iter()
            .position(|existing| existing == &bytes)
        {
            return u32::try_from(index).expect("signature table fits u32");
        }
        let index = u32::try_from(self.signatures.len()).expect("signature table fits u32");
        self.signatures.push(bytes);
        index
    }

    fn string(&mut self, value: &str) -> u32 {
        if let Some(index) = self.strings.iter().position(|existing| existing == value) {
            return u32::try_from(index).expect("string table fits u32");
        }
        let index = u32::try_from(self.strings.len()).expect("string table fits u32");
        self.strings.push(value.to_owned());
        index
    }

    fn file_entry(&mut self, name: &str) -> u32 {
        let name = self.string(name);
        let mut entry = Pb::new();
        entry.field_varint(4, u64::from(name));
        let bytes = entry.into_bytes();
        if let Some(index) = self
            .file_entries
            .iter()
            .position(|existing| existing == &bytes)
        {
            return u32::try_from(index).expect("file-entry table fits u32");
        }
        let index = u32::try_from(self.file_entries.len()).expect("file-entry table fits u32");
        self.file_entries.push(bytes);
        index
    }
}

fn symbol(signature: u32, kind: u64) -> u64 {
    (u64::from(signature) << 8) | kind
}

/// Kotlin IR's `BinaryLattice`: even bits carry the name, odd bits the type index.
fn lattice(name: u32, ty: u32) -> u64 {
    fn spread(mut value: u64) -> u64 {
        value &= 0xffff_ffff;
        value = (value | (value << 16)) & 0x0000_ffff_0000_ffff;
        value = (value | (value << 8)) & 0x00ff_00ff_00ff_00ff;
        value = (value | (value << 4)) & 0x0f0f_0f0f_0f0f_0f0f;
        value = (value | (value << 2)) & 0x3333_3333_3333_3333;
        (value | (value << 1)) & 0x5555_5555_5555_5555
    }
    spread(u64::from(name)) | (spread(u64::from(ty)) << 1)
}

fn entry_table(entries: &[Vec<u8>]) -> Vec<u8> {
    let count = i32::try_from(entries.len()).expect("entry count fits i32");
    let mut bytes = (-count).to_be_bytes().to_vec();
    for entry in entries {
        leb128(
            u64::try_from(entry.len()).expect("entry length fits u64"),
            &mut bytes,
        );
    }
    for entry in entries {
        bytes.extend_from_slice(entry);
    }
    bytes
}

fn declaration_table(declarations: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(
        &u32::try_from(declarations.len())
            .expect("declaration count fits u32")
            .to_be_bytes(),
    );
    let mut offset = 4 + declarations.len() * 12;
    for (signature, declaration) in declarations {
        bytes.extend_from_slice(&signature.to_be_bytes());
        bytes.extend_from_slice(
            &u32::try_from(offset)
                .expect("declaration offset fits u32")
                .to_be_bytes(),
        );
        bytes.extend_from_slice(
            &u32::try_from(declaration.len())
                .expect("declaration length fits u32")
                .to_be_bytes(),
        );
        offset += declaration.len();
    }
    for (_, declaration) in declarations {
        bytes.extend_from_slice(declaration);
    }
    bytes
}

fn per_file(file: Vec<u8>) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(8 + file.len());
    bytes.extend_from_slice(&1u32.to_be_bytes());
    bytes.extend_from_slice(
        &u32::try_from(file.len())
            .expect("per-file data fits u32")
            .to_be_bytes(),
    );
    bytes.extend(file);
    bytes
}

fn leb128(mut value: u64, bytes: &mut Vec<u8>) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        bytes.push(byte | if value == 0 { 0 } else { 0x80 });
        if value == 0 {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::klib_ir::tree::{KlibIrBody, KlibIrExprKind, KlibIrNullability};
    use crate::metadata::klib_ir::{
        read_inlinable_declaration_trees, read_inlinable_declaration_trees_where, KlibIrSignature,
    };

    fn fixture() -> (PreparedFunction, KlibPublicIdSignature) {
        let signature = KlibPublicIdSignature::classifier(TypeName::from("fixture/probe"));
        let bound = Ty::nullable(Ty::obj("kotlin/Any"));
        let parameter = Ty::ty_param("fixture.T", bound);
        (
            PreparedFunction {
                signature: signature.clone(),
                name: "probe".to_owned(),
                file_name: "Probe.kt".to_owned(),
                flags: 1030,
                inline_only: false,
                dispatch_receiver: None,
                parameters: vec![PreparedParameter {
                    name: "value".to_owned(),
                    ty: Ty::nullable(Ty::obj("kotlin/Any")),
                    default_null: true,
                }],
                context_count: 0,
                extension_parameter: None,
                result: Ty::Boolean,
                type_parameters: vec![PreparedTypeParameter {
                    name: "T".to_owned(),
                    ty: parameter,
                    bounds: vec![bound],
                    variance: TypeVariance::Invariant,
                    reified: true,
                }],
                body: ReifiedTypeTest {
                    value_slot: 0,
                    type_parameter: 0,
                    negated: false,
                },
            },
            signature,
        )
    }

    #[test]
    fn prepared_inline_component_round_trips_exact_type_test_and_provenance() {
        let (function, signature) = fixture();
        let artifacts = artifacts(&[function]);
        assert_eq!(artifacts.len(), 8);
        let file_entries = artifacts
            .iter()
            .find(|(name, _)| name.ends_with("/fileEntries.knf"))
            .map(|(_, bytes)| bytes)
            .expect("prepared component has a file-entry table");
        assert_ne!(file_entries, &per_file(entry_table(&[])));

        let root = std::env::temp_dir().join(format!(
            "krusty-inline-ir-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let mut writer = crate::klib::write::KlibWriter::native_test_fixture("fixture");
        for (name, bytes) in artifacts {
            writer.add_file(&name, bytes);
        }
        for (name, bytes) in writer.entries() {
            let path = root.join(name);
            std::fs::create_dir_all(path.parent().expect("entry has a parent"))
                .expect("create fixture directory");
            std::fs::write(path, bytes).expect("write fixture entry");
        }
        let archive = crate::klib::KlibArchive::open(&root).expect("open generated KLIB");
        let trees = read_inlinable_declaration_trees(&archive).expect("decode prepared IR");
        let (arena, decoded) = trees
            .function(&KlibIrSignature::Public(signature))
            .expect("the exact public declaration is indexed");
        assert_eq!(decoded.base.flags, 1030);
        assert_eq!(decoded.prepared_inline_file, Some(0));
        let [parameter] = decoded.regular_parameters.as_slice() else {
            panic!("fixture has one regular parameter")
        };
        let default = parameter
            .default_value
            .expect("parameter keeps its default");
        assert!(matches!(
            arena.expr(default).kind,
            KlibIrExprKind::Const(crate::metadata::klib_ir::KlibIrConstant::Null)
        ));
        let KlibIrBody::Block(statements) = decoded.body.as_ref().expect("function has a body")
        else {
            panic!("function body is a block")
        };
        let [statement] = statements.as_slice() else {
            panic!("function body is one statement")
        };
        let crate::metadata::klib_ir::tree::KlibIrStatement::Expression(returned) = statement
        else {
            panic!("body statement is an expression")
        };
        let KlibIrExprKind::Return { value, .. } = &arena.expr(*returned).kind else {
            panic!("body expression is a return")
        };
        let KlibIrExprKind::TypeOperator { operand, .. } = &arena.expr(*value).kind else {
            panic!("returned expression is a type test")
        };
        let crate::metadata::klib_ir::tree::KlibIrType::Simple {
            nullability,
            arguments,
            abbreviation,
            ..
        } = arena.ty(*operand)
        else {
            panic!("type-test operand is a simple type parameter")
        };
        assert_eq!(*nullability, KlibIrNullability::NotSpecified);
        assert!(arguments.is_empty());
        assert!(abbreviation.is_none());
        std::fs::remove_dir_all(root).expect("remove fixture directory");
    }

    #[test]
    fn a_filtered_prepared_decode_skips_unselected_declaration_bodies() {
        let (kept, kept_signature) = fixture();
        let (mut skipped, _) = fixture();
        let skipped_signature =
            KlibPublicIdSignature::classifier(TypeName::from("fixture/skipped"));
        skipped.signature = skipped_signature.clone();
        skipped.name = "skipped".to_owned();

        let root = std::env::temp_dir().join(format!(
            "krusty-filtered-inline-ir-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let mut writer = crate::klib::write::KlibWriter::native_test_fixture("fixture");
        for (name, bytes) in artifacts(&[kept, skipped]) {
            writer.add_file(&name, bytes);
        }
        for (name, bytes) in writer.entries() {
            let path = root.join(name);
            std::fs::create_dir_all(path.parent().expect("entry has a parent"))
                .expect("create fixture directory");
            std::fs::write(path, bytes).expect("write fixture entry");
        }
        let archive = crate::klib::KlibArchive::open(&root).expect("open generated KLIB");
        let mut offered = Vec::new();
        let trees = read_inlinable_declaration_trees_where(&archive, |identity| {
            offered.push(identity.clone());
            identity == &KlibIrSignature::Public(kept_signature.clone())
        })
        .expect("decode selected prepared IR");

        assert_eq!(
            offered,
            [
                KlibIrSignature::Public(kept_signature.clone()),
                KlibIrSignature::Public(skipped_signature.clone()),
            ]
        );
        assert!(trees
            .function(&KlibIrSignature::Public(kept_signature))
            .is_some());
        assert!(trees
            .function(&KlibIrSignature::Public(skipped_signature))
            .is_none());
        std::fs::remove_dir_all(root).expect("remove fixture directory");
    }

    #[test]
    fn inline_only_prepared_copy_has_exact_private_annotated_flags() {
        let (mut function, signature) = fixture();
        function.flags = 1027;
        function.inline_only = true;
        let root = std::env::temp_dir().join(format!(
            "krusty-inline-only-ir-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let mut writer = crate::klib::write::KlibWriter::native_test_fixture("fixture");
        for (name, bytes) in artifacts(&[function]) {
            writer.add_file(&name, bytes);
        }
        for (name, bytes) in writer.entries() {
            let path = root.join(name);
            std::fs::create_dir_all(path.parent().expect("entry has a parent"))
                .expect("create fixture directory");
            std::fs::write(path, bytes).expect("write fixture entry");
        }
        let archive = crate::klib::KlibArchive::open(&root).expect("open generated KLIB");
        let trees = read_inlinable_declaration_trees(&archive).expect("decode prepared IR");
        let (_, decoded) = trees
            .function(&KlibIrSignature::Public(signature))
            .expect("the exact public declaration is indexed");
        assert_eq!(decoded.base.flags, 1027);
        let [annotation] = decoded.base.annotations.as_slice() else {
            panic!("InlineOnly copy has one annotation")
        };
        assert_eq!(
            annotation.access.symbol.kind,
            crate::metadata::klib_ir::KlibIrSymbolKind::Constructor
        );
        assert_eq!(annotation.constructor_type_arguments, 0);
        std::fs::remove_dir_all(root).expect("remove fixture directory");
    }
}
