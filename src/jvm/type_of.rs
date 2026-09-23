//! JVM realization of Kotlin's `typeOf<T>()`.
//!
//! Common IR keeps the selected type argument as a complete semantic [`Ty`]. This module turns it
//! into the instruction sequence kotlinc's `generateTypeOf` produces: a chain of
//! `kotlin.jvm.internal.Reflection` factory calls that builds the runtime `KType`, with each type
//! argument wrapped in a `KTypeProjection` and each type parameter described by a
//! `KTypeParameter`. A reified type parameter of the function being emitted is not known until a
//! call site splices the body, so it becomes kotlinc's `reifiedOperationMarker(6, "T")` followed
//! by an `aconst_null` placeholder, which the splicer replaces with the substituted type's own
//! sequence.
//!
//! The generator produces backend-neutral [`TypeOfInsn`]s. The method emitter and the bytecode
//! splicer each encode them into their own instruction representation, so both sites build exactly
//! the same sequence.

use std::collections::HashMap;

use crate::ir::{FunId, IrFile, IrTypeParameter};
use crate::types::{Ty, TypeName, TypeVariance};

const REFLECTION: &str = "kotlin/jvm/internal/Reflection";
const K_TYPE: &str = "kotlin/reflect/KType";
const K_TYPE_PROJECTION: &str = "kotlin/reflect/KTypeProjection";
const K_TYPE_PROJECTION_COMPANION: &str = "kotlin/reflect/KTypeProjection$Companion";
const K_VARIANCE: &str = "kotlin/reflect/KVariance";

/// kotlinc's `ReifiedTypeInliner.OperationKind.TYPE_OF`.
pub(super) const TYPE_OF_MARKER: i32 = 6;

/// One instruction of a `typeOf` realization, independent of the constant pool it will be encoded
/// against.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TypeOfInsn {
    /// `ldc` of a class constant. Array classes are spelled by their descriptor (`[I`).
    LdcClass(String),
    /// `getstatic <wrapper>.TYPE`, the `Class` of a JVM primitive.
    PrimitiveClass(&'static str),
    LdcString(String),
    PushInt(i32),
    AconstNull,
    Dup,
    New(&'static str),
    ANewArray(&'static str),
    AAStore,
    GetStatic {
        owner: &'static str,
        name: &'static str,
        descriptor: &'static str,
    },
    InvokeStatic {
        owner: &'static str,
        name: &'static str,
        descriptor: String,
    },
    InvokeVirtual {
        owner: &'static str,
        name: &'static str,
        descriptor: String,
    },
    InvokeSpecial {
        owner: &'static str,
        name: &'static str,
        descriptor: String,
    },
    /// `iconst 6; ldc "<argument>"; invokestatic Intrinsics.reifiedOperationMarker`.
    ReifiedMarker(String),
}

/// Why a `typeOf` operand has no JVM realization.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum TypeOfError {
    /// kotlinc rejects `typeOf` of a suspend function type (`TYPEOF_SUSPEND_TYPE`).
    SuspendFunctionType,
    /// kotlinc rejects a non-reified type parameter whose bounds reach itself
    /// (`TYPEOF_NON_REIFIED_TYPE_PARAMETER_WITH_RECURSIVE_BOUND`).
    RecursiveBound(String),
    /// The type names a type parameter that no declaration of this file owns.
    UnknownTypeParameter(String),
    /// The operand is not a type a program can observe (an error or undetermined type).
    Undescribable(Ty),
}

impl std::fmt::Display for TypeOfError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TypeOfError::SuspendFunctionType => {
                write!(f, "typeOf of a suspend function type is not supported")
            }
            TypeOfError::RecursiveBound(name) => write!(
                f,
                "typeOf of non-reified type parameter {name} with a recursive bound is not supported"
            ),
            TypeOfError::UnknownTypeParameter(name) => {
                write!(f, "typeOf names type parameter {name} with no declaration here")
            }
            TypeOfError::Undescribable(ty) => write!(f, "typeOf cannot describe {ty:?}"),
        }
    }
}

/// The declaration that owns a type parameter, as the runtime identifies it.
#[derive(Clone, Copy, Debug)]
enum Container {
    Class(TypeName),
    Function(FunId),
    /// A property, named through its getter: kotlinc describes a generic property's type
    /// parameter by a property reference whose signature is the getter's.
    Property {
        getter: FunId,
        is_var: bool,
    },
}

/// Every type parameter this file declares, by semantic identity, with its owner.
pub(super) struct TypeParameters<'a> {
    ir: &'a IrFile,
    facade: &'a str,
    declarations: HashMap<&'a str, (&'a IrTypeParameter, Container)>,
}

impl<'a> TypeParameters<'a> {
    pub(super) fn new(ir: &'a IrFile, facade: &'a str) -> Self {
        let mut declarations = HashMap::new();
        for (&function, signature) in &ir.signatures {
            for parameter in &signature.type_params {
                declarations.insert(
                    parameter.semantic_name.as_str(),
                    (parameter, Container::Function(function)),
                );
            }
        }
        // A property's accessors are functions too, but their type parameters belong to the
        // property, so these entries replace the accessor-signature ones above.
        let properties = ir
            .member_ext_props
            .values()
            .flatten()
            .map(|property| (&property.type_params, property.getter, property.is_var))
            .chain(
                ir.top_level_generic_properties
                    .iter()
                    .map(|property| (&property.type_params, property.getter, property.is_var)),
            );
        for (type_params, getter, is_var) in properties {
            for parameter in type_params {
                declarations.insert(
                    parameter.semantic_name.as_str(),
                    (parameter, Container::Property { getter, is_var }),
                );
            }
        }
        for (classifier, signature) in ir.class_signatures() {
            for parameter in &signature.type_params {
                declarations.insert(
                    parameter.semantic_name.as_str(),
                    (parameter, Container::Class(classifier)),
                );
            }
        }
        Self {
            ir,
            facade,
            declarations,
        }
    }

    fn declaration(&self, identity: &str) -> Result<(&'a IrTypeParameter, Container), TypeOfError> {
        self.declarations
            .get(identity)
            .copied()
            .ok_or_else(|| TypeOfError::UnknownTypeParameter(identity.to_owned()))
    }

    /// Declared upper bounds. A parameter without one is bounded by `Any?`, which kotlinc's IR
    /// spells as an explicit super type.
    fn bounds(parameter: &IrTypeParameter) -> Vec<Ty> {
        if parameter.bounds.is_empty() {
            vec![Ty::nullable(Ty::obj("kotlin/Any"))]
        } else {
            parameter.bounds.iter().map(|(bound, _)| *bound).collect()
        }
    }
}

/// Append the realization of `typeOf<ty>()` to `out`.
pub(super) fn generate(
    ty: Ty,
    parameters: &TypeParameters<'_>,
    out: &mut Vec<TypeOfInsn>,
) -> Result<(), TypeOfError> {
    Generator { parameters, out }.type_of(ty, false)
}

struct Generator<'g, 'a> {
    parameters: &'g TypeParameters<'a>,
    out: &'g mut Vec<TypeOfInsn>,
}

fn k_type_descriptor(arguments: &[&str]) -> String {
    format!("({})L{K_TYPE};", arguments.concat())
}

/// Mutable Kotlin collection classifiers, which share a JVM class with their read-only face and
/// are told apart only by `Reflection.mutableCollectionType`.
fn is_mutable_collection(name: TypeName) -> bool {
    [
        "kotlin/collections/MutableIterator",
        "kotlin/collections/MutableIterable",
        "kotlin/collections/MutableCollection",
        "kotlin/collections/MutableList",
        "kotlin/collections/MutableListIterator",
        "kotlin/collections/MutableSet",
        "kotlin/collections/MutableMap",
        "kotlin/collections/MutableMap.MutableEntry",
        "kotlin/collections/MutableMap$MutableEntry",
    ]
    .into_iter()
    .any(|candidate| name.matches(candidate))
}

/// The class instance kotlinc's `generateClassInstance(wrapPrimitives = false)` pushes for a
/// classifier type: a primitive's `TYPE` for a non-null signed scalar, otherwise the mapped class,
/// with arrays spelled by their descriptor and value classes kept as themselves.
fn class_instance(ty: Ty) -> Result<TypeOfInsn, TypeOfError> {
    let (inner, nullable) = match ty {
        Ty::Nullable(inner) => (*inner, true),
        other => (other, false),
    };
    if !nullable && inner.is_jvm_scalar() && !inner.is_unsigned() {
        let wrapper = crate::jvm::jvm_class_map::wrapper_internal(inner)
            .ok_or(TypeOfError::Undescribable(ty))?;
        return Ok(TypeOfInsn::PrimitiveClass(wrapper));
    }
    let class = match inner {
        Ty::Unit => "kotlin/Unit".to_owned(),
        Ty::Nothing => "java/lang/Void".to_owned(),
        Ty::Fun(signature) => {
            if signature.suspend {
                return Err(TypeOfError::SuspendFunctionType);
            }
            crate::jvm::names::function_interface_internal_name(signature.params.len())
        }
        Ty::Obj(name, _)
            if name.matches("kotlin/Array") || crate::types::prim_array_element(name).is_some() =>
        {
            if crate::types::prim_array_element(name).is_some_and(Ty::is_unsigned) {
                crate::jvm::names::classfile_internal_name(&name.render())
            } else {
                crate::jvm::names::type_descriptor(inner)
            }
        }
        Ty::Obj(name, _) => crate::jvm::names::classfile_internal_name(&name.render()),
        _ => return Err(TypeOfError::Undescribable(ty)),
    };
    Ok(TypeOfInsn::LdcClass(class))
}

impl Generator<'_, '_> {
    fn push(&mut self, instruction: TypeOfInsn) {
        self.out.push(instruction);
    }

    fn reflection(&mut self, name: &'static str, descriptor: String) {
        self.push(TypeOfInsn::InvokeStatic {
            owner: REFLECTION,
            name,
            descriptor,
        });
    }

    /// kotlinc's `unrollArrayIfFewerThan`: fewer than `limit` elements are passed as separate
    /// arguments, any more as one array. Returns the argument descriptors pushed.
    fn unrolled(
        &mut self,
        elements: &[Ty],
        limit: usize,
        element_class: &'static str,
        mut element: impl FnMut(&mut Self, Ty) -> Result<(), TypeOfError>,
    ) -> Result<Vec<String>, TypeOfError> {
        let descriptor = format!("L{element_class};");
        if elements.len() < limit {
            for &value in elements {
                element(self, value)?;
            }
            return Ok(vec![descriptor; elements.len()]);
        }
        self.push(TypeOfInsn::PushInt(elements.len() as i32));
        self.push(TypeOfInsn::ANewArray(element_class));
        for (index, &value) in elements.iter().enumerate() {
            self.push(TypeOfInsn::Dup);
            self.push(TypeOfInsn::PushInt(index as i32));
            element(self, value)?;
            self.push(TypeOfInsn::AAStore);
        }
        Ok(vec![format!("[{descriptor}")])
    }

    fn type_of(&mut self, ty: Ty, type_parameter_bound: bool) -> Result<(), TypeOfError> {
        if let Ty::PlatformNullable(lower) = ty {
            // A flexible type: its lower bound, its upper bound, then the combination.
            self.type_of(*lower, type_parameter_bound)?;
            self.type_of(Ty::nullable(*lower), type_parameter_bound)?;
            self.reflection(
                "platformType",
                k_type_descriptor(&[&format!("L{K_TYPE};"), &format!("L{K_TYPE};")]),
            );
            return Ok(());
        }
        let nullable = ty.is_nullable();
        let inner = ty.non_null();
        let arguments = if let Ty::TyParam(identity, _) = inner {
            let (parameter, container) = self.parameters.declaration(identity)?;
            if !type_parameter_bound && parameter.reified {
                let suffix = if nullable { "?" } else { "" };
                self.push(TypeOfInsn::ReifiedMarker(format!(
                    "{}{suffix}",
                    parameter.name
                )));
                self.push(TypeOfInsn::AconstNull);
                return Ok(());
            }
            if references_recursive_bound(self.parameters, identity, &mut Vec::new())? {
                return Err(TypeOfError::RecursiveBound(parameter.name.clone()));
            }
            self.type_parameter(parameter, container)?;
            vec!["Lkotlin/reflect/KClassifier;".to_owned()]
        } else {
            // A nullable primitive is its wrapper class, as kotlinc maps `Int?` to `Integer`.
            let classified = if nullable { Ty::nullable(inner) } else { inner };
            self.push(class_instance(classified)?);
            let arguments = classifier_arguments(inner);
            let mut descriptors = vec!["Ljava/lang/Class;".to_owned()];
            descriptors.extend(self.unrolled(
                &arguments,
                3,
                K_TYPE_PROJECTION,
                |generator, argument| generator.projection(argument, type_parameter_bound),
            )?);
            descriptors
        };
        let arguments = arguments.iter().map(String::as_str).collect::<Vec<_>>();
        self.reflection(
            if nullable { "nullableTypeOf" } else { "typeOf" },
            k_type_descriptor(&arguments),
        );
        match inner {
            Ty::Obj(name, _) if is_mutable_collection(name) => self.reflection(
                "mutableCollectionType",
                k_type_descriptor(&[&format!("L{K_TYPE};")]),
            ),
            Ty::Nothing => {
                self.reflection("nothingType", k_type_descriptor(&[&format!("L{K_TYPE};")]))
            }
            _ => {}
        }
        Ok(())
    }

    fn projection(&mut self, argument: Ty, type_parameter_bound: bool) -> Result<(), TypeOfError> {
        self.push(TypeOfInsn::GetStatic {
            owner: K_TYPE_PROJECTION,
            name: "Companion",
            descriptor: "Lkotlin/reflect/KTypeProjection$Companion;",
        });
        let (ty, factory) = match argument {
            Ty::StarProjection(_) => {
                self.push(TypeOfInsn::InvokeVirtual {
                    owner: K_TYPE_PROJECTION_COMPANION,
                    name: "getSTAR",
                    descriptor: format!("()L{K_TYPE_PROJECTION};"),
                });
                return Ok(());
            }
            Ty::InProjection(ty) => (*ty, "contravariant"),
            Ty::OutProjection(ty) => (*ty, "covariant"),
            ty => (ty, "invariant"),
        };
        self.type_of(ty, type_parameter_bound)?;
        self.push(TypeOfInsn::InvokeVirtual {
            owner: K_TYPE_PROJECTION_COMPANION,
            name: factory,
            descriptor: format!("(L{K_TYPE};)L{K_TYPE_PROJECTION};"),
        });
        Ok(())
    }

    /// kotlinc's `generateNonReifiedTypeParameter`: the owner, the `KTypeParameter`, and its
    /// upper bounds, leaving the parameter on the stack.
    fn type_parameter(
        &mut self,
        parameter: &IrTypeParameter,
        container: Container,
    ) -> Result<(), TypeOfError> {
        self.container(container);
        self.push(TypeOfInsn::LdcString(parameter.name.clone()));
        self.push(TypeOfInsn::GetStatic {
            owner: K_VARIANCE,
            name: match parameter.variance {
                TypeVariance::Invariant => "INVARIANT",
                TypeVariance::In => "IN",
                TypeVariance::Out => "OUT",
            },
            descriptor: "Lkotlin/reflect/KVariance;",
        });
        self.push(TypeOfInsn::PushInt(i32::from(parameter.reified)));
        self.reflection(
            "typeParameter",
            "(Ljava/lang/Object;Ljava/lang/String;Lkotlin/reflect/KVariance;Z)Lkotlin/reflect/KTypeParameter;"
                .to_owned(),
        );
        let bounds = TypeParameters::bounds(parameter);
        self.push(TypeOfInsn::Dup);
        let arguments = self.unrolled(&bounds, 2, K_TYPE, |generator, bound| {
            generator.type_of(bound, true)
        })?;
        self.reflection(
            "setUpperBounds",
            format!("(Lkotlin/reflect/KTypeParameter;{})V", arguments.concat()),
        );
        Ok(())
    }

    fn container(&mut self, container: Container) {
        match container {
            Container::Class(classifier) => {
                self.push(TypeOfInsn::LdcClass(
                    crate::jvm::names::classfile_internal_name(&classifier.render()),
                ));
                self.reflection(
                    "getOrCreateKotlinClass",
                    "(Ljava/lang/Class;)Lkotlin/reflect/KClass;".to_owned(),
                );
            }
            Container::Function(function) => {
                self.callable_reference(function, "kotlin/jvm/internal/FunctionReferenceImpl", true)
            }
            Container::Property { getter, is_var } => {
                let arity = self.parameters.ir.functions[getter as usize].params.len()
                    + usize::from(
                        self.parameters.ir.functions[getter as usize]
                            .dispatch_receiver
                            .is_some(),
                    );
                let class = match (is_var, arity) {
                    (false, 0) => "kotlin/jvm/internal/PropertyReference0Impl",
                    (false, 1) => "kotlin/jvm/internal/PropertyReference1Impl",
                    (false, _) => "kotlin/jvm/internal/PropertyReference2Impl",
                    (true, 0) => "kotlin/jvm/internal/MutablePropertyReference0Impl",
                    (true, 1) => "kotlin/jvm/internal/MutablePropertyReference1Impl",
                    (true, _) => "kotlin/jvm/internal/MutablePropertyReference2Impl",
                };
                self.callable_reference(getter, class, false);
            }
        }
    }

    /// kotlinc's reflective reference to a declared callable: `new <class>(arity?, owner,
    /// name, jvmSignature, topLevelFlag)`. A property is named by its own name but signed by its
    /// getter, and carries no arity.
    fn callable_reference(&mut self, function: FunId, class: &'static str, with_arity: bool) {
        let ir = self.parameters.ir;
        let declaration = &ir.functions[function as usize];
        // A member names its class; a top-level callable names the facade of the file declaring
        // it, which for a foreign inline template is not the host file.
        let owner = ir
            .classes
            .iter()
            .find(|class| class.methods.contains(&function))
            .map(|class| class.fq_name_id())
            .or(declaration.dispatch_receiver);
        let facade = ir.foreign_template_facades.get(&function).map_or_else(
            || self.parameters.facade.to_owned(),
            |facade| crate::jvm::names::classfile_internal_name(&facade.render()),
        );
        let name = if with_arity {
            ir.fn_source_names
                .get(&function)
                .cloned()
                .unwrap_or_else(|| declaration.name.clone())
        } else {
            self.property_name(function)
        };
        let signature = format!(
            "{}{}",
            declaration.name,
            super::ir_emit::ir_method_desc(&declaration.params, &declaration.ret)
        );
        self.push(TypeOfInsn::New(class));
        self.push(TypeOfInsn::Dup);
        let mut descriptor = String::from("(");
        if with_arity {
            let arity =
                declaration.params.len() + usize::from(declaration.dispatch_receiver.is_some());
            self.push(TypeOfInsn::PushInt(arity as i32));
            descriptor.push('I');
        }
        descriptor.push_str("Ljava/lang/Class;Ljava/lang/String;Ljava/lang/String;I)V");
        self.push(TypeOfInsn::LdcClass(owner.map_or(facade, |owner| {
            crate::jvm::names::classfile_internal_name(&owner.render())
        })));
        self.push(TypeOfInsn::LdcString(name));
        self.push(TypeOfInsn::LdcString(signature));
        self.push(TypeOfInsn::PushInt(i32::from(owner.is_none())));
        self.push(TypeOfInsn::InvokeSpecial {
            owner: class,
            name: "<init>",
            descriptor,
        });
    }

    /// The Kotlin name of the property whose getter is `getter`.
    fn property_name(&self, getter: FunId) -> String {
        let ir = self.parameters.ir;
        ir.member_ext_props
            .values()
            .flatten()
            .find(|property| property.getter == getter)
            .map(|property| property.name.clone())
            .or_else(|| {
                ir.top_level_generic_properties
                    .iter()
                    .find(|property| property.getter == getter)
                    .map(|property| property.name.clone())
            })
            .unwrap_or_else(|| ir.functions[getter as usize].name.clone())
    }
}

/// The type arguments of a classifier type in runtime order. A function type carries its
/// parameters followed by its result.
fn classifier_arguments(ty: Ty) -> Vec<Ty> {
    match ty {
        Ty::Obj(_, arguments) => arguments.to_vec(),
        Ty::Fun(signature) => signature
            .params
            .iter()
            .copied()
            .chain(std::iter::once(signature.ret))
            .collect(),
        _ => Vec::new(),
    }
}

/// JVM operand-stack words a method descriptor's arguments and result occupy. Every value a
/// `typeOf` realization passes is a reference or an `int`, so each takes one word.
fn descriptor_words(descriptor: &str) -> (i32, i32) {
    let (arguments, result) = crate::jvm::names::parse_method_descriptor(descriptor)
        .expect("typeOf realizations name well-formed method descriptors");
    (arguments.len() as i32, i32::from(result != "V"))
}

/// Encode a realization into a method body being emitted.
pub(super) fn encode(
    instructions: &[TypeOfInsn],
    code: &mut super::classfile::CodeBuilder,
    cw: &mut super::classfile::ClassWriter,
) {
    for instruction in instructions {
        match instruction {
            TypeOfInsn::LdcClass(class) => code.ldc_class(class, cw),
            TypeOfInsn::PrimitiveClass(wrapper) => {
                let field = cw.fieldref(wrapper, "TYPE", "Ljava/lang/Class;");
                code.getstatic(field, 1);
            }
            TypeOfInsn::LdcString(value) => code.push_string(value, cw),
            TypeOfInsn::PushInt(value) => code.push_int(*value, cw),
            TypeOfInsn::AconstNull => code.aconst_null(),
            TypeOfInsn::Dup => code.dup(),
            TypeOfInsn::New(class) => {
                let class = cw.class_ref(class);
                code.new_obj(class);
            }
            TypeOfInsn::ANewArray(class) => {
                let class = cw.class_ref(class);
                code.anewarray(class);
            }
            TypeOfInsn::AAStore => code.array_store(0x53, 1),
            TypeOfInsn::GetStatic {
                owner,
                name,
                descriptor,
            } => {
                let field = cw.fieldref(owner, name, descriptor);
                code.getstatic(field, 1);
            }
            TypeOfInsn::InvokeStatic {
                owner,
                name,
                descriptor,
            } => {
                let (arguments, result) = descriptor_words(descriptor);
                let method = cw.methodref(owner, name, descriptor);
                code.invokestatic(method, arguments, result);
            }
            TypeOfInsn::InvokeVirtual {
                owner,
                name,
                descriptor,
            } => {
                let (arguments, result) = descriptor_words(descriptor);
                let method = cw.methodref(owner, name, descriptor);
                code.invokevirtual(method, arguments, result);
            }
            TypeOfInsn::InvokeSpecial {
                owner,
                name,
                descriptor,
            } => {
                let (arguments, result) = descriptor_words(descriptor);
                let method = cw.methodref(owner, name, descriptor);
                code.invokespecial(method, arguments, result);
            }
            TypeOfInsn::ReifiedMarker(argument) => {
                code.push_int(TYPE_OF_MARKER, cw);
                code.push_string(argument, cw);
                let marker = cw.methodref(
                    "kotlin/jvm/internal/Intrinsics",
                    "reifiedOperationMarker",
                    "(ILjava/lang/String;)V",
                );
                code.invokestatic(marker, 2, 0);
            }
        }
    }
}

/// The operand-stack peak a realization reaches above its starting height.
pub(super) fn max_stack(instructions: &[TypeOfInsn]) -> u16 {
    let (mut height, mut peak) = (0i32, 0i32);
    for instruction in instructions {
        height += match instruction {
            TypeOfInsn::LdcClass(_)
            | TypeOfInsn::PrimitiveClass(_)
            | TypeOfInsn::LdcString(_)
            | TypeOfInsn::PushInt(_)
            | TypeOfInsn::AconstNull
            | TypeOfInsn::Dup
            | TypeOfInsn::New(_)
            | TypeOfInsn::GetStatic { .. } => 1,
            TypeOfInsn::ANewArray(_) => 0,
            TypeOfInsn::AAStore => -3,
            TypeOfInsn::InvokeStatic { descriptor, .. } => {
                let (arguments, result) = descriptor_words(descriptor);
                result - arguments
            }
            TypeOfInsn::InvokeVirtual { descriptor, .. }
            | TypeOfInsn::InvokeSpecial { descriptor, .. } => {
                let (arguments, result) = descriptor_words(descriptor);
                result - arguments - 1
            }
            // The marker's two arguments are pushed and consumed by the marker call.
            TypeOfInsn::ReifiedMarker(_) => {
                peak = peak.max(height + 2);
                0
            }
        };
        peak = peak.max(height);
    }
    peak as u16
}

/// Encode a realization as spliced instructions against the host class's constant pool.
pub(super) fn encode_insns(
    instructions: &[TypeOfInsn],
    cw: &mut super::classfile::ClassWriter,
) -> Vec<super::inline::Insn> {
    use super::inline::Insn;
    fn plain(op: u8, operands: Vec<u8>) -> Insn {
        Insn::Plain { op, operands }
    }
    fn pooled(op: u8, index: u16) -> Insn {
        plain(op, index.to_be_bytes().to_vec())
    }
    fn ldc(index: u16) -> Insn {
        match u8::try_from(index) {
            Ok(index) => plain(0x12, vec![index]),
            Err(_) => pooled(0x13, index),
        }
    }
    fn push_int(value: i32) -> Insn {
        match value {
            -1..=5 => plain((0x03 + value) as u8, Vec::new()),
            -128..=127 => plain(0x10, vec![value as i8 as u8]),
            _ => plain(0x11, (value as i16).to_be_bytes().to_vec()),
        }
    }
    let mut out = Vec::with_capacity(instructions.len());
    for instruction in instructions {
        match instruction {
            TypeOfInsn::LdcClass(class) => out.push(ldc(cw.class_ref(class))),
            TypeOfInsn::PrimitiveClass(wrapper) => out.push(pooled(
                0xb2,
                cw.fieldref(wrapper, "TYPE", "Ljava/lang/Class;"),
            )),
            TypeOfInsn::LdcString(value) => out.push(ldc(cw.const_string(value))),
            TypeOfInsn::PushInt(value) => out.push(push_int(*value)),
            TypeOfInsn::AconstNull => out.push(plain(0x01, Vec::new())),
            TypeOfInsn::Dup => out.push(plain(0x59, Vec::new())),
            TypeOfInsn::New(class) => out.push(pooled(0xbb, cw.class_ref(class))),
            TypeOfInsn::ANewArray(class) => out.push(pooled(0xbd, cw.class_ref(class))),
            TypeOfInsn::AAStore => out.push(plain(0x53, Vec::new())),
            TypeOfInsn::GetStatic {
                owner,
                name,
                descriptor,
            } => out.push(pooled(0xb2, cw.fieldref(owner, name, descriptor))),
            TypeOfInsn::InvokeStatic {
                owner,
                name,
                descriptor,
            } => out.push(pooled(0xb8, cw.methodref(owner, name, descriptor))),
            TypeOfInsn::InvokeVirtual {
                owner,
                name,
                descriptor,
            } => out.push(pooled(0xb6, cw.methodref(owner, name, descriptor))),
            TypeOfInsn::InvokeSpecial {
                owner,
                name,
                descriptor,
            } => out.push(pooled(0xb7, cw.methodref(owner, name, descriptor))),
            TypeOfInsn::ReifiedMarker(argument) => {
                out.push(push_int(TYPE_OF_MARKER));
                out.push(ldc(cw.const_string(argument)));
                out.push(pooled(
                    0xb8,
                    cw.methodref(
                        "kotlin/jvm/internal/Intrinsics",
                        "reifiedOperationMarker",
                        "(ILjava/lang/String;)V",
                    ),
                ));
            }
        }
    }
    out
}

/// kotlinc's `typeReferencesParameterWithRecursiveBound`, over declared bounds.
fn references_recursive_bound(
    parameters: &TypeParameters<'_>,
    identity: &str,
    used: &mut Vec<String>,
) -> Result<bool, TypeOfError> {
    if used.iter().any(|seen| seen == identity) {
        return Ok(true);
    }
    let Ok((parameter, _)) = parameters.declaration(identity) else {
        return Ok(false);
    };
    used.push(identity.to_owned());
    for bound in TypeParameters::bounds(parameter) {
        if type_references_recursive_bound(parameters, bound, used)? {
            return Ok(true);
        }
    }
    used.pop();
    Ok(false)
}

fn type_references_recursive_bound(
    parameters: &TypeParameters<'_>,
    ty: Ty,
    used: &mut Vec<String>,
) -> Result<bool, TypeOfError> {
    match ty.non_null() {
        Ty::TyParam(identity, _) => references_recursive_bound(parameters, identity, used),
        Ty::PlatformNullable(inner) | Ty::InProjection(inner) | Ty::OutProjection(inner) => {
            type_references_recursive_bound(parameters, *inner, used)
        }
        Ty::StarProjection(_) => Ok(false),
        other => {
            for argument in classifier_arguments(other) {
                if type_references_recursive_bound(parameters, argument, used)? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn realize(ty: Ty) -> Vec<TypeOfInsn> {
        let ir = IrFile::default();
        let parameters = TypeParameters::new(&ir, "AKt");
        let mut out = Vec::new();
        generate(ty, &parameters, &mut out).expect("describable");
        out
    }

    fn type_of(descriptor: &str) -> TypeOfInsn {
        TypeOfInsn::InvokeStatic {
            owner: REFLECTION,
            name: "typeOf",
            descriptor: descriptor.to_owned(),
        }
    }

    #[test]
    fn a_non_null_primitive_is_described_by_its_primitive_class() {
        assert_eq!(
            realize(Ty::Int),
            vec![
                TypeOfInsn::PrimitiveClass("java/lang/Integer"),
                type_of("(Ljava/lang/Class;)Lkotlin/reflect/KType;"),
            ]
        );
    }

    #[test]
    fn a_nullable_primitive_is_described_by_its_wrapper() {
        assert_eq!(
            realize(Ty::nullable(Ty::Int)),
            vec![
                TypeOfInsn::LdcClass("java/lang/Integer".to_owned()),
                TypeOfInsn::InvokeStatic {
                    owner: REFLECTION,
                    name: "nullableTypeOf",
                    descriptor: "(Ljava/lang/Class;)Lkotlin/reflect/KType;".to_owned(),
                },
            ]
        );
    }

    #[test]
    fn a_mutable_collection_is_marked_after_its_read_only_class() {
        let out = realize(Ty::obj_args(
            "kotlin/collections/MutableList",
            &[Ty::String],
        ));
        assert_eq!(out[0], TypeOfInsn::LdcClass("java/util/List".to_owned()));
        assert_eq!(
            out.last(),
            Some(&TypeOfInsn::InvokeStatic {
                owner: REFLECTION,
                name: "mutableCollectionType",
                descriptor: "(Lkotlin/reflect/KType;)Lkotlin/reflect/KType;".to_owned(),
            })
        );
    }

    #[test]
    fn three_arguments_are_passed_as_one_array() {
        let out = realize(Ty::fun(vec![Ty::Int, Ty::Int], Ty::Int));
        assert_eq!(
            out[0],
            TypeOfInsn::LdcClass("kotlin/jvm/functions/Function2".to_owned())
        );
        assert_eq!(out[1], TypeOfInsn::PushInt(3));
        assert_eq!(out[2], TypeOfInsn::ANewArray(K_TYPE_PROJECTION));
        assert_eq!(
            out.last(),
            Some(&type_of(
                "(Ljava/lang/Class;[Lkotlin/reflect/KTypeProjection;)Lkotlin/reflect/KType;"
            ))
        );
        assert!(!out
            .iter()
            .any(|instruction| matches!(instruction, TypeOfInsn::ReifiedMarker(_))));
    }

    #[test]
    fn an_unknown_type_parameter_is_refused() {
        let ir = IrFile::default();
        let parameters = TypeParameters::new(&ir, "AKt");
        let mut out = Vec::new();
        assert_eq!(
            generate(
                Ty::ty_param("T@nowhere", Ty::obj("kotlin/Any")),
                &parameters,
                &mut out
            ),
            Err(TypeOfError::UnknownTypeParameter("T@nowhere".to_owned()))
        );
    }
}
