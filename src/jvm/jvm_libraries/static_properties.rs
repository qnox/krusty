//! Static properties a classifier or package publishes from compiled classes.
//!
//! A Java static field, a `companion object` property hoisted onto its outer class, a top-level
//! `const val` on a package facade, and a `companion { … }` block property all read as a property
//! with no receiver. This module finds the classfile realization of each, joins it with the
//! declaration's Kotlin metadata when there is one, and normalizes it into the common
//! [`PropertyInfo`] with receiver-less accessors. The provider facade only asks for the property.

use super::*;

impl JvmLibraries {
    /// A `companion { … }` block property of `internal`, declared by its class `@Metadata`: a static
    /// member of the class read and written with no receiver. It is normalized like a top-level
    /// property with the class in the facade's place: its public static accessors take only its
    /// context parameters, and a `const val` is the public static final field its
    /// `JvmPropertySignature` names, whose `ConstantValue` is the declaration's constant.
    pub(super) fn companion_block_property(
        &self,
        internal: TypeName,
        name: &str,
    ) -> Option<PropertyInfo> {
        let class = self.cp.find_name(internal)?;
        let declaration = super::metadata::class_properties(&class)
            .iter()
            .find(|property| property.is_companion_block_member && property.name == name)?;
        let accessor = |jvm_name: &str, descriptor: &str| {
            class
                .methods
                .iter()
                .find(|method| {
                    method.is_static() && method.name == jvm_name && method.descriptor == descriptor
                })
                .map(|method| StaticAccessor {
                    owner: internal,
                    public: method.is_public(),
                })
        };
        let mut properties = self
            .static_metadata_property(declaration, internal, None, accessor)
            .into_iter()
            .collect::<Vec<_>>();
        if let Some(field) = self.declared_static_field(&class, internal, declaration) {
            super::super::top_level_properties::merge_metadata_const(
                name,
                declaration,
                &field,
                &mut properties,
            );
        }
        let mut property = properties.pop()?;
        self.register_external_property(&mut property);
        Some(property)
    }

    /// The public static final field of `class` that `declaration`'s `JvmPropertySignature` names,
    /// typed by the declaration and carrying the field's `ConstantValue`.
    fn declared_static_field(
        &self,
        class: &crate::jvm::classreader::ClassInfo,
        internal: TypeName,
        declaration: &super::metadata::MetaProp,
    ) -> Option<JvmStaticField> {
        let signature = declaration.field.as_ref()?;
        let field = class.fields.iter().find(|field| {
            field.name == signature.name
                && field.descriptor == signature.desc
                && field.access & 0x0019 == 0x0019 // PUBLIC | STATIC | FINAL
        })?;
        let ty = declared_property_ty(declaration, declared_desc_to_ty(&field.descriptor));
        let constant = field.const_value.as_ref().map(|value| LibraryConst {
            ty,
            value: Self::library_const(value),
        });
        let mut field = JvmStaticField {
            external_identity: None,
            owner: internal,
            name: field.name.clone(),
            descriptor: field.descriptor.clone(),
            ty,
            constant,
            visibility: declaration.visibility,
            is_final: true,
        };
        self.register_external_static_field(&mut field);
        Some(field)
    }

    pub(super) fn top_level_static_field(
        &self,
        package: TypeName,
        name: &str,
    ) -> Option<JvmStaticField> {
        // A top-level `const val` is a `public static final` field on the package facade that carries
        // it. This classfile scan stays entirely inside the JVM provider.
        self.cp
            .package_facades_name(package)
            .into_iter()
            .find_map(|facade| self.static_field_name(facade, name))
    }

    pub(super) fn static_field_name(
        &self,
        internal: TypeName,
        name: &str,
    ) -> Option<JvmStaticField> {
        let mut stack = vec![internal];
        let mut seen = std::collections::HashSet::new();
        while let Some(cur) = stack.pop() {
            if !seen.insert(cur) {
                continue;
            }
            let Some(ci) = self.cp.find_name(cur) else {
                continue;
            };
            if let Some(f) = ci.fields.iter().find(|f| {
                f.name == name
                    && f.access & 0x0008 != 0
                    // Publish every non-private associated declaration. Kotlin visibility is
                    // enforced by the resolver at the lexical use site; dropping `protected`
                    // here made a valid subclass read indistinguishable from a missing property.
                    && f.access & 0x0002 == 0
            }) {
                let ty = self
                    .metadata_property_ty(cur, name)
                    .or_else(|| {
                        f.signature
                            .as_deref()
                            .and_then(|signature| {
                                parse_concrete_field_gsig(signature, &f.descriptor)
                            })
                            .map(|ty| self.semanticize_jvm_type(ty))
                    })
                    .unwrap_or_else(|| declared_desc_to_ty(&f.descriptor));
                let ty = if ci.meta.is_present() {
                    ty
                } else {
                    java_type_nullability(ty, f.nullability)
                };
                let constant = f.const_value.as_ref().map(|value| LibraryConst {
                    ty,
                    value: Self::library_const(value),
                });
                let mut field = JvmStaticField {
                    external_identity: None,
                    owner: cur,
                    name: name.to_string(),
                    descriptor: f.descriptor.clone(),
                    ty,
                    constant,
                    visibility: if f.access & 0x0001 != 0 {
                        Visibility::Public
                    } else if f.access & 0x0004 != 0 {
                        Visibility::Protected
                    } else {
                        Visibility::PackagePrivate
                    },
                    is_final: f.access & 0x0010 != 0,
                };
                self.register_external_static_field(&mut field);
                return Some(field);
            }
            if let Some(superclass) = ci.super_class {
                stack.push(superclass);
            }
            stack.extend(ci.interfaces.iter_ids());
        }
        None
    }

    /// Physical storage for a companion-declared `@JvmField` property lives on the outer class.
    /// Keep that placement inside the JVM provider: the semantic owner remains the companion and
    /// callers receive an ordinary associated property with an opaque external accessor identity.
    pub(super) fn classifier_static_field_name(
        &self,
        internal: TypeName,
        name: &str,
    ) -> Option<JvmStaticField> {
        self.static_field_name(internal, name).or_else(|| {
            let outer = internal.nested_owner()?;
            let outer_class = self.cp.find_name(outer)?;
            let is_companion =
                super::metadata::class_companion_name(&outer_class).and_then(|companion| {
                    crate::types::existing_type_name_nested_child(outer, &companion)
                }) == Some(internal);
            if !is_companion {
                return None;
            }
            let companion = self.cp.find_name(internal)?;
            super::metadata::class_properties(&companion)
                .iter()
                .any(|property| property.name == name)
                .then(|| self.static_field_name(outer, name))
                .flatten()
        })
    }

    fn register_external_static_field(&self, field: &mut JvmStaticField) {
        let descriptor = field.descriptor.clone();
        let mut declaration = LibraryCallable::library(
            field.owner,
            field.name.clone(),
            Vec::new(),
            field.ty,
            field.ty.platform_lower_bound(),
            descriptor,
        );
        declaration.external_identity = Some(self.cp.intern_external_callable(
            &declaration,
            crate::libraries::ExternalCallableKind::StaticFieldRead,
        ));
        field.external_identity = declaration.external_identity;
    }

    pub(super) fn associated_property_for_static_field(
        &self,
        field: JvmStaticField,
    ) -> Option<PropertyInfo> {
        let (getter, setter) = self.static_field_accessors(&field);
        let mut property = PropertyInfo {
            return_value_status: None,
            name: field.name,
            kind: PropKind::TopLevel,
            receiver: None,
            formals: Vec::new(),
            ty: field.ty,
            context_count: 0,
            context_param_names: Vec::new(),
            context_parameter_identities: Vec::new(),
            getter,
            setter,
            setter_visibility: field.visibility,
            setter_parameter_name: None,
            is_const: field.constant.is_some() && field.is_final,
            implicit_integer_coercion: false,
            compile_time_constant: field.constant,
            visibility: field.visibility,
            owner: field.owner,
            receiver_rank: 0,
            source_key: None,
            stable_declaration: None,
            getter_declaration: None,
            setter_declaration: None,
            source_member: None,
            accessor_derived: false,
            read_stability: crate::libraries::PropertyReadStability::Unstable,
        };
        self.register_external_property(&mut property);
        Some(property)
    }

    /// The field read, and for a non-final field the field write, that realize a static field as a
    /// property's accessors.
    fn static_field_accessors(
        &self,
        field: &JvmStaticField,
    ) -> (LibraryCallable, Option<LibraryCallable>) {
        let descriptor = field.descriptor.clone();
        let mut getter = LibraryCallable::library(
            field.owner,
            field.name.clone(),
            Vec::new(),
            field.ty,
            field.ty.platform_lower_bound(),
            descriptor.clone(),
        );
        getter.external_identity = field.external_identity;
        let setter = (!field.is_final).then(|| {
            let mut setter = LibraryCallable::library(
                field.owner,
                field.name.clone(),
                vec![field.ty.platform_lower_bound()],
                Ty::Unit,
                Ty::Unit,
                descriptor,
            );
            setter.params = vec![field.ty];
            setter.external_identity = Some(self.cp.intern_external_callable(
                &setter,
                crate::libraries::ExternalCallableKind::StaticFieldWrite,
            ));
            setter
        });
        (getter, setter)
    }
}

/// A metadata property's declared Kotlin type: its generic signature's return, else its return
/// class with metadata nullability, else the `physical` JVM type when metadata names no class.
fn declared_property_ty(declaration: &super::metadata::MetaProp, physical: Ty) -> Ty {
    declaration.generic_sig.as_ref().map_or_else(
        || {
            let ty = declaration
                .ret_class
                .map_or(physical, kotlin_type_name_to_ty);
            if declaration.ret_nullable {
                Ty::nullable(ty)
            } else {
                ty
            }
        },
        |signature| signature.ret,
    )
}

/// A static accessor that realizes a metadata property: the class holding it, and whether it is
/// public.
pub(super) struct StaticAccessor {
    pub(super) owner: TypeName,
    pub(super) public: bool,
}

impl JvmLibraries {
    /// Normalize a receiver-less or extension metadata property that `owner` publishes through static
    /// accessors (a package facade's top-level property, or a class's `companion { … }` block
    /// property) into a [`PropertyInfo`]. `accessor` finds the static method realizing a JVM
    /// signature; `package` names the package of a top-level property, which may have a compiler
    /// realization. Context parameters come first in every accessor, then the extension receiver.
    pub(super) fn static_metadata_property(
        &self,
        mp: &super::metadata::MetaProp,
        owner: TypeName,
        package: Option<TypeName>,
        accessor: impl Fn(&str, &str) -> Option<StaticAccessor>,
    ) -> Option<PropertyInfo> {
        // Accessors carry context parameters first, then the extension receiver when one
        // exists. These are declaration roles from metadata; the descriptor only verifies
        // that the selected physical accessor realizes the same arity.
        let context_count = mp.context_params.len();
        let receiver_params = usize::from(mp.is_extension);
        let property_gsig = mp.generic_sig.clone();
        let name = mp.name.as_str();
        let context_parameter_identities = mp.context_parameter_identities();
        let Some(getter_sig) = mp.getter.clone() else {
            crate::trace_compiler!(
                "metadata_properties",
                "property {}.{name} rejected: metadata has no getter realization",
                owner.render(),
            );
            return None;
        };
        let Some(getter_method) = accessor(&getter_sig.name, &getter_sig.desc) else {
            crate::trace_compiler!(
                "metadata_properties",
                "property {}.{name} rejected: getter {}{} is absent from {}",
                owner.render(),
                getter_sig.name,
                getter_sig.desc,
                owner.render()
            );
            return None;
        };
        let Some((gparams, gret)) = parse_method_desc(&getter_sig.desc) else {
            crate::trace_compiler!(
                "metadata_properties",
                "property {}.{name} rejected: malformed getter descriptor {}",
                owner.render(),
                getter_sig.desc
            );
            return None;
        };
        if gparams.len() != context_count + receiver_params {
            crate::trace_compiler!(
                "metadata_properties",
                "property {}.{name} rejected: getter parameter count {} != metadata context/receiver count {}",
                owner.render(),
                gparams.len(),
                context_count + receiver_params,
            );
            return None;
        }
        let generic_receiver = property_gsig.as_ref().and_then(|gsig| gsig.receiver);
        let receiver = mp.is_extension.then(|| {
            generic_receiver.unwrap_or_else(|| {
                mp.receiver_class
                    .map_or(Ty::obj("kotlin/Any"), Ty::obj_name)
            })
        });
        let semantic_context = property_gsig
            .as_ref()
            .map(|signature| signature.params.clone())
            .unwrap_or_else(|| gparams[..context_count].to_vec());
        let fallback_ret = mp.ret_class.map_or(gret, kotlin_type_name_to_ty);
        let property_ty = property_gsig.as_ref().map_or_else(
            || {
                if mp.ret_nullable {
                    Ty::nullable(fallback_ret)
                } else {
                    fallback_ret
                }
            },
            |gsig| gsig.ret,
        );
        let property_kind = if mp.is_extension {
            PropKind::Extension
        } else {
            PropKind::TopLevel
        };
        let property_intrinsic = match package {
            Some(package) => crate::libraries::builtin_top_level_realization::property_realization(
                crate::libraries::builtin_declaration::BuiltinPropertyDeclaration {
                    package,
                    name,
                    kind: property_kind,
                    receiver,
                    ty: property_ty,
                    context_count,
                    type_parameter_count: property_gsig
                        .as_ref()
                        .map_or(0, |signature| signature.formals.len()),
                    mutable: mp.setter.is_some(),
                },
            ),
            None => None,
        };
        // An exact compiler intrinsic may deliberately have no callable public accessor.
        // `coroutineContext` is a public `@InlineOnly` suspend property whose private JVM
        // getter throws; the provider publishes its semantic declaration and marks the
        // compiler realization instead of exposing that physical method as a fallback.
        if !getter_method.public && property_intrinsic.is_none() {
            return None;
        }
        let mut getter = LibraryCallable::library(
            getter_method.owner,
            getter_sig.name,
            gparams,
            property_ty,
            gret,
            getter_sig.desc,
        );
        getter.params = semantic_context.iter().copied().chain(receiver).collect();
        getter.source_receiver = receiver;
        getter.context_count = context_count;
        getter.generic_sig = property_gsig.clone().map(Box::new);
        getter.compiler_intrinsic = property_intrinsic;
        let setter = mp.setter.clone().and_then(|setter_sig| {
            let (sparams, sret) = parse_method_desc(&setter_sig.desc)?;
            if sparams.len() != context_count + receiver_params + 1 || sret != Ty::Unit {
                return None;
            }
            let setter_method = accessor(&setter_sig.name, &setter_sig.desc)?;
            if !setter_method.public {
                return None;
            }
            let mut setter = LibraryCallable::library(
                setter_method.owner,
                setter_sig.name,
                sparams,
                Ty::Unit,
                sret,
                setter_sig.desc,
            );
            setter.params = semantic_context
                .iter()
                .copied()
                .chain(receiver)
                .chain(std::iter::once(property_ty))
                .collect();
            setter.source_receiver = receiver;
            setter.context_count = context_count;
            Some(setter)
        });
        Some(PropertyInfo {
            return_value_status: Some(mp.return_value_status),
            name: name.to_string(),
            kind: property_kind,
            receiver,
            formals: property_gsig
                .as_ref()
                .map(|gsig| gsig.formals.clone())
                .unwrap_or_default(),
            ty: property_ty,
            context_count,
            context_param_names: mp
                .context_params
                .iter()
                .map(|parameter| parameter.name.clone())
                .collect(),
            context_parameter_identities,
            getter,
            setter,
            setter_visibility: mp.visibility,
            setter_parameter_name: mp.setter_parameter_name.clone(),
            is_const: mp.is_const,
            implicit_integer_coercion: false,
            compile_time_constant: None,
            visibility: mp.visibility,
            owner,
            receiver_rank: 0,
            source_key: None,
            stable_declaration: None,
            getter_declaration: None,
            setter_declaration: None,
            source_member: None,
            accessor_derived: false,
            read_stability: crate::libraries::PropertyReadStability::Unstable,
        })
    }
}
