//! A class's Kotlin declaration record, built from common IR.
//!
//! `@Metadata` and a KLIB fragment describe a class with the same `Class` record: its header, its
//! constructors, its members and the order kotlinc visits them in. [`build`] makes that record once,
//! from the checked declarations, with no target in it. Each entry keeps the declaration it
//! describes ([`PropertyOrigin`], [`FunctionOrigin`], a secondary constructor's ordinal), so a
//! target adds how it realizes that declaration (the JVM its method, field and constructor
//! signatures) beside the record instead of inside it.
//!
//! The record is built at the backend handoff ([`record_all`]), before any target lowering rewrites
//! a declaration, so it reads every declaration as source declared it.

use crate::ir::{
    IrClass, IrDataClassMemberRole, IrFile, IrProperty, IrSecondaryCtor, IrValueClassAnyMember,
};
use crate::metadata::builder::TypeAliasMeta;
use crate::metadata::class_builder::{
    CapturedTypeParameters, ClassDeclaration, ClassMemberOrder, ClassTail, CtorMeta, EnumEntryMeta,
    FnMeta, PropMeta, EQUALS_FN_FLAGS, FN_IS_SUSPEND, HASHCODE_TOSTRING_FN_FLAGS,
    OBJECT_CTOR_FLAGS, SEALED_CTOR_FLAGS,
};
use crate::metadata::declaration_flags::{
    class_flags, data_component_flags, data_copy_flags, declaration_visibility_bits, function_flags,
};
use crate::metadata::local_classifiers;
use crate::metadata::member_order::{self, MemberRanges};
use crate::metadata::MetadataAnnotations;
use crate::types::{Ty, TypeName, Visibility};

/// Which declaration a property record describes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PropertyOrigin {
    /// The class's declared property at this index.
    Declared(usize),
    /// A class `const val`, by its static storage.
    Constant(u32),
    /// The class's member extension property at this index (`IrFile::member_ext_props`).
    MemberExtension(usize),
    /// The class's `companion { … }` block property at this index.
    CompanionBlock(usize),
}

/// Which declaration a function record describes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FunctionOrigin {
    /// A declared member function, by its checked callable identity.
    Declared(crate::fir::CallableId),
    /// An `interface by` forwarder the class generates for its delegate.
    Delegation(u32),
    /// A data class's generated member.
    DataMember(IrDataClassMemberRole),
    /// A value class's generated `equals`/`hashCode`/`toString`.
    ValueClassAny(IrValueClassAnyMember),
    /// A function a compiler plugin generated and published.
    Generated(u32),
}

/// A published secondary constructor's record.
pub(crate) struct SecondaryConstructorRecord {
    pub(crate) params: Vec<(String, Ty)>,
    pub(crate) param_spellings: Vec<crate::spelling::Spelled>,
    pub(crate) param_defaults: Vec<bool>,
    pub(crate) vararg_index: Option<usize>,
    pub(crate) flags: u64,
    pub(crate) annotations: MetadataAnnotations,
}

/// Everything a class's `Class` record is built from, owned, with the declaration behind each entry.
pub(crate) struct ClassDeclarationRecord {
    class: TypeName,
    ctor_params: Vec<(String, Ty)>,
    pub(crate) props: Vec<PropMeta>,
    /// Which declaration each of `props` describes.
    pub(crate) prop_origins: Vec<PropertyOrigin>,
    pub(crate) methods: Vec<FnMeta>,
    /// Which declaration each of `methods` describes.
    pub(crate) method_origins: Vec<FunctionOrigin>,
    enum_entries: Vec<(String, MetadataAnnotations)>,
    spellings: crate::spelling::DeclaredSpellings,
    supertype_spellings: Vec<crate::spelling::Spelled>,
    flags: u64,
    companion: Option<String>,
    nested: Vec<String>,
    member_order: Vec<ClassMemberOrder>,
    type_aliases: Vec<TypeAliasMeta>,
    secondary_ctors: Vec<SecondaryConstructorRecord>,
    /// The ordinal in the class's secondary constructors of the declaration each of
    /// `secondary_ctors` describes.
    pub(crate) secondary_ordinals: Vec<usize>,
    ctor_param_defaults: Vec<bool>,
    ctor_param_tparams: Vec<Option<u32>>,
    ctor_param_annotations: Vec<MetadataAnnotations>,
    inline_underlying: Option<(String, Option<Ty>)>,
    ctor_vararg_index: Option<usize>,
    /// Whether the class records a primary constructor at all.
    pub(crate) emit_primary_ctor: bool,
    primary_ctor_flags: u64,
    type_params: Vec<String>,
    type_param_bounds: Vec<crate::ir::IrTypeParameter>,
    captured_type_params: CapturedParameters,
    sealed_subclasses: Vec<TypeName>,
    supertypes: Vec<Ty>,
    annotations: MetadataAnnotations,
    pub(crate) primary_ctor_annotations: MetadataAnnotations,
    local_classifiers: std::collections::HashSet<TypeName>,
    enum_entry_bodies: std::collections::HashSet<TypeName>,
    is_enum: bool,
}

/// A target's renaming of classifier identities after the record was built: the JVM gives local
/// classes their physical names after the backend handoff. The record applies it to every
/// classifier it names, exactly as the target applies it to the IR.
pub(crate) trait ClassifierRenaming {
    fn name(&self, name: TypeName) -> TypeName;
    fn ty(&self, ty: Ty) -> Ty;
    fn spelled(&self, spelled: &mut crate::spelling::Spelled);
    fn spellings(&self, spellings: &mut crate::spelling::DeclaredSpellings);
    fn annotation(&self, annotation: &mut crate::ir::AppliedAnnotation);
    fn type_parameters(&self, parameters: &mut [crate::ir::IrTypeParameter]);
}

impl ClassDeclarationRecord {
    /// Apply `renaming` to every classifier the record names.
    pub(crate) fn rename_classifiers(&mut self, renaming: &dyn ClassifierRenaming) {
        let annotations = |annotations: &mut MetadataAnnotations| {
            annotations
                .records_mut()
                .iter_mut()
                .for_each(|annotation| renaming.annotation(annotation));
        };
        let named_types = |types: &mut [(String, Ty)]| {
            types.iter_mut().for_each(|(_, ty)| *ty = renaming.ty(*ty));
        };
        self.class = renaming.name(self.class);
        named_types(&mut self.ctor_params);
        for property in &mut self.props {
            property.ty = renaming.ty(property.ty);
            property
                .context_params
                .iter_mut()
                .for_each(|(_, _, ty)| *ty = renaming.ty(*ty));
            renaming.spellings(&mut property.spellings);
            property.receiver = property.receiver.map(|ty| renaming.ty(ty));
            renaming.type_parameters(&mut property.type_params);
            annotations(&mut property.annotations);
            annotations(&mut property.field_annotations);
            annotations(&mut property.accessor_annotations.getter);
            annotations(&mut property.accessor_annotations.setter);
            annotations(&mut property.accessor_annotations.setter_parameter);
        }
        for function in &mut self.methods {
            named_types(&mut function.params);
            function.ret = renaming.ty(function.ret);
            function.receiver = function.receiver.map(|ty| renaming.ty(ty));
            function
                .type_param_bounds
                .iter_mut()
                .flatten()
                .for_each(|bound| *bound = renaming.ty(*bound));
            renaming.spellings(&mut function.spellings);
            annotations(&mut function.annotations);
            function.param_annotations.iter_mut().for_each(annotations);
        }
        self.enum_entries
            .iter_mut()
            .for_each(|(_, entry)| annotations(entry));
        renaming.spellings(&mut self.spellings);
        self.supertype_spellings
            .iter_mut()
            .for_each(|spelled| renaming.spelled(spelled));
        for alias in &mut self.type_aliases {
            alias.expansion = renaming.ty(alias.expansion);
            renaming.spelled(&mut alias.expansion_spelling);
        }
        for constructor in &mut self.secondary_ctors {
            named_types(&mut constructor.params);
            constructor
                .param_spellings
                .iter_mut()
                .for_each(|spelled| renaming.spelled(spelled));
            annotations(&mut constructor.annotations);
        }
        self.ctor_param_annotations.iter_mut().for_each(annotations);
        if let Some((_, Some(underlying))) = &mut self.inline_underlying {
            *underlying = renaming.ty(*underlying);
        }
        renaming.type_parameters(&mut self.type_param_bounds);
        self.sealed_subclasses
            .iter_mut()
            .for_each(|subclass| *subclass = renaming.name(*subclass));
        self.supertypes
            .iter_mut()
            .for_each(|supertype| *supertype = renaming.ty(*supertype));
        annotations(&mut self.annotations);
        annotations(&mut self.primary_ctor_annotations);
        self.local_classifiers = self
            .local_classifiers
            .iter()
            .map(|&classifier| renaming.name(classifier))
            .collect();
        self.enum_entry_bodies = self
            .enum_entry_bodies
            .iter()
            .map(|&classifier| renaming.name(classifier))
            .collect();
    }
}

enum CapturedParameters {
    Reserved(Vec<String>),
    NumberedOnUse(Vec<String>),
}

/// What a target sets on a class's record header: options of the compilation, the class's version
/// requirement, and how it approximates a non-denotable intersection.
pub(crate) struct ClassTailOptions<'a> {
    pub(crate) param_assertions: bool,
    pub(crate) annotations_in_metadata: bool,
    pub(crate) compiler_version_requirement:
        Option<crate::metadata::version_requirements::VersionRequirement>,
    pub(crate) intersection: Option<&'a dyn Fn(Ty) -> Option<Ty>>,
    /// The primary constructor's annotations in the order the target lists them; the record's own
    /// (source) order when `None`.
    pub(crate) primary_ctor_annotations: Option<&'a MetadataAnnotations>,
}

impl ClassDeclarationRecord {
    /// The record's class-level inputs.
    pub(crate) fn tail<'a>(
        &'a self,
        options: ClassTailOptions<'a>,
        secondary_ctors: &'a [CtorMeta<'a>],
        nested: &'a [&'a str],
    ) -> ClassTail<'a> {
        ClassTail {
            intersection_approximation: options.intersection,
            spellings: self.spellings.clone(),
            supertype_spellings: &self.supertype_spellings,
            type_params: &self.type_params,
            type_param_bounds: &self.type_param_bounds,
            captured_type_params: match &self.captured_type_params {
                CapturedParameters::Reserved(parameters) => {
                    CapturedTypeParameters::Reserved(parameters)
                }
                CapturedParameters::NumberedOnUse(parameters) => {
                    CapturedTypeParameters::NumberedOnUse(parameters)
                }
            },
            ctor_param_tparams: &self.ctor_param_tparams,
            ctor_param_annotations: &self.ctor_param_annotations,
            flags: self.flags,
            primary_ctor_flags: self.primary_ctor_flags,
            ctor_param_defaults: &self.ctor_param_defaults,
            inline_underlying: self
                .inline_underlying
                .as_ref()
                .map(|(name, ty)| (name.as_str(), *ty)),
            emit_primary_ctor: self.emit_primary_ctor,
            compiler_version_requirement: options.compiler_version_requirement,
            param_assertions: options.param_assertions,
            companion: self.companion.as_deref(),
            secondary_ctors,
            ctor_vararg_index: self.ctor_vararg_index,
            nested,
            member_order: &self.member_order,
            type_aliases: &self.type_aliases,
            sealed_subclasses: &self.sealed_subclasses,
            supertypes: &self.supertypes,
            annotations: &self.annotations,
            primary_ctor_annotations: options
                .primary_ctor_annotations
                .unwrap_or(&self.primary_ctor_annotations),
            local_classifiers: &self.local_classifiers,
            enum_entry_bodies: &self.enum_entry_bodies,
            is_enum: self.is_enum,
            annotations_in_metadata: options.annotations_in_metadata,
        }
    }

    /// The secondary constructors as the class builder takes them.
    pub(crate) fn secondary_constructors(&self) -> Vec<CtorMeta<'_>> {
        self.secondary_ctors
            .iter()
            .map(|shape| CtorMeta {
                params: &shape.params,
                param_spellings: &shape.param_spellings,
                param_defaults: &shape.param_defaults,
                vararg_index: shape.vararg_index,
                flags: shape.flags,
                annotations: &shape.annotations,
            })
            .collect()
    }

    /// The nested classifier names as the class builder takes them.
    pub(crate) fn nested(&self) -> Vec<&str> {
        self.nested.iter().map(String::as_str).collect()
    }

    /// The enum entries as the class builder takes them.
    pub(crate) fn enum_entries(&self) -> Vec<EnumEntryMeta<'_>> {
        self.enum_entries
            .iter()
            .map(|(name, annotations)| EnumEntryMeta {
                name,
                annotations: annotations.clone(),
            })
            .collect()
    }

    /// The primary constructor's declared parameters.
    pub(crate) fn ctor_params(&self) -> &[(String, Ty)] {
        &self.ctor_params
    }

    /// The record's declaration, given its tail and its enum entries.
    pub(crate) fn declaration<'a>(
        &'a self,
        tail: &'a ClassTail<'a>,
        enum_entries: &'a [EnumEntryMeta<'a>],
    ) -> ClassDeclaration<'a> {
        ClassDeclaration {
            name: self.class,
            ctor_params: &self.ctor_params,
            props: &self.props,
            methods: &self.methods,
            enum_entries,
            tail,
        }
    }
}

/// The member functions of `c` its record describes as declarations, in source order: exact
/// source-callable realizations that are not accessors, lifted local functions, synthetic methods,
/// interface-delegation forwarders or data class members.
fn declared_functions(ir: &IrFile, c: &IrClass) -> Vec<u32> {
    // Member-extension-PROPERTY accessors are described as `Property` records, never as
    // functions — kotlinc emits no `Function` record for `getDoubled` of `val Int.doubled`.
    let ext_prop_accessor_fids: std::collections::HashSet<u32> = ir
        .member_ext_props
        .get(&c.fq_name_id())
        .into_iter()
        .flatten()
        .flat_map(|prop| std::iter::once(prop.getter).chain(prop.setter))
        .collect();
    let property_accessor_fids: std::collections::HashSet<u32> = c
        .properties
        .iter()
        .flat_map(|property| property.getter.into_iter().chain(property.setter))
        .collect();
    let source_callable_fids = ir
        .checked_callable_functions
        .values()
        .copied()
        .collect::<std::collections::HashSet<_>>();
    // Metadata Function records come only from exact source-callable realizations. Accessors have
    // Property records, while generated declarations use the explicit publication contract below.
    let mut declared_fids: Vec<u32> = c
        .methods
        .iter()
        .copied()
        .filter(|&fid| {
            // A physical class method is not necessarily a Kotlin declaration. Lifted local
            // functions are implementation methods and have no Function entry. An `interface by`
            // forwarder is a Kotlin function too, but it is recorded after the class's own
            // declarations, sorted with the other delegation members; the override edge names it.
            // Property forwarders stay Property records. Metadata consumes those identities
            // instead of inferring declaration status from a name, descriptor, or parameter spelling.
            if !source_callable_fids.contains(&fid) || ir.is_interface_delegation_function(fid) {
                return false;
            }
            if ir.lambda_own_params_from.contains_key(&fid) || ir.synthetic_methods.contains(&fid) {
                return false;
            }
            if ext_prop_accessor_fids.contains(&fid) {
                return false;
            }
            if property_accessor_fids.contains(&fid) {
                return false;
            }
            !ir.is_data_class_member(c.fq_name_id(), fid)
        })
        .collect();
    declared_fids.sort_by_key(|fid| ir.fn_source_order.get(fid).copied().unwrap_or(u32::MAX));
    declared_fids
}

/// Record every class's declarations in `ir`, replacing any recorded before. Runs at the backend
/// handoff, and again after a target's compiler plugins add declarations, so a record always
/// describes declarations no target lowering has rewritten yet.
pub(crate) fn record_all(ir: &mut IrFile) {
    let records = ir
        .classes
        .iter()
        .map(|class| (class.fq_name_id(), build(ir, class)))
        .collect();
    ir.class_declarations = records;
}

/// The declaration record of class `c`; `None` when a data class lacks one of its generated members.
pub(crate) fn build(ir: &IrFile, c: &IrClass) -> Option<ClassDeclarationRecord> {
    // `data` synthesizes over the PRIMARY-CONSTRUCTOR properties only — `c.fields` also holds the
    // backing fields of body properties (`data class P(val x: Int) { val y = 1 }` has two fields but
    // one component). Counting all of them advertised a `component2` the class file does not define,
    // and a `copy(II)` where only `copy(I)` exists; real kotlinc reading that record accepts
    // `val (a, b) = p` and binds a method that is not there.
    let data_component_fields = &c.fields[..(c.ctor_param_count as usize).min(c.fields.len())];
    let data_component_properties = if c.is_data {
        (0..data_component_fields.len())
            .map(|index| {
                c.properties
                    .iter()
                    .find(|property| property.backing_field == Some(index as u32))
            })
            .collect::<Option<Vec<_>>>()?
    } else {
        Vec::new()
    };
    let declared_fids = declared_functions(ir, c);
    let generated_publication = ir.generated_member_publication(c.fq_name_id());
    // Metadata describes Kotlin PROPERTY declarations, never physical fields. Synthetic storage such
    // as `x$delegate`, `this$0`, and interface-delegation fields has no source declaration and must not
    // leak into the metadata name/type namespace. A property's backing field is its realization,
    // which a target records beside this record.
    let mut declared_props: Vec<(u32, PropertyOrigin, PropMeta)> = c
        .properties
        .iter()
        .enumerate()
        .map(|(property_index, property)| {
            crate::trace_compiler!(
                "metadata",
                "emit class metadata property owner={:?} name={} ty={:?} context={:?}",
                c.fq_name,
                property.name,
                property.ty,
                property.context_params,
            );
            let mut spellings = ir
                .prop_declared_spellings
                .get(&(c.fq_name_id(), property.source_order))
                .cloned()
                .unwrap_or_default();
            let constructor_vararg = c.ctor_args.iter().any(|arg| {
                arg.is_vararg
                    && arg.field_index.is_some()
                    && arg.field_index == property.backing_field
            });
            let ty = if constructor_vararg {
                let recorded =
                    crate::metadata::vararg_recorded_declaration(property.ty, &spellings.ret);
                spellings.ret = recorded.1;
                recorded.0
            } else {
                property.ty
            };
            let record = PropMeta {
                return_value_status: property.return_value_status,
                spellings,
                name: property.name.clone(),
                // A `vararg val` constructor property has the parameter's `Array<out E>` type.
                ty,
                context_params: property.context_params.clone(),
                is_var: property.is_var,
                visibility: property.visibility,
                // The checked initializer decides it; a hoisted companion property keeps it too.
                has_constant: property.has_constant_initializer,
                is_const: false,
                modifiers: property.modifiers,
                setter_visibility: property.setter_visibility,
                tparam: match property.ty {
                    Ty::TyParam(name, _) => Some(name),
                    Ty::Nullable(inner) | Ty::PlatformNullable(inner) => match *inner {
                        Ty::TyParam(name, _) => Some(name),
                        _ => None,
                    },
                    _ => None,
                }
                .and_then(|parameter| {
                    let parameter = crate::types::type_parameter_source_name(parameter);
                    c.type_params
                        .iter()
                        .position(|candidate| candidate == parameter)
                })
                .or_else(|| {
                    ir.field_signatures(&c.fq_name()).and_then(|signatures| {
                        signatures
                            .iter()
                            .find(|(field, _)| field == &property.name)
                            .and_then(|(_, parameter)| {
                                c.type_params
                                    .iter()
                                    .position(|candidate| candidate == parameter)
                            })
                    })
                })
                .map(|index| index as u32),
                receiver: None,
                type_params: property.type_params.clone(),
                setter_parameter_name: crate::metadata::declaration_records::explicit_setter_name(
                    ir,
                    property.setter,
                ),
                annotations: property_annotations(c, &property.name),
                field_annotations: property_backing_field_annotations(c, &property.name),
                accessor_annotations: crate::metadata::AccessorMetadataAnnotations::of(
                    &property.accessor_annotations,
                ),
                companion: false,
            };
            (
                property.source_order,
                PropertyOrigin::Declared(property_index),
                record,
            )
        })
        .collect();
    // A class's own `const val`s (`declared_class_statics`) enter here — BEFORE the extension
    // properties — and the whole set then sorts by each declaration's exact source offset (kotlinc's
    // metadata property order). The key stays attached to the declaration across storage moves.
    for &static_id in ir
        .declared_class_statics
        .get(&c.fq_name_id())
        .into_iter()
        .flatten()
    {
        let prop = &ir.statics[static_id as usize];
        let record = PropMeta {
            return_value_status: Default::default(),
            spellings: crate::spelling::DeclaredSpellings::default(),
            name: prop.name.clone(),
            ty: prop.ty,
            context_params: Vec::new(),
            is_var: prop.is_var,
            visibility: prop.visibility,
            has_constant: true,
            is_const: true,
            modifiers: Default::default(),
            setter_visibility: prop.visibility,
            tparam: None,
            receiver: None,
            type_params: Vec::new(),
            setter_parameter_name: None,
            annotations: property_annotations(c, &prop.name),
            field_annotations: property_backing_field_annotations(c, &prop.name),
            accessor_annotations: Default::default(),
            companion: false,
        };
        declared_props.push((
            prop.source_order,
            PropertyOrigin::Constant(static_id),
            record,
        ));
    }
    declared_props.extend(
        ir.companion_blocks
            .properties_of(c.fq_name_id())
            .enumerate()
            .map(|(index, property)| {
                (
                    property.source_order,
                    PropertyOrigin::CompanionBlock(index),
                    companion_block_property(ir, property),
                )
            }),
    );
    declared_props.sort_by_key(|(line, _, _)| *line);
    let mut prop_source_orders: Vec<u32> =
        declared_props.iter().map(|(order, _, _)| *order).collect();
    let mut prop_origins: Vec<PropertyOrigin> = declared_props
        .iter()
        .map(|(_, origin, _)| *origin)
        .collect();
    let mut props: Vec<PropMeta> = declared_props
        .into_iter()
        .map(|(_, _, property)| property)
        .collect();
    // Member EXTENSION properties: a `Property` record with `receiver_type` — the declaration the
    // accessor methods (excluded from the declared functions) realize.
    for (index, ext) in ir
        .member_ext_props
        .get(&c.fq_name_id())
        .into_iter()
        .flatten()
        .enumerate()
    {
        let record = PropMeta {
            return_value_status: Default::default(),
            spellings: ir
                .prop_declared_spellings
                .get(&(c.fq_name_id(), ext.source_order))
                .cloned()
                .unwrap_or_default(),
            name: ext.name.clone(),
            ty: ext.ty,
            context_params: Vec::new(),
            is_var: ext.is_var,
            visibility: ext.visibility,
            has_constant: false,
            is_const: false,
            modifiers: ext.modifiers,
            setter_visibility: ext.visibility,
            tparam: None,
            receiver: Some(ext.receiver),
            type_params: ext.type_params.clone(),
            setter_parameter_name: crate::metadata::declaration_records::explicit_setter_name(
                ir, ext.setter,
            ),
            annotations: property_annotations(c, &ext.name),
            field_annotations: Default::default(),
            accessor_annotations: Default::default(),
            companion: false,
        };
        props.push(record);
        prop_origins.push(PropertyOrigin::MemberExtension(index));
        prop_source_orders.push(
            ir.fn_source_order
                .get(&ext.getter)
                .copied()
                .unwrap_or(u32::MAX),
        );
    }
    let named_ctor_args: Vec<(String, Ty, bool, Option<u32>)> = c
        .ctor_args
        .iter()
        .filter_map(|arg| {
            arg.name.as_ref().map(|name| {
                (
                    name.clone(),
                    arg.declared_ty.unwrap_or(arg.ty),
                    arg.has_default,
                    arg.type_param,
                )
            })
        })
        .collect();
    // Parallel to `named_ctor_args` (hence the SAME unnamed-parameter filter): the class's
    // `ctor_param_annotations` covers every `ctor_args` entry, including the synthetic unnamed ones a
    // metadata constructor record never lists.
    let ctor_param_annotations: Vec<MetadataAnnotations> = c
        .ctor_args
        .iter()
        .enumerate()
        .filter(|(_, arg)| arg.name.is_some())
        .map(|(i, _)| MetadataAnnotations::of_optional(c.ctor_param_annotations.get(i)))
        .collect();
    let ctor_params_with_defaults = if c.ctor_args.is_empty() {
        c.fields
            .iter()
            .take(c.ctor_param_count as usize)
            .map(|field| {
                let type_param = ir.field_signatures(&c.fq_name()).and_then(|signatures| {
                    signatures
                        .iter()
                        .find(|(name, _)| name == &field.name)
                        .and_then(|(_, type_param)| {
                            c.type_params
                                .iter()
                                .position(|candidate| candidate == type_param)
                        })
                        .map(|index| index as u32)
                });
                (
                    field.name.clone(),
                    field.ty,
                    field.has_default(),
                    type_param,
                )
            })
            .collect()
    } else {
        // `ctor_args` may contain only the unnamed enclosing-instance parameter of an `inner`
        // class. That parameter belongs to the target's realization, never in Kotlin metadata's
        // source value-parameter list; an empty `named_ctor_args` is therefore authoritative.
        named_ctor_args
    };
    // Position of a `vararg` primary-ctor parameter within the NAMED parameter list (the same
    // filtered order `ctor_params` uses) — `None` when unnamed-args fallback is in effect.
    let ctor_vararg_index = c
        .ctor_args
        .iter()
        .filter(|arg| arg.name.is_some())
        .position(|arg| arg.is_vararg);
    let ctor_params: Vec<(String, Ty)> = ctor_params_with_defaults
        .iter()
        .map(|(name, ty, _, _)| (name.clone(), *ty))
        .collect();
    let ctor_param_defaults: Vec<bool> = ctor_params_with_defaults
        .iter()
        .map(|(_, _, has_default, _)| *has_default)
        .collect();
    let ctor_param_tparams: Vec<Option<u32>> = ctor_params_with_defaults
        .iter()
        .map(|(_, _, _, type_param)| *type_param)
        .collect();
    let callables: std::collections::HashMap<u32, crate::fir::CallableId> = ir
        .checked_callable_functions
        .iter()
        .map(|(&callable, &function)| (function, callable))
        .collect();
    let class_ty = Ty::obj_name(c.fq_name);
    let declared_method_list = declared_fids
        .iter()
        .filter_map(|&fid| {
            let origin = FunctionOrigin::Declared(
                *callables
                    .get(&fid)
                    .expect("a declared metadata function is a checked callable's realization"),
            );
            describe_function(ir, fid).map(|record| (record, origin))
        })
        .collect::<Vec<_>>();
    let declared_method_count = declared_method_list.len();
    let inferred_methods = if c.is_data {
        let mut methods = declared_method_list;
        methods.extend(data_class_functions(
            ir,
            c,
            &data_component_properties,
            class_ty,
        )?);
        methods
    } else if c.is_value {
        let mut declared = declared_method_list;
        declared.extend(value_class_functions(ir, c));
        declared
    } else {
        declared_method_list
    };
    let mut methods = match generated_publication.map(|publication| publication.metadata_scope) {
        Some(crate::ir::IrGeneratedFunctionMetadataScope::Exclusive) => Vec::new(),
        Some(crate::ir::IrGeneratedFunctionMetadataScope::Additive) | None => inferred_methods,
    };
    let inferred_method_count = methods.len();
    if let Some(publication) = generated_publication {
        methods.extend(
            generated_functions(ir, publication)
                .into_iter()
                .map(|(fid, record)| (record, FunctionOrigin::Generated(fid))),
        );
    }
    let mut delegation = member_order::delegation_functions(ir, c)
        .into_iter()
        .filter_map(|fid| {
            describe_function(ir, fid).map(|record| (record, FunctionOrigin::Delegation(fid)))
        })
        .collect::<Vec<_>>();
    member_order::sort_delegation_functions(&mut delegation);
    let delegation_functions = methods.len()..methods.len() + delegation.len();
    methods.extend(delegation);
    let (methods, method_origins): (Vec<FnMeta>, Vec<FunctionOrigin>) = methods.into_iter().unzip();
    let type_aliases = ir
        .class_type_aliases
        .get(&c.fq_name_id())
        .into_iter()
        .flatten()
        .map(|alias| TypeAliasMeta {
            name: alias.name.clone(),
            formals: alias.formals.clone(),
            expansion: alias.expansion,
            visibility: alias.visibility,
            expansion_spelling: alias.expansion_spelling.clone(),
            decl_order: alias.source_order as usize,
        })
        .collect::<Vec<_>>();
    let delegation_properties = member_order::sort_delegation_properties(
        &mut props,
        &mut prop_origins,
        &mut prop_source_orders,
    );
    let member_order = member_order::member_order(
        ir,
        c,
        &prop_source_orders,
        &declared_fids,
        MemberRanges {
            synthesized: declared_method_count..inferred_method_count,
            delegation_functions,
            delegation_properties,
        },
        generated_publication,
        &type_aliases,
    );
    let enum_entries = c
        .enum_entries
        .iter()
        .map(|entry| {
            // An enum constant has no property declaration. Its annotations live on the physical
            // field in common IR, while Kotlin metadata records them on the enum entry.
            (
                entry.name.clone(),
                MetadataAnnotations::of_optional(
                    c.field_annotations
                        .iter()
                        .find(|annotations| annotations.field == entry.name)
                        .map(|annotations| &annotations.annotations),
                ),
            )
        })
        .collect();
    // `c.fields` may be a target's storage realization by this point: value-class lowering may
    // replace a generic underlying parameter with the carrier of its bound. Kotlin metadata instead
    // describes the source property. Keep those two facts separate, especially for
    // `V<T : U<Int>>(val value: T)`: the physical field may use `U`'s carrier, but
    // Class.inlineClassUnderlyingType must still name this class's own `T` identity.
    let inline_underlying = c.is_value.then(|| {
        let property = c
            .properties
            .iter()
            .find(|property| property.backing_field == Some(0))
            .expect("a value class must retain its semantic underlying property");
        (
            property.name.clone(),
            (!property.visibility.is_public_api()).then_some(property.ty),
        )
    });
    let (supertypes, spellings, supertype_spellings) = declared_supertypes(ir, c);
    let enclosing_type_parameters = local_classifiers::enclosing_type_parameters(ir, c);
    let (secondary_ctors, secondary_ordinals) = secondary_constructors(c, ir).into_iter().unzip();
    Some(ClassDeclarationRecord {
        class: c.fq_name_id(),
        ctor_params,
        props,
        prop_origins,
        methods,
        method_origins,
        enum_entries,
        spellings,
        supertype_spellings,
        flags: class_flags(ir, c),
        // A class with a companion records its simple name (`Class.companionObjectName`, f4) —
        // the consumer resolves `C.member` through it.
        companion: c
            .companion_class
            .as_ref()
            .map(|companion| companion.nested_segment_ref().to_string()),
        nested: nested_classifiers(ir, c),
        member_order,
        type_aliases,
        secondary_ctors,
        secondary_ordinals,
        ctor_param_defaults,
        ctor_param_tparams,
        ctor_param_annotations,
        inline_underlying,
        ctor_vararg_index,
        // An interface has no constructor; a class with ONLY secondary constructors emits no
        // primary record either (its `Class.constructor` entries are the secondaries). Every other
        // class keeps its (possibly implicit) primary record — an `enum class` without a declared
        // constructor still records the implicit private one. An anonymous object's constructor (an
        // enum entry body's too) is not callable.
        emit_primary_ctor: !c.is_interface
            && !c.is_anonymous_object
            && !c.is_enum_entry
            && (c.has_primary_ctor || c.secondary_ctors.is_empty()),
        // An `enum class`'s primary ctor is private too — entries are the only instances.
        // A DECLARED constructor visibility (`class C protected constructor(…)`) takes
        // precedence: the consumer must reject constructions the declaration forbids.
        primary_ctor_flags: match ir.ctor_visibilities.get(&c.fq_name_id()) {
            Some(Visibility::Protected) => SEALED_CTOR_FLAGS,
            Some(Visibility::Private) => OBJECT_CTOR_FLAGS,
            _ if c.is_sealed => SEALED_CTOR_FLAGS,
            _ if c.is_singleton() || c.is_enum => OBJECT_CTOR_FLAGS,
            _ => 0,
        },
        type_params: c.type_params.clone(),
        type_param_bounds: ir
            .class_signature(&c.fq_name())
            .map(|signature| signature.type_params.clone())
            .unwrap_or_default(),
        captured_type_params: if c.is_local_class || c.is_anonymous_object {
            CapturedParameters::NumberedOnUse(c.captured_type_params.clone())
        } else {
            CapturedParameters::Reserved(enclosing_type_parameters)
        },
        // `Class.sealedSubclassFqName` (f16) belongs only to a SEALED classifier — the IR records
        // subtype relationships for every class, but kotlinc writes the field for sealed ones alone
        // (a plain interface with implementors carries none).
        sealed_subclasses: if c.is_sealed {
            sorted_sealed_subclasses(c)
        } else {
            Vec::new()
        },
        supertypes,
        annotations: MetadataAnnotations::of(&c.applied_annotations),
        primary_ctor_annotations: MetadataAnnotations::of(&c.primary_ctor_annotations),
        local_classifiers: local_classifiers::names(ir),
        enum_entry_bodies: local_classifiers::enum_entry_bodies(ir),
        is_enum: c.is_enum,
    })
}

/// One declared (or delegation) member function's record.
fn describe_function(ir: &IrFile, fid: u32) -> Option<FnMeta> {
    let f = ir.functions.get(fid as usize)?;
    // Real parameter identities — metadata is reflection-visible, so a placeholder would be an
    // observable lie. A missing semantic name is rejected below.
    let parameter_identities = ir.function_parameter_identities(fid);
    // The record is built before any target lowering, so the function is still as source declared
    // it: its own name, parameters and return type.
    let (params, ret) = (f.params.as_slice(), f.ret);
    let suspend = ir.suspend_funs.contains(&fid);
    let name = ir
        .fn_source_names
        .get(&fid)
        .map(String::as_str)
        .expect("a source metadata function retains its declaration name");
    let semantic_signature = ir.signatures.get(&fid);
    // A member mentioning an ENCLOSING-CLASS type parameter records its semantic shape separately
    // (`member_semantic_sigs`) — the erased params would publish `Any`.
    let member_semantic = ir
        .member_semantic_sigs
        .get(&fid)
        .filter(|_| !ir.extension_receiver_fns.contains(&fid));
    let metadata_params = semantic_signature
        .map(|signature| signature.params.as_slice())
        .or(member_semantic.map(|(params, _)| params.as_slice()))
        .unwrap_or(params);
    let metadata_ret = semantic_signature
        .and_then(|signature| signature.ret)
        .or(member_semantic.map(|(_, ret)| *ret))
        .unwrap_or(ret);
    // A parameter's declared `?` lives in a side-table, not in `params` (which stays non-null so the
    // value-class mangle is undisturbed) — re-apply it for the record.
    let declared_nullable = ir.fn_param_declared_nullable.get(&fid);
    let function_type_params = semantic_signature
        .map(|signature| {
            signature
                .type_params
                .iter()
                .map(|parameter| parameter.name.clone())
                .collect()
        })
        .unwrap_or_default();
    let semantic_function_type_params = semantic_signature
        .map(|signature| {
            signature
                .type_params
                .iter()
                .map(|parameter| parameter.semantic_name.clone())
                .collect()
        })
        .unwrap_or_default();
    let function_type_param_bounds = semantic_signature
        .map(|signature| {
            signature
                .type_params
                .iter()
                .map(|parameter| parameter.bounds.iter().map(|(bound, _)| *bound).collect())
                .collect()
        })
        .unwrap_or_default();
    // Restore an extension receiver to `Function.receiver_type` so metadata keeps only the
    // declaration's logical value parameters.
    let member_context_count = ir.fn_context_counts.get(&fid).copied().unwrap_or(0);
    let is_ext =
        ir.extension_receiver_fns.contains(&fid) && member_context_count < metadata_params.len();
    let receiver_index = is_ext.then_some(member_context_count);
    let parameter_identities =
        parameter_identities.expect("a metadata function retains exact parameter identities");
    assert!(
        metadata_params.len() <= parameter_identities.len(),
        "metadata declaration parameters retain their exact identities"
    );
    let apply_nullable = |source_index: usize, t: Ty| {
        if declared_nullable
            .and_then(|v| v.get(source_index))
            .copied()
            .unwrap_or(false)
        {
            Ty::nullable(t)
        } else {
            t
        }
    };
    let receiver = receiver_index.map(|index| apply_nullable(index, metadata_params[index]));
    let logical_param_indices = (0..metadata_params.len())
        .filter(|&index| receiver_index != Some(index))
        .collect::<Vec<_>>();
    let logical_params: Vec<(String, Ty)> = logical_param_indices
        .iter()
        .map(|&metadata_index| {
            let identity = parameter_identities
                .get(metadata_index)
                .expect("a metadata parameter carries an exact identity");
            let n = match identity.role {
                crate::ir::IrParameterRole::ContextReceiver { .. } => String::new(),
                // A delegation forwarder of a parameter its interface declares without a name (a
                // Java interface compiled without parameter names) records kotlinc's `pN`.
                crate::ir::IrParameterRole::Generated(
                    crate::ir::IrGeneratedParameterRole::InterfaceDelegationValue { ordinal },
                ) => format!("p{ordinal}"),
                _ => crate::metadata::declaration_records::parameter_name(identity)
                    .expect("a metadata value parameter carries a semantic name")
                    .to_string(),
            };
            (
                n,
                apply_nullable(metadata_index, metadata_params[metadata_index]),
            )
        })
        .collect();
    let context_parameter_kinds = parameter_identities
        .iter()
        .take(member_context_count)
        .map(crate::metadata::declaration_records::context_kind)
        .collect();
    // Per-parameter declaration facts. Only the defaults the declaration writes itself count; an
    // override's inherited defaults stay with the declaration it overrides.
    let defaults = ir.declared_param_defaults(fid).into_iter().flat_map(|ds| {
        let own = ds
            .iter()
            .enumerate()
            .filter(|(i, _)| receiver_index != Some(*i));
        own.map(|(_, d)| d.is_some())
    });
    let param_modifiers =
        crate::metadata::declaration_records::declared_value_parameters(ir, fid, defaults);
    let vararg_index = ir.fn_varargs.get(&fid).map(|vararg| vararg.index);
    Some(FnMeta {
        // How SOURCE spelled this member's declared types — carried on the IR because class
        // metadata is built without the AST (see `IrFile::fn_declared_spellings`).
        spellings: ir
            .fn_declared_spellings
            .get(&fid)
            .cloned()
            .unwrap_or_default(),
        name: name.to_string(),
        params: logical_params,
        ret: metadata_ret,
        receiver,
        type_params: function_type_params,
        semantic_type_params: semantic_function_type_params,
        type_param_bounds: function_type_param_bounds,
        flags: function_flags(ir, fid, f) | if suspend { FN_IS_SUSPEND } else { 0 },
        has_function_typed_parameter: ir.function_typed_parameter_fns.contains(&fid),
        params_have_defaults: false,
        param_modifiers,
        vararg_index,
        context_count: member_context_count,
        context_parameter_kinds,
        // The declaration's own annotations, mirrored into the record. SOURCE-retained
        // annotations were dropped during lowering and never reach it.
        annotations: MetadataAnnotations::of_optional(ir.function_annotations.get(&fid)),
        // `metadata_params` is the DECLARED parameter list (a suspend fn's synthesized
        // `Continuation` is already dropped), so the side table lines up with it.
        param_annotations: logical_param_indices
            .iter()
            .map(|&metadata_index| {
                ir.fn_param_annotations
                    .get(&fid)
                    .and_then(|table| table.get(metadata_index))
                    .map(MetadataAnnotations::of)
                    .unwrap_or_default()
            })
            .collect(),
        no_infer_params: logical_param_indices
            .iter()
            .map(|&metadata_index| {
                ir.fn_param_no_infer
                    .get(&fid)
                    .and_then(|flags| flags.get(metadata_index))
                    .copied()
                    .unwrap_or(false)
            })
            .collect(),
        contract: ir.fn_contracts.get(&fid).map(|contract| contract.to_arc()),
        has_source: true,
    })
}

/// kotlinc's generated data class members, after the declared ones: `componentN`, `copy`,
/// `equals`, `hashCode`, `toString`. Their shapes come entirely from the primary-ctor properties.
/// `None` when the class lacks a `componentN` or `copy` its record would describe.
fn data_class_functions(
    ir: &IrFile,
    c: &IrClass,
    components: &[&IrProperty],
    class_ty: Ty,
) -> Option<Vec<(FnMeta, FunctionOrigin)>> {
    let owner = c.fq_name_id();
    let mut methods = Vec::new();
    for (i, property) in components.iter().enumerate() {
        let role = IrDataClassMemberRole::Component(i as u32);
        ir.data_class_member(owner, role)?;
        let mut record = FnMeta::plain(format!("component{}", i + 1), Vec::new(), property.ty);
        record.flags = data_component_flags(ir, c, i as u32)?;
        methods.push((record, FunctionOrigin::DataMember(role)));
    }
    // A `data object` is a SINGLETON: kotlinc gives it `equals`/`hashCode`/`toString` ONLY —
    // there is nothing to copy from and no primary-constructor property to destructure.
    if c.is_data && !c.is_singleton() {
        ir.data_class_member(owner, IrDataClassMemberRole::Copy)?;
        let mut record = FnMeta::plain(
            "copy".into(),
            components
                .iter()
                .map(|property| (property.name.clone(), property.ty))
                .collect(),
            class_ty,
        );
        record.flags = data_copy_flags(ir, c);
        record.params_have_defaults = true;
        methods.push((
            record,
            FunctionOrigin::DataMember(IrDataClassMemberRole::Copy),
        ));
    }
    // `equals`, `hashCode` and `toString` override `Any`'s and have no source of their own.
    let overrides = [
        (IrDataClassMemberRole::Equals, IrValueClassAnyMember::Equals),
        (
            IrDataClassMemberRole::HashCode,
            IrValueClassAnyMember::HashCode,
        ),
        (
            IrDataClassMemberRole::ToString,
            IrValueClassAnyMember::ToString,
        ),
    ];
    for (role, member) in overrides {
        if ir.data_class_member(owner, role).is_none() {
            continue;
        }
        methods.push((any_override(member), FunctionOrigin::DataMember(role)));
    }
    Some(methods)
}

/// The `equals`/`hashCode`/`toString` a value class generates, in kotlinc's order: each `Any`
/// member its source does not override.
fn value_class_functions(ir: &IrFile, c: &IrClass) -> Vec<(FnMeta, FunctionOrigin)> {
    let overridden = ir.any_member_overrides(c.fq_name_id());
    [
        IrValueClassAnyMember::Equals,
        IrValueClassAnyMember::HashCode,
        IrValueClassAnyMember::ToString,
    ]
    .into_iter()
    .filter(|member| {
        !c.methods
            .iter()
            .any(|function| overridden.get(function) == Some(member))
    })
    .map(|member| (any_override(member), FunctionOrigin::ValueClassAny(member)))
    .collect()
}

/// The records of the functions a compiler plugin generated and published, each with its function:
/// the published name, visibility and parameter names over the function's semantic signature.
fn generated_functions(
    ir: &IrFile,
    publication: &crate::ir::IrGeneratedMemberPublication,
) -> Vec<(u32, FnMeta)> {
    publication
        .functions
        .iter()
        .filter_map(|member| {
            let metadata = member.metadata.as_ref()?;
            let function = &ir.functions[member.function as usize];
            let semantic_signature = ir.signatures.get(&member.function);
            let member_semantic = ir.member_semantic_sigs.get(&member.function);
            let params = semantic_signature
                .map(|signature| signature.params.as_slice())
                .or(member_semantic.map(|(params, _)| params.as_slice()))
                .unwrap_or(function.params.as_slice());
            let ret = semantic_signature
                .and_then(|signature| signature.ret)
                .or(member_semantic.map(|(_, ret)| *ret))
                .unwrap_or(function.ret);
            assert_eq!(
                member.parameter_identities.len(),
                params.len(),
                "generated metadata parameter identities exactly match semantic arity"
            );
            let mut record = FnMeta::plain(
                metadata.source_name.clone(),
                member
                    .parameter_identities
                    .iter()
                    .map(|identity| {
                        crate::metadata::declaration_records::parameter_name(identity)
                            .expect("generated metadata parameters carry semantic names")
                            .to_string()
                    })
                    .zip(params.iter().copied())
                    .collect(),
                ret,
            );
            record.flags = generated_function_flags(ir, member.function, metadata.visibility);
            record.annotations =
                MetadataAnnotations::of_optional(ir.function_annotations.get(&member.function));
            Some((member.function, record))
        })
        .collect()
}

/// `Function.flags` of a generated function: its published visibility and its modality.
fn generated_function_flags(ir: &IrFile, function: u32, visibility: Visibility) -> u64 {
    let modality = if ir.functions[function as usize].body.is_none() {
        2
    } else if ir.open_methods.contains(&function) {
        1
    } else {
        0
    };
    (declaration_visibility_bits(visibility) << 1) | (modality << 4)
}

/// A generated override of one of `Any`'s members, which has no source of its own.
fn any_override(member: IrValueClassAnyMember) -> FnMeta {
    let mut record = match member {
        IrValueClassAnyMember::Equals => {
            let mut record = FnMeta::plain(
                "equals".into(),
                vec![("other".into(), Ty::nullable(Ty::obj("kotlin/Any")))],
                Ty::Boolean,
            );
            record.flags = EQUALS_FN_FLAGS;
            record
        }
        IrValueClassAnyMember::HashCode => {
            let mut record = FnMeta::plain("hashCode".into(), Vec::new(), Ty::Int);
            record.flags = HASHCODE_TOSTRING_FN_FLAGS;
            record
        }
        IrValueClassAnyMember::ToString => {
            let mut record = FnMeta::plain("toString".into(), Vec::new(), Ty::String);
            record.flags = HASHCODE_TOSTRING_FN_FLAGS;
            record
        }
    };
    record.has_source = false;
    record
}

/// A `companion { … }` block property's record: a static member of the class declaring the block.
fn companion_block_property(
    ir: &IrFile,
    property: &crate::ir::IrCompanionBlockProperty,
) -> PropMeta {
    PropMeta {
        return_value_status: Default::default(),
        spellings: crate::spelling::DeclaredSpellings::default(),
        name: property.name.clone(),
        ty: property.ty,
        context_params: Vec::new(),
        is_var: property.is_var,
        visibility: property.visibility,
        has_constant: property.has_constant,
        is_const: property.is_const,
        modifiers: property.modifiers,
        setter_visibility: property.setter_visibility,
        tparam: None,
        receiver: None,
        type_params: Vec::new(),
        setter_parameter_name: crate::metadata::declaration_records::explicit_setter_name(
            ir,
            property.setter,
        ),
        annotations: Default::default(),
        field_annotations: Default::default(),
        accessor_annotations: Default::default(),
        companion: true,
    }
}

/// The supertypes the class DECLARED, in source order, with the class header's spellings and the
/// supertypes' spellings aligned to them.
fn declared_supertypes(
    ir: &IrFile,
    c: &IrClass,
) -> (
    Vec<Ty>,
    crate::spelling::DeclaredSpellings,
    Vec<crate::spelling::Spelled>,
) {
    let superclass = c.superclass;
    let any = crate::types::wk::any();
    let mut supertypes = ir
        .class_signature(&c.fq_name())
        .filter(|signature| !signature.supers.is_empty())
        .map(|signature| signature.supers.clone())
        .unwrap_or_default();
    // A generic class's recorded supers ALWAYS materialize the superclass position — holding
    // `kotlin/Any` when none was declared. The record lists only the supertypes source DECLARED,
    // so drop that implicit `Any`; leaving it in shows every consumer a supertype the declaration
    // never wrote. Both shapes then agree: a superclass slot exists exactly when one was declared.
    if superclass == any
        && supertypes
            .first()
            .is_some_and(|first| matches!(first, Ty::Obj(n, _) if *n == any))
    {
        supertypes.remove(0);
    }
    if supertypes.is_empty() {
        if superclass != any {
            supertypes.push(Ty::obj_name(superclass));
        }
        supertypes.extend(c.interfaces.iter_ids().map(Ty::obj_name));
    }
    // The header's spellings have to follow the same shape, or every abbreviation lands on the
    // neighbouring supertype.
    let has_declared_superclass = superclass != any;
    let class_spellings = ir
        .class_declared_spellings
        .get(&c.fq_name_id())
        .cloned()
        .unwrap_or_default();
    let mut supertype_spellings = class_spellings.supertype_spellings(has_declared_superclass);
    // Both lists lead with the superclass; the record lists it where the source wrote it.
    let superclass_position = ir.class_superclass_positions.get(&c.fq_name_id());
    if let Some(position) = superclass_position.filter(|_| has_declared_superclass) {
        let position = *position as usize;
        // An interface without a spelling has no entry yet; it must not shift the superclass's.
        if supertype_spellings.len() <= position {
            supertype_spellings.resize(position + 1, crate::spelling::Spelled::default());
        }
        supertypes[..=position].rotate_left(1);
        supertype_spellings[..=position].rotate_left(1);
    }
    (supertypes, class_spellings, supertype_spellings)
}

/// The simple names `Class.nestedClassName` (f7) lists: every DECLARED direct nested classifier in
/// declaration order, the names a producer generates and publishes, then the companion.
pub(crate) fn nested_classifiers(ir: &IrFile, c: &IrClass) -> Vec<String> {
    // Declaration origin and the exact identity-tree relation keep synthesized classes out without
    // interpreting their backend spellings. Common IR carries the stable declaration order selected
    // by the frontend; the IR arena and debug lines do not define this order.
    let mut source_nested: Vec<(u32, String)> = ir
        .classes
        .iter()
        .enumerate()
        .filter(|(_, candidate)| {
            // A classifier declared in executable code is nobody's member; one nested in a local
            // class is that class's member like any other.
            candidate.is_source_declared
                && candidate.enclosure.is_none()
                && candidate.fq_name.nested_owner() == Some(c.fq_name)
        })
        .map(|(index, candidate)| {
            (
                ir.class_source_order(index as crate::ir::ClassId)
                    .expect("a source classifier carries its stable declaration order"),
                candidate.fq_name.nested_segment_ref().to_string(),
            )
        })
        .collect();
    source_nested.sort_by_key(|(source_order, _)| *source_order);
    let mut nested_names: Vec<String> = source_nested
        .into_iter()
        .map(|(_, segment)| segment)
        .collect();
    // A producer can generate a classifier that Kotlin code names (`Foo.$serializer`) and publish
    // that fact on the owning class. Other synthesized implementation classes stay out on their
    // `is_source_declared` record alone. Generated names precede the COMPANION, which kotlinc lists
    // last of that set (`$serializer` then `Companion` for a `@Serializable` class).
    let generated_nested = c.published_nested_classifiers.iter().cloned();
    // kotlinc lists the companion under `nestedClassName` (f7) TOO, alongside its own
    // `companionObjectName` (f4) record — both reference the same interned string.
    let companion_segment = c
        .companion_class
        .as_ref()
        .map(|companion| companion.nested_segment_ref().to_string());
    let at = companion_segment
        .as_ref()
        .and_then(|segment| nested_names.iter().position(|name| name == segment))
        .unwrap_or(nested_names.len());
    nested_names.splice(at..at, generated_nested);
    if let Some(segment) = companion_segment {
        if !nested_names.contains(&segment) {
            nested_names.push(segment);
        }
    }
    nested_names
}

/// A sealed classifier's direct subclasses in kotlinc's order: by simple name, then by path.
pub(crate) fn sorted_sealed_subclasses(c: &IrClass) -> Vec<TypeName> {
    let mut subclasses: Vec<TypeName> = c.sealed_subclasses.iter_ids().collect();
    subclasses.sort_by(|a, b| {
        a.nested_segment_ref()
            .cmp(b.nested_segment_ref())
            .then_with(|| a.path_cmp(*b))
    });
    subclasses
}

/// The annotations of a PROPERTY's own applications. The records name the same applications a
/// target's annotation carrier holds; a property whose only application is an erased optional
/// expectation has no carrier yet still declares annotations.
fn property_annotations(c: &IrClass, property: &str) -> MetadataAnnotations {
    MetadataAnnotations::of_optional(
        c.property_annotations
            .iter()
            .find(|annotations| annotations.property == property)
            .map(|annotations| &annotations.annotations),
    )
}

/// The annotations that landed on a property's BACKING FIELD.
fn property_backing_field_annotations(c: &IrClass, property: &str) -> MetadataAnnotations {
    MetadataAnnotations::of_optional(
        c.field_annotations
            .iter()
            .find(|annotations| annotations.field == property)
            .map(|annotations| &annotations.annotations),
    )
}

/// The class's published secondary constructors, in record order, each with the declaration it
/// describes.
fn secondary_constructors(c: &IrClass, ir: &IrFile) -> Vec<(SecondaryConstructorRecord, usize)> {
    let serialization = ir
        .generated_secondary_constructor_by_owner(
            c.fq_name_id(),
            crate::ir::IrSecondaryConstructorRole::SerializationDeserialization,
        )
        .and_then(|ordinal| usize::try_from(ordinal).ok());
    c.secondary_ctors
        .iter()
        .enumerate()
        .filter_map(|(ordinal, constructor)| {
            let mut record = secondary_constructor(c, constructor)?;
            // The serialization plugin's deserializing constructor takes every property, present or
            // not in the input, so each of its reference parameters admits null.
            if serialization == Some(ordinal) {
                assert_eq!(
                    record.params.len(),
                    constructor.params.len(),
                    "serialization constructor metadata exactly matches its parameter contract"
                );
                for ((_, declared), parameter) in record.params.iter_mut().zip(&constructor.params)
                {
                    // `Unit` is the `kotlin.Unit` singleton as a parameter.
                    if matches!(parameter, Ty::Unit) || parameter.is_reference() {
                        *declared = Ty::nullable(*declared);
                    }
                }
            }
            Some((record, ordinal))
        })
        .collect()
}

/// A secondary constructor's record; `None` for a constructor the declaration does not publish.
fn secondary_constructor(
    class: &IrClass,
    constructor: &IrSecondaryCtor,
) -> Option<SecondaryConstructorRecord> {
    // Every enum constructor is private, as the primary's record says too: entries are the only
    // instances.
    let visibility = constructor.metadata_visibility.map(|visibility| {
        if class.is_enum {
            Visibility::Private
        } else {
            visibility
        }
    })?;
    Some(SecondaryConstructorRecord {
        params: constructor.named_params.clone(),
        param_spellings: constructor.declared_spellings.params.clone(),
        param_defaults: constructor.defaults.iter().map(Option::is_some).collect(),
        vararg_index: constructor.vararg_index,
        flags: secondary_constructor_flags(visibility),
        annotations: MetadataAnnotations::of(&constructor.annotations),
    })
}

/// `Constructor.flags` of a secondary constructor: `IS_SECONDARY` and its visibility.
fn secondary_constructor_flags(visibility: Visibility) -> u64 {
    const IS_SECONDARY: u64 = 16;
    IS_SECONDARY | (declaration_visibility_bits(visibility) << 1)
}

#[cfg(test)]
mod tests {
    use super::{generated_function_flags, generated_functions, SecondaryConstructorRecord};
    use super::{secondary_constructor, secondary_constructor_flags, secondary_constructors};
    use crate::ir::{
        CtorDelegateTarget, DeclarationAnnotations, IrFile, IrFunction,
        IrGeneratedDeclarationDebug, IrGeneratedFunctionMetadata, IrGeneratedFunctionMetadataScope,
        IrGeneratedFunctionPublication, IrGeneratedMemberPublication, IrSecondaryConstructorRole,
        IrSecondaryCtor,
    };
    use crate::plugins::synthetic_class;
    use crate::types::{type_name, Ty, Visibility};

    fn constructor(metadata_visibility: Option<Visibility>) -> IrSecondaryCtor {
        IrSecondaryCtor {
            annotations: DeclarationAnnotations::default(),
            source_order: u32::MAX,
            lines: crate::ir::IrSecondaryCtorLines::default(),
            prefix_params: vec![Ty::Boolean],
            params: vec![Ty::Int, Ty::obj("kotlin/String")],
            declared_spellings: crate::spelling::DeclaredSpellings::default(),
            named_params: vec![
                ("seen0".to_string(), Ty::Int),
                ("value".to_string(), Ty::obj("kotlin/String")),
            ],
            metadata_visibility,
            generated_debug: crate::ir::IrGeneratedDeclarationDebug::None,
            vararg_index: Some(1),
            defaults: vec![Some(0), None],
            delegate_prelude: Vec::new(),
            delegate_args: Vec::new(),
            default_parameters: Vec::new(),
            body: None,
            delegate: CtorDelegateTarget::Super {
                owner: type_name("kotlin/Any"),
                target_params: Vec::new(),
                target: crate::ir::IrConstructorTarget::UNRESTRICTED_PRIMARY,
                default_masks: Vec::new(),
            },
            synthetic: true,
            vc_params: false,
            param_checks: Vec::new(),
        }
    }

    fn params(records: &[(SecondaryConstructorRecord, usize)]) -> Vec<Vec<(String, Ty)>> {
        records
            .iter()
            .map(|(record, _)| record.params.clone())
            .collect()
    }

    #[test]
    fn publication_is_explicit_not_inferred_from_names_or_synthetic_shape() {
        let class = synthetic_class("sample/Owner");
        let mut unpublished = constructor(None);
        unpublished.synthetic = false;
        assert!(
            secondary_constructor(&class, &unpublished).is_none(),
            "a non-synthetic classfile method does not publish a constructor"
        );

        let mut empty_published = constructor(Some(Visibility::Internal));
        empty_published.named_params.clear();
        assert!(
            secondary_constructor(&class, &empty_published).is_some(),
            "an explicit zero-parameter publication does not need a spelling sentinel"
        );

        let source = constructor(Some(Visibility::Internal));
        let published =
            secondary_constructor(&class, &source).expect("explicitly published constructor");
        assert_eq!(published.params, source.named_params);
        assert_eq!(published.param_defaults, [true, false]);
        assert_eq!(published.vararg_index, Some(1));
        assert_eq!(published.flags, 16);
        assert!(!published.annotations.declares_annotations());
    }

    #[test]
    fn the_deserializing_constructor_admits_null_for_each_reference_parameter() {
        let mut ir = IrFile::default();
        let class_id = ir.add_class(synthetic_class("sample/Owner"));
        let mut generated = constructor(Some(Visibility::Internal));
        generated.prefix_params.clear();
        generated.params = vec![
            Ty::Int,
            Ty::obj("kotlin/String"),
            Ty::Unit,
            Ty::ty_param("T", Ty::obj("kotlin/Any")),
            Ty::Long,
        ];
        generated.named_params = vec![
            ("primitive".to_string(), Ty::Int),
            ("reference".to_string(), Ty::obj("kotlin/String")),
            ("unit".to_string(), Ty::Unit),
            (
                "generic".to_string(),
                Ty::ty_param("T", Ty::obj("kotlin/Any")),
            ),
            ("value".to_string(), Ty::obj("sample/Value")),
        ];
        let unrelated = constructor(Some(Visibility::Internal));
        let class = &mut ir.classes[class_id as usize];
        class.secondary_ctors.push(generated);
        class.secondary_ctors.push(unrelated.clone());
        ir.record_generated_secondary_constructor(
            class_id,
            IrSecondaryConstructorRole::SerializationDeserialization,
            0,
        );

        let records = secondary_constructors(&ir.classes[class_id as usize], &ir);
        assert_eq!(
            params(&records),
            vec![
                vec![
                    ("primitive".to_string(), Ty::Int),
                    (
                        "reference".to_string(),
                        Ty::nullable(Ty::obj("kotlin/String")),
                    ),
                    ("unit".to_string(), Ty::nullable(Ty::Unit)),
                    (
                        "generic".to_string(),
                        Ty::nullable(Ty::ty_param("T", Ty::obj("kotlin/Any"))),
                    ),
                    ("value".to_string(), Ty::obj("sample/Value")),
                ],
                unrelated.named_params,
            ]
        );
        assert_eq!(
            records
                .iter()
                .map(|(_, origin)| *origin)
                .collect::<Vec<_>>(),
            [0, 1]
        );
    }

    #[test]
    fn secondary_constructor_flags_encode_exact_semantic_visibility() {
        assert_eq!(secondary_constructor_flags(Visibility::Internal), 16);
        assert_eq!(secondary_constructor_flags(Visibility::Private), 18);
        assert_eq!(secondary_constructor_flags(Visibility::Protected), 20);
        assert_eq!(secondary_constructor_flags(Visibility::Public), 22);
    }

    fn generated_function(ir: &mut IrFile, body: bool) -> u32 {
        let body = body.then(|| {
            ir.add_expr(crate::ir::IrExpr::Block {
                stmts: Vec::new(),
                value: None,
            })
        });
        ir.add_fun(IrFunction {
            name: "write$Self".to_string(),
            params: vec![Ty::obj("sample/Value"), Ty::String, Ty::Unit],
            ret: Ty::Unit,
            body,
            is_static: true,
            dispatch_receiver: None,
            param_checks: vec![None, None, None],
        })
    }

    #[test]
    fn generated_function_flags_use_semantic_visibility_and_real_modality() {
        let mut ir = IrFile::default();
        let function = generated_function(&mut ir, true);
        assert_eq!(
            generated_function_flags(&ir, function, Visibility::Internal),
            0
        );
        ir.open_methods.insert(function);
        assert_eq!(
            generated_function_flags(&ir, function, Visibility::Public),
            22
        );
    }

    #[test]
    fn a_generated_function_is_described_by_its_published_names_and_declared_shape() {
        let mut ir = IrFile::default();
        let function = generated_function(&mut ir, false);
        let publication = IrGeneratedMemberPublication {
            metadata_scope: IrGeneratedFunctionMetadataScope::Additive,
            functions: vec![IrGeneratedFunctionPublication {
                function,
                parameter_identities: ["self", "output", "serialDesc"]
                    .map(crate::ir::IrParameterIdentity::producer_value)
                    .to_vec(),
                metadata: Some(IrGeneratedFunctionMetadata {
                    source_name: "write$Self".to_string(),
                    visibility: Visibility::Internal,
                }),
                debug: IrGeneratedDeclarationDebug::declaration_line(3),
            }],
        };

        let records = generated_functions(&ir, &publication);
        assert_eq!(records.len(), 1);
        let (generated, record) = &records[0];
        assert_eq!(*generated, function);
        assert_eq!(record.name, "write$Self");
        assert_eq!(
            record.params,
            vec![
                ("self".to_string(), Ty::obj("sample/Value")),
                ("output".to_string(), Ty::String),
                ("serialDesc".to_string(), Ty::Unit),
            ]
        );
        assert_eq!(record.ret, Ty::Unit);
        assert_eq!(record.flags, 32);
        assert!(!record.annotations.declares_annotations());
    }
}
