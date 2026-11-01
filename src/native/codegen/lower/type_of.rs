//! `typeOf<T>()` — a `kotlin.reflect.KType`.
//!
//! The checked call carries the whole type argument, inline specialization has already replaced a
//! reified parameter by the call site's type, and the frontend has already approximated an
//! intersection to its common supertype. So every site names one fixed type, and the object is a
//! STATIC one: a header wearing the runtime's `kt_type_ktype_impl`, the classifier's descriptor and
//! its static `KClass`, the nullability, and one projection per type argument, each pointing at the
//! argument's own static object. The runtime reads that layout to answer `equals`, `hashCode`,
//! `toString`, `classifier` and `isMarkedNullable`; nothing is allocated at the site, and
//! `classifier` hands out the one `KClass` instead of a new one per read, as the JVM's
//! `Reflection` hands out the class's cached `KClass`.
//!
//! A type every file of the program names alike (its classifier and arguments all have program-wide
//! descriptors) is one object for the whole program: each file emits it under a name spelled from
//! the type, in a COMDAT group of its own, and the linker keeps the first group and drops the rest.
//! So is the static `KClass` of a program-wide descriptor. A type naming something only its file
//! can name, such as a local class, is that file's own object.
//!
//! A type naming a type parameter is declined rather than described: its classifier is a
//! `KTypeParameter`, which this target does not build. So is a type whose classifier has no
//! runtime descriptor, and `KType.arguments`, whose `KTypeProjection` objects it does not build
//! either.

use super::*;
use crate::types::TyKind;
use crate::types::TypeName;

/// Byte offsets of the static object, matching `KTypeOfObject` in `krusty_rt.c`.
const CLASSIFIER: u32 = model::HEADER_SIZE;
const CLASSIFIER_CLASS: u32 = CLASSIFIER + 8;
const NULLABLE: u32 = CLASSIFIER_CLASS + 8;
const ARGUMENT_COUNT: u32 = NULLABLE + 8;
const ARGUMENTS: u32 = ARGUMENT_COUNT + 8;
/// One `KTypeOfArgument`: the variance, then the argument's object.
const ARGUMENT_SIZE: u32 = 16;
/// The static `KClass`: its header, then the descriptor it stands for (`KClass` in `krusty_rt.c`).
const DESCRIBED: u32 = model::HEADER_SIZE;
const CLASS_SIZE: u32 = DESCRIBED + 8;

/// Kotlin/Native's variance encoding (`KVarianceMapper`), which the runtime renders from.
const VARIANCE_STAR: i64 = -1;
const VARIANCE_INVARIANT: i64 = 0;
const VARIANCE_IN: i64 = 1;
const VARIANCE_OUT: i64 = 2;

/// The `KType` members this target answers, read off the static object by the runtime.
#[derive(Clone, Copy)]
pub(super) enum KTypeMember {
    Classifier,
    IsMarkedNullable,
    /// `arguments`, which would need `KTypeProjection` objects this target does not build.
    Arguments,
}

impl KTypeMember {
    pub(super) fn of(owner: TypeName, name: &str) -> Option<Self> {
        if !owner.matches("kotlin/reflect/KType") {
            return None;
        }
        match name {
            "classifier" => Some(Self::Classifier),
            "isMarkedNullable" => Some(Self::IsMarkedNullable),
            "arguments" => Some(Self::Arguments),
            _ => None,
        }
    }
}

impl FileLowering<'_> {
    /// The static `KType` object describing `ty`, emitted on first use.
    pub(super) fn type_of_object(&mut self, ty: Ty) -> Result<DataId, Unsupported> {
        if let Some(object) = self.type_of_objects.get(&ty) {
            return Ok(*object);
        }
        let (inner, nullable) = match ty.kind() {
            TyKind::Nullable(inner) => (inner, true),
            _ => (ty, false),
        };
        let (classifier, arguments) = match inner.kind() {
            TyKind::Fun(signature) => {
                if signature.suspend {
                    return Err(declined!("`typeOf` of the suspend function type `{ty:?}`"));
                }
                let descriptor = u8::try_from(signature.params.len())
                    .ok()
                    .and_then(|arity| {
                        super::classifier_shapes::function_check_descriptor(
                            crate::ir::TypeCheckRole::FunctionOfArity(arity),
                        )
                    })
                    .ok_or_else(|| declined!("`typeOf` of the function type `{ty:?}`"))?;
                let arguments = signature
                    .params
                    .iter()
                    .chain(std::iter::once(&signature.ret))
                    .map(|argument| {
                        (
                            VARIANCE_INVARIANT,
                            Some(crate::types::stored_value_ty(*argument)),
                        )
                    })
                    .collect::<Vec<_>>();
                (self.import_data(descriptor)?, arguments)
            }
            TyKind::Obj(name, arguments) => {
                let arguments = arguments
                    .iter()
                    .map(|argument| match argument.kind() {
                        TyKind::StarProjection(_) => (VARIANCE_STAR, None),
                        TyKind::InProjection(argument) => (VARIANCE_IN, Some(argument)),
                        TyKind::OutProjection(argument) => (VARIANCE_OUT, Some(argument)),
                        _ => (VARIANCE_INVARIANT, Some(*argument)),
                    })
                    .collect::<Vec<_>>();
                (self.type_of_classifier(name, inner)?, arguments)
            }
            // The built-in scalars and `String` are `Obj` kinds and take the arm above; `Unit` is the
            // one built-in spelled apart from its class.
            TyKind::Unit => (
                self.type_descriptor(inner)?
                    .ok_or_else(|| declined!("`typeOf` of `{ty:?}`"))?,
                Vec::new(),
            ),
            TyKind::TyParam(..) | TyKind::DefinitelyNotNull(_) => {
                return Err(declined!(
                    "`typeOf` of the type parameter `{ty:?}`, whose classifier is a \
                     `KTypeParameter`"
                ))
            }
            _ => return Err(declined!("`typeOf` of `{ty:?}`")),
        };
        let mut objects = Vec::with_capacity(arguments.len());
        for (variance, argument) in arguments {
            let object = argument
                .map(|argument| self.type_of_object(argument))
                .transpose()?;
            objects.push((variance, object));
        }
        let classifier_class = self.static_class(classifier)?;

        // The program-wide name: the classifier's symbol, `?`, then each projection's variance and
        // object. Two files spell one type alike, and two types never alike.
        let shared = self.program_wide_name(classifier).and_then(|classifier| {
            let mut name = format!("kt_typeof.{classifier}");
            if nullable {
                name.push('?');
            }
            if !objects.is_empty() {
                name.push('<');
                for (index, (variance, argument)) in objects.iter().enumerate() {
                    if index > 0 {
                        name.push(',');
                    }
                    match argument {
                        None => name.push('*'),
                        Some(argument) => {
                            let argument = self.program_wide_name(*argument)?;
                            name.push_str(&format!("{variance}:{argument}"));
                        }
                    }
                }
                name.push('>');
            }
            Some(name)
        });
        let local = format!("kt_typeof_{}", self.type_of_objects.len());
        let object = self.declare_reflection_object(shared.as_deref(), &local)?;
        let size = ARGUMENTS + ARGUMENT_SIZE * objects.len() as u32;
        let mut bytes = vec![0u8; size as usize];
        let mut write = |offset: u32, value: i64| {
            bytes[offset as usize..offset as usize + 8].copy_from_slice(&value.to_le_bytes());
        };
        write(NULLABLE, i64::from(nullable));
        write(ARGUMENT_COUNT, objects.len() as i64);
        for (index, (variance, _)) in objects.iter().enumerate() {
            write(ARGUMENTS + ARGUMENT_SIZE * index as u32, *variance);
        }
        let header = self.import_data("kt_type_ktype_impl")?;
        let mut addresses = vec![
            (0, header),
            (CLASSIFIER, classifier),
            (CLASSIFIER_CLASS, classifier_class),
        ];
        for (index, (_, argument)) in objects.iter().enumerate() {
            if let Some(argument) = argument {
                addresses.push((ARGUMENTS + ARGUMENT_SIZE * index as u32 + 8, *argument));
            }
        }
        self.define_reflection_object(object, shared.as_deref(), bytes, &addresses)?;
        self.type_of_objects.insert(ty, object);
        Ok(object)
    }

    /// The static `KClass` standing for `descriptor`, emitted on first use.
    fn static_class(&mut self, descriptor: DataId) -> Result<DataId, Unsupported> {
        if let Some(class) = self.static_classes.get(&descriptor) {
            return Ok(*class);
        }
        let shared = self
            .program_wide_name(descriptor)
            .map(|descriptor| format!("kt_kclass.{descriptor}"));
        let local = format!("kt_kclass_{}", self.static_classes.len());
        let class = self.declare_reflection_object(shared.as_deref(), &local)?;
        let header = self.import_data("kt_type_kclass")?;
        self.define_reflection_object(
            class,
            shared.as_deref(),
            vec![0u8; CLASS_SIZE as usize],
            &[(0, header), (DESCRIBED, descriptor)],
        )?;
        self.static_classes.insert(descriptor, class);
        Ok(class)
    }

    /// The symbol every file of the program names `data` by, or `None` when it is this file's own.
    fn program_wide_name(&self, data: DataId) -> Option<String> {
        let declaration = self.module.declarations().get_data_decl(data);
        if declaration.linkage == Linkage::Local {
            return None;
        }
        declaration.name.clone()
    }

    /// Declare a static reflection object: under its program-wide name as a weak definition the
    /// linker folds, or as this file's own.
    pub(super) fn declare_reflection_object(
        &mut self,
        shared: Option<&str>,
        local: &str,
    ) -> Result<DataId, Unsupported> {
        let Some(shared) = shared else {
            return self.declare_local_data(local, false);
        };
        self.module
            .declare_data(shared, Linkage::Preemptible, false, false)
            .map_err(|error| declined!("declaring `{shared}` ({error})"))
    }

    /// Define a static reflection object from its bytes and the addresses written into them. A
    /// program-wide one gets a section of its own, which [`lower_file`] makes its COMDAT group.
    pub(super) fn define_reflection_object(
        &mut self,
        object: DataId,
        shared: Option<&str>,
        bytes: Vec<u8>,
        addresses: &[(u32, DataId)],
    ) -> Result<(), Unsupported> {
        let mut description = DataDescription::new();
        description.define(bytes.into_boxed_slice());
        description.set_align(8);
        if let Some(shared) = shared {
            description.set_custom_section(&format!(".data.rel.ro.{shared}"));
            self.comdat_objects.push(object);
        }
        for (offset, data) in addresses {
            let global = self.module.declare_data_in_data(*data, &mut description);
            description.write_data_addr(*offset, global, 0);
        }
        self.module
            .define_data(object, &description)
            .map_err(|error| format!("defining a static reflection object ({error})"))?;
        Ok(())
    }

    /// The descriptor of the class a `typeOf` type names. A mapped collection interface is the
    /// runtime's marker for it whatever this file implements, since the question is which class
    /// the type names and not which objects a check would accept. Only a marker that is that
    /// exact interface will do: `MutableIterator` and `ListIterator` have none of their own (an
    /// `is` check of either tests the `Iterator` marker), and naming `Iterator` for them would make
    /// two types of different classes equal and print alike.
    fn type_of_classifier(&mut self, name: TypeName, ty: Ty) -> Result<DataId, Unsupported> {
        let marker = match self.classifiers.classifier(name).and_then(|fact| fact.role) {
            Some(crate::types::ClassifierRole::MappedCollection(collection)) => Some(
                super::classifier_shapes::exact_collection_descriptor(collection).ok_or_else(
                    || declined!("`typeOf` of `{ty:?}`, whose class has no runtime descriptor"),
                )?,
            ),
            _ if name == crate::types::wk::any() => Some("kt_type_any"),
            _ => None,
        };
        if let Some(marker) = marker {
            return self.import_data(marker);
        }
        self.type_descriptor(ty)?
            .ok_or_else(|| declined!("`typeOf` of `{ty:?}`, whose class has no descriptor"))
    }
}

impl BodyLowering<'_, '_, '_> {
    /// `typeOf<T>()`: the address of the static object describing the checked type argument.
    pub(super) fn type_of(
        &mut self,
        target: crate::fir::ExternalCallableId,
        substitutions: &[crate::ir::IrCheckedSubstitution],
    ) -> Result<Option<Value>, Unsupported> {
        let argument = substitutions
            .iter()
            .find(|substitution| {
                matches!(
                    substitution.parameter,
                    crate::fir::FirTypeParameterRef::External { callable, ordinal: 0 }
                        if callable == target
                )
            })
            .ok_or("a `typeOf` call without its checked type argument")?;
        let object = self.file.type_of_object(argument.value)?;
        Ok(Some(self.data_address(object)))
    }

    /// `t.classifier` and `t.isMarkedNullable`. `KType` is an open interface, so the receiver may
    /// be a program's own implementation. When every class of the module that can be one is
    /// declared in this file, each is dispatched to through its override, and only what remains
    /// reaches the runtime, which reads the static object after checking the receiver is one and
    /// fails loudly on anything else. A class of another file has no override this file can name,
    /// so a read in a module whose other files implement `KType` declines.
    pub(super) fn type_of_member(
        &mut self,
        member: KTypeMember,
        target: crate::fir::ExternalPropertyId,
        receiver: u32,
    ) -> Result<Option<Value>, Unsupported> {
        let (symbol, ret) = match member {
            KTypeMember::Classifier => ("kt_ktype_classifier", super::classes_literal::kclass()),
            KTypeMember::IsMarkedNullable => ("kt_ktype_is_marked_nullable", Ty::Boolean),
            KTypeMember::Arguments => {
                return Err(declined!(
                    "`KType.arguments`, whose `KTypeProjection` objects this target does not build"
                ))
            }
        };
        let ktype = crate::types::type_name("kotlin/reflect/KType");
        let module_implementors = self.file.module_facts.dependency_implementors(ktype);
        if module_implementors
            .iter()
            .any(|&file| file != self.file.source)
        {
            return Err(declined!(
                "a `KType` member read where another file implements `KType`"
            ));
        }
        let mut implementors = Vec::with_capacity(module_implementors.len());
        if !module_implementors.is_empty() {
            let getter = self
                .file
                .callables
                .property(target)
                .ok_or("a `KType` member without its property")?
                .getter;
            let declared: Vec<_> = self
                .file
                .implementors_of(ktype)
                .into_iter()
                .filter(|&class| !self.file.ir.classes[class as usize].is_abstract)
                .collect();
            implementors.extend(
                declared
                    .iter()
                    .filter_map(|&class| self.external_property_getter_slot(class, getter, ret)),
            );
            // Every instantiable implementor of the module is a class of this file, and each must
            // be an arm: one reached only through a supertype has no override this names.
            if implementors.len() != module_implementors.len() {
                return Err(declined!(
                    "a `KType` member read where this file implements `KType` with a class whose \
                     override cannot be dispatched to"
                ));
            }
        }
        let object = self.reference(receiver)?;
        if self.terminated {
            return Ok(None);
        }
        self.dispatch_by_implementor_with(&implementors, object, &[], ret, |body, object| {
            body.runtime_call(symbol, &[any()], ret, &[object])
        })
    }

    /// The `KType` member a dependency property read names, if it is one.
    pub(super) fn type_of_member_of(
        &self,
        target: crate::fir::ExternalPropertyId,
    ) -> Option<KTypeMember> {
        let property = self.file.callables.property(target)?;
        let getter = self.file.callables.callable(property.getter)?;
        KTypeMember::of(getter.physical_owner, &property.name)
    }
}
