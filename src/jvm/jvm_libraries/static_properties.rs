//! Static properties a classifier or package publishes from compiled classes.
//!
//! A Java static field, a `companion object` property hoisted onto its outer class, a top-level
//! `const val` on a package facade, and a `companion { … }` block property all read as a property
//! with no receiver. This module finds the classfile realization of each, joins it with the
//! declaration's Kotlin metadata when there is one, and normalizes it into the common
//! [`PropertyInfo`] with receiver-less accessors. The provider facade only asks for the property.

use super::*;

impl JvmLibraries {
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

    /// A `companion { … }` block property of `internal`, declared by its class `@Metadata`: a static
    /// member of the class realized by public static accessors on the class itself (its backing
    /// field is private), read and written with no receiver.
    pub(super) fn companion_block_property(
        &self,
        internal: TypeName,
        name: &str,
    ) -> Option<PropertyInfo> {
        let class = self.cp.find_name(internal)?;
        let declaration = super::metadata::class_properties(&class)
            .iter()
            .find(|property| property.is_companion_block_member && property.name == name)?;
        if declaration.is_const {
            return self.companion_block_constant(internal, &class, declaration);
        }
        let static_accessor = |signature: &super::metadata::MetaJvmMethodSig| {
            class.methods.iter().any(|method| {
                method.is_static()
                    && method.is_public()
                    && method.name == signature.name
                    && method.descriptor == signature.desc
            })
        };
        let getter_signature = declaration
            .getter
            .clone()
            .filter(|signature| static_accessor(signature))?;
        let (getter_params, physical_ret) = parse_method_desc(&getter_signature.desc)?;
        if !getter_params.is_empty() {
            return None;
        }
        let ty = declared_property_ty(declaration, physical_ret);
        let getter = LibraryCallable::library(
            internal,
            getter_signature.name,
            Vec::new(),
            ty,
            physical_ret,
            getter_signature.desc,
        );
        let setter = declaration
            .setter
            .clone()
            .filter(|signature| static_accessor(signature))
            .and_then(|signature| {
                let (params, ret) = parse_method_desc(&signature.desc)?;
                if params.len() != 1 || ret != Ty::Unit {
                    return None;
                }
                let mut setter = LibraryCallable::library(
                    internal,
                    signature.name,
                    params,
                    Ty::Unit,
                    ret,
                    signature.desc,
                );
                setter.params = vec![ty];
                Some(setter)
            });
        let mut property = PropertyInfo {
            name: name.to_string(),
            kind: PropKind::TopLevel,
            receiver: None,
            formals: Vec::new(),
            ty,
            context_count: 0,
            context_param_names: Vec::new(),
            context_parameter_identities: Vec::new(),
            getter,
            setter,
            setter_visibility: declaration.visibility,
            setter_parameter_name: declaration.setter_parameter_name.clone(),
            is_const: declaration.is_const,
            implicit_integer_coercion: false,
            compile_time_constant: None,
            visibility: declaration.visibility,
            owner: internal,
            receiver_rank: 0,
            source_key: None,
            stable_declaration: None,
            getter_declaration: None,
            setter_declaration: None,
            source_member: None,
            accessor_derived: false,
            read_stability: crate::libraries::PropertyReadStability::Unstable,
            return_value_status: Some(declaration.return_value_status),
        };
        self.register_external_property(&mut property);
        Some(property)
    }

    /// A `companion { … }` block `const val` of `internal`: its metadata declaration joined with the
    /// exact public static field its `JvmPropertySignature` names, whose `ConstantValue` is the
    /// declaration's compile-time constant. It has no accessors; a read is the field itself.
    fn companion_block_constant(
        &self,
        internal: TypeName,
        class: &super::classreader::ClassInfo,
        declaration: &super::metadata::MetaProp,
    ) -> Option<PropertyInfo> {
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
        let (getter, _) = self.static_field_accessors(&field);
        let mut property = PropertyInfo {
            name: declaration.name.clone(),
            kind: PropKind::TopLevel,
            receiver: None,
            formals: Vec::new(),
            ty,
            context_count: 0,
            context_param_names: Vec::new(),
            context_parameter_identities: Vec::new(),
            getter,
            setter: None,
            setter_visibility: declaration.visibility,
            setter_parameter_name: None,
            is_const: true,
            implicit_integer_coercion: false,
            compile_time_constant: field.constant,
            visibility: declaration.visibility,
            owner: internal,
            receiver_rank: 0,
            source_key: None,
            stable_declaration: None,
            getter_declaration: None,
            setter_declaration: None,
            source_member: None,
            accessor_derived: false,
            read_stability: crate::libraries::PropertyReadStability::Unstable,
            return_value_status: Some(declaration.return_value_status),
        };
        self.register_external_property(&mut property);
        Some(property)
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
