//! The `Class` record: its fields and the order kotlinc interns their strings, the same for every
//! carrier.

use super::carrier::{AnnotationSite, ClassCarrier, ConstructorSlot};
use super::declarations::*;
use crate::metadata::builder::ContractTypeTable;
use crate::metadata::type_encoder::{
    encode_indexed_type_parameter, encode_metadata_type_parameter, encode_type,
    semantic_named_type_parameters, MetadataTypeParameter, StringTable, TypeParameterRef,
    TypeParameters,
};
use crate::metadata::version_requirements::{
    needs_inline_parameter_null_check, VersionRequirementTable, INLINE_PARAMETER_NULL_CHECK,
};
use crate::metadata::{property_flags, protobuf::Pb};
use crate::types::{Ty, TypeName, Visibility};

/// Append a value parameter's `annotation` records (f7) — one per applied annotation, in DECLARATION
/// order. `ValueParameter.flags` serializes BEFORE these and already carries `HAS_ANNOTATIONS` from
/// [`crate::metadata::MetadataAnnotations::declares_annotations`]; when `annotations_in_metadata`
/// is false the bit stays set and this appends nothing.
///
/// The records are appended AFTER the parameter's type, which is also the order kotlinc interns their
/// class ids in: a parameter's annotation descriptor lands in `d2` after the parameter's own name.
///
/// kotlinc's `LanguageFeature.AnnotationsInMetadata` (since 2.4) gates the records, not the flags:
/// at an older source-language level the `HAS_ANNOTATIONS` bit stays set but no record is appended.
pub(crate) fn append_param_annotations(
    st: &mut StringTable<'_>,
    vp: &mut Pb,
    annotations: Option<&crate::metadata::MetadataAnnotations>,
    annotations_in_metadata: bool,
    annotation_field: u32,
) {
    let Some(annotations) = annotations.filter(|_| annotations_in_metadata) else {
        return;
    };
    for annotation in annotations.records() {
        let encoded = crate::metadata::builder::annotation_pb(st, annotation);
        vp.field_message(annotation_field, &encoded);
    }
}

/// The `HAS_ANNOTATIONS` bit of a value parameter's `flags`.
pub(crate) fn param_annotation_flags(
    annotations: Option<&crate::metadata::MetadataAnnotations>,
) -> u64 {
    if annotations.is_some_and(crate::metadata::MetadataAnnotations::declares_annotations) {
        HAS_ANNOTATIONS
    } else {
        0
    }
}

/// A declaration visibility's `flags` bits, which a property and its accessors share.
fn visibility_flags(visibility: Visibility) -> u64 {
    match visibility {
        Visibility::Internal => 0,
        Visibility::Private => PRIVATE_VISIBILITY,
        Visibility::Protected => 4,
        Visibility::Public => 6,
        Visibility::PackagePrivate => {
            unreachable!(
                "package-private is a Java classpath visibility, never emitted to Kotlin metadata"
            )
        }
    }
}

pub(super) fn property_flags(prop: &PropMeta) -> u64 {
    (property_flags::DEFAULT & !property_flags::VISIBILITY_MASK)
        | visibility_flags(prop.visibility)
        | if prop.is_var {
            property_flags::IS_VAR | property_flags::HAS_SETTER
        } else {
            0
        }
        | if prop.has_constant {
            property_flags::HAS_CONSTANT
        } else {
            0
        }
        | if prop.is_const {
            property_flags::IS_CONST
        } else {
            0
        }
        | modality_bits(prop.modifiers.modality)
        | match prop.modifiers.member_kind {
            crate::ir::IrMemberKind::Declaration => 0,
            crate::ir::IrMemberKind::Delegation => property_flags::MEMBER_KIND_DELEGATION,
        }
        | if prop.modifiers.lateinit {
            property_flags::IS_LATEINIT
        } else {
            0
        }
        | if prop.modifiers.delegated {
            property_flags::IS_DELEGATED
        } else {
            0
        }
        | prop.return_value_status.metadata_value() << property_flags::RETURN_VALUE_STATUS_SHIFT
        | if prop.companion {
            property_flags::IS_STATIC
        } else {
            0
        }
}

/// kotlinc's name for a setter value parameter source did not name
/// (`SpecialNames.IMPLICIT_SET_PARAMETER`).
const IMPLICIT_SETTER_PARAMETER: &str = "<set-?>";
/// kotlinc's name for the unnamed value parameter of a setter source declares without a body
/// (`private set`).
const DECLARED_SETTER_PARAMETER: &str = "value";
/// `private` in the visibility bits (1-3) of a property or accessor flag word.
const PRIVATE_VISIBILITY: u64 = 2;

/// A `var`'s setter is not kotlinc's default accessor when source wrote its body, when it is
/// delegated, or when it narrows the property's visibility (`private set`). A bodiless `set`, even
/// an annotated one, stays the default accessor.
fn setter_is_not_default(p: &PropMeta) -> bool {
    p.is_var && (p.modifiers.declared_setter || p.setter_visibility != p.visibility)
}

/// `Property.flags` bits 4-5, which the accessor flag words share.
fn modality_bits(modality: crate::ir::IrPropertyModality) -> u64 {
    match modality {
        crate::ir::IrPropertyModality::Final => 0,
        crate::ir::IrPropertyModality::Open => property_flags::MODALITY_OPEN,
        crate::ir::IrPropertyModality::Abstract => property_flags::MODALITY_ABSTRACT,
    }
}

fn type_pb(st: &mut StringTable<'_>, t: Ty, type_parameters: &TypeParameters) -> Pb {
    encode_type(st, t, type_parameters)
        .unwrap_or_else(|error| panic!("invalid emitted metadata type: {error}"))
}

/// [`type_pb`] for a DECLARED type, carrying how source spelled it so a `typealias` becomes
/// `Type.abbreviated_type` (field 13).
fn type_pb_declared(
    st: &mut StringTable<'_>,
    t: Ty,
    spelled: &crate::spelling::Spelled,
    type_parameters: &TypeParameters,
) -> Pb {
    crate::metadata::type_encoder::encode_declared_type(st, t, spelled, type_parameters)
        .unwrap_or_else(|error| panic!("invalid emitted metadata type: {error}"))
}

/// [`type_pb`], but a `Some(id)` encodes the type as `Type.typeParameter` (f7) — a bare type parameter
/// (`val a: T`), which kotlinc records by INDEX rather than by the erased `java/lang/Object` class name.
fn type_pb_tp(
    st: &mut StringTable<'_>,
    t: Ty,
    tparam: Option<u32>,
    spelled: &crate::spelling::Spelled,
    type_parameters: &TypeParameters,
) -> Pb {
    match tparam {
        // A bare type parameter is recorded by INDEX and has no classifier to abbreviate.
        Some(index) => encode_indexed_type_parameter(st, t, index).map(|mut encoded| {
            if spelled.definitely_non_null {
                encoded.field_varint(1, 2); // Type.flags: DEFINITELY_NOT_NULL_TYPE
            }
            encoded
        }),
        None => {
            crate::metadata::type_encoder::encode_declared_type(st, t, spelled, type_parameters)
        }
    }
    .unwrap_or_else(|error| panic!("invalid emitted metadata type: {error}"))
}

/// Build one `Class.constructor` message: `flags` (f1, omitted if 0), value parameters (f2), and the
/// JvmProtoBuf constructor signature (f100, name `<init>` + `desc`).
struct CtorShape<'a> {
    params: &'a [(String, Ty)],
    /// How SOURCE spelled each parameter's type, parallel to `params` — a primary constructor's
    /// come from the class header (`class Holder(val p: Cargo)`). Empty leaves them unabbreviated,
    /// which is right for a SECONDARY constructor until its own spellings are threaded.
    param_spellings: &'a [crate::spelling::Spelled],
    /// Which of the declaration's constructors this is, for the carrier's extensions.
    slot: ConstructorSlot,
    flags: u64,
    param_defaults: &'a [bool],
    param_tparams: &'a [Option<u32>],
    /// User annotations on each parameter, parallel to `params` (a short list ⇒ the rest carry none).
    param_annotations: &'a [crate::metadata::MetadataAnnotations],
    /// Index into `params` of a `vararg` parameter — emits `ValueParameter.vararg_element_type` (f4),
    /// the only place ctor vararg-ness survives into metadata.
    vararg_index: Option<usize>,
    /// Applied annotations → `Constructor.annotation` (f3) + the `HAS_ANNOTATIONS` flag bit.
    annotations: &'a crate::metadata::MetadataAnnotations,
    /// kotlinc's `LanguageFeature.AnnotationsInMetadata` (since 2.4): `false` writes the
    /// `HAS_ANNOTATIONS` flags but none of the annotation records. Selected from finalized
    /// source-language settings, not inferred from the metadata stamp.
    annotations_in_metadata: bool,
}

fn build_ctor(
    st: &mut StringTable<'_>,
    carrier: &dyn ClassCarrier,
    shape: CtorShape<'_>,
    type_parameters: &TypeParameters,
) -> Pb {
    let mut ctor = Pb::new();
    // `HAS_ANNOTATIONS` (bit 0) follows from the declaration's annotations, exactly like a
    // function's — whether or not the records below are written. Setting
    // it
    // forces the flags field to be WRITTEN, so the proto default the caller was relying on has to be
    // materialized first: a public primary constructor carries 0 here precisely because 6 (visibility
    // PUBLIC) is the default and the field is omitted at that value — OR-ing bit 0 onto the 0 would
    // write 1 (visibility INTERNAL) where kotlinc writes 7.
    let flags = if !shape.annotations.declares_annotations() {
        shape.flags
    } else {
        (if shape.flags == 0 {
            PUBLIC_CTOR_FLAGS
        } else {
            shape.flags
        }) | HAS_ANNOTATIONS
    };
    if flags != 0 {
        ctor.field_varint(1, flags); // Constructor.flags = 1
    }
    for (i, (pname, pty)) in shape.params.iter().enumerate() {
        let mut vp = Pb::new();
        let annotations = shape.param_annotations.get(i);
        // `ValueParameter.flags` (f1) — DECLARES_DEFAULT_VALUE for a param that declares a default,
        // HAS_ANNOTATIONS when it carries annotations (the f7 records below are gated on the
        // source feature; the bit is not). Both precede the name, as kotlinc does.
        let flags = if shape.param_defaults.get(i).copied().unwrap_or(false) {
            DECLARES_DEFAULT_VALUE
        } else {
            0
        } | param_annotation_flags(annotations);
        if flags != 0 {
            vp.field_varint(1, flags);
        }
        vp.field_varint(2, st.local(pname) as u64); // ValueParameter.name = 2
                                                    // A `vararg` records `Array<out E>`, like the package-function writer.
        let element_spelling = shape
            .param_spellings
            .get(i)
            .unwrap_or(crate::spelling::Spelled::NONE);
        let (recorded, recorded_spelling) = if shape.vararg_index == Some(i) {
            crate::metadata::vararg_recorded_declaration(*pty, element_spelling)
        } else {
            (*pty, element_spelling.clone())
        };
        let ty = type_pb_tp(
            st,
            recorded,
            (shape.vararg_index != Some(i))
                .then(|| shape.param_tparams.get(i).copied().flatten())
                .flatten(),
            &recorded_spelling,
            type_parameters,
        );
        st.put_type(&mut vp, 3, 5, &ty); // ValueParameter.type(_id) = 3 / 5
                                         // A vararg parameter records its ELEMENT type as `vararg_element_type` (f4) — the declared
                                         // type stays the array, exactly as the package-function writer does.
        if shape.vararg_index == Some(i) {
            let elem = pty
                .array_elem()
                .or_else(|| pty.type_args().first().copied());
            if let Some(elem) = elem {
                let et = type_pb_tp(
                    st,
                    elem,
                    shape.param_tparams.get(i).copied().flatten(),
                    element_spelling,
                    type_parameters,
                );
                st.put_type(&mut vp, 4, 6, &et); // ValueParameter.vararg_element_type(_id) = 4 / 6
            }
        }
        append_param_annotations(
            st,
            &mut vp,
            annotations,
            shape.annotations_in_metadata,
            carrier.annotation_field(AnnotationSite::ValueParameter),
        );
        ctor.repeated_message(2, &vp); // Constructor.value_parameter = 2
    }
    // The carrier's extensions INTERN first (kotlinc's serializer writes the JVM signature before
    // folding the annotations, so `()V` precedes `Lp/Mark;` in d2) while the annotation SERIALIZES
    // first — the proto fields stay in ascending order.
    let extensions = carrier.constructor_extensions(st, shape.slot);
    // Constructor.annotation = 3 — after the value parameters, in kotlinc's ascending field order.
    // A disabled source feature keeps the `HAS_ANNOTATIONS` flag above but writes no records.
    let annotations: Vec<Pb> = if shape.annotations_in_metadata {
        shape
            .annotations
            .records()
            .iter()
            .map(|annotation| crate::metadata::builder::annotation_pb(st, annotation))
            .collect()
    } else {
        Vec::new()
    };
    let annotation_field = carrier.annotation_field(AnnotationSite::Constructor);
    for annotation in &annotations {
        ctor.repeated_message(annotation_field, annotation);
    }
    ctor.append(&extensions);
    ctor
}

/// f13 = an enum entry (`EnumEntry { name = f1, annotation = f2 }`). The entry's NAME interns
/// before its annotations, and each annotation's own strings follow it, kotlinc's `d2` order.
/// `annotations_in_metadata` gates the records (kotlinc's 2.4 `AnnotationsInMetadata` feature).
fn enum_entry_pb(
    st: &mut StringTable<'_>,
    entry: &EnumEntryMeta<'_>,
    annotations_in_metadata: bool,
    annotation_field: u32,
) -> Pb {
    let mut ee = Pb::new();
    ee.field_varint(1, st.local(entry.name) as u64);
    if annotations_in_metadata {
        for annotation in entry.annotations.records() {
            let encoded = crate::metadata::builder::annotation_pb(st, annotation);
            ee.repeated_message(annotation_field, &encoded);
        }
    }
    ee
}

/// Everything one `Class` record is built from.
pub(crate) struct ClassDeclaration<'a> {
    pub(crate) name: TypeName,
    pub(crate) ctor_params: &'a [(String, Ty)],
    pub(crate) props: &'a [PropMeta],
    pub(crate) methods: &'a [FnMeta],
    pub(crate) enum_entries: &'a [EnumEntryMeta<'a>],
    pub(crate) tail: &'a ClassTail<'a>,
}

/// The `Class` message for `declaration`, its strings interned into `st` in kotlinc's order.
pub(crate) fn class_message(
    st: &mut StringTable<'_>,
    declaration: &ClassDeclaration<'_>,
    carrier: &dyn ClassCarrier,
) -> Pb {
    let &ClassDeclaration {
        name: class_internal,
        ctor_params,
        props,
        methods,
        enum_entries,
        tail,
    } = declaration;
    // `HAS_ANNOTATIONS` (bit 0) is the declaration's own fact, set whether or not records follow.
    let class_flags = tail.flags | u64::from(tail.annotations.declares_annotations());
    let companion_name = tail.companion;
    let nested_class_names = tail.nested;
    let annotations_in_metadata = tail.annotations_in_metadata;
    // A KLIB class keeps its types in its own `TypeTable`, referenced by id.
    st.open_type_table();

    // STRINGS ARE INTERNED IN kotlinc's ORDER (fq_name, supertype, constructors, properties'
    // JVM signatures, functions, enum entries, then the companion + nested names LAST) even though the
    // proto writes fields in field-number order below — so the d2 indices match. Build every sub-message
    // first (interning), then assemble the `Class` message.

    // f3 = fq_name: the resolved class identity encoded directly into the metadata string table.
    let fq = st.class_id(class_internal);

    // f5 = typeParameter: `{ id, name }` per declared parameter, in order. kotlinc interns the names
    // right after the fq_name, before any member signature.
    assert_eq!(
        tail.type_param_bounds.len(),
        tail.type_params.len(),
        "metadata class type parameters require semantic identities"
    );
    let (reserved, numbered_on_use) = match tail.captured_type_params {
        CapturedTypeParameters::Reserved(parameters) => (parameters, &[][..]),
        CapturedTypeParameters::NumberedOnUse(parameters) => (&[][..], parameters),
    };
    let captured_count = reserved.len();
    let mut class_type_parameters = TypeParameters::classifier(
        captured_count + tail.type_params.len(),
        numbered_on_use.iter().cloned(),
    );
    for (index, semantic) in reserved.iter().enumerate() {
        class_type_parameters.insert(semantic.clone(), TypeParameterRef::Id(index as u64));
    }
    for (index, (source, parameter)) in tail
        .type_params
        .iter()
        .zip(tail.type_param_bounds)
        .enumerate()
    {
        let id = TypeParameterRef::Id((captured_count + index) as u64);
        class_type_parameters.insert(source.clone(), id.clone());
        class_type_parameters.insert(parameter.semantic_name.clone(), id);
    }
    // The class header (its type parameters' bounds and its supertypes) belongs to the class
    // itself, so like any declaration being written it names its own parameters
    // (`Type.type_parameter_name`); its members address them by table id.
    let mut class_header_type_parameters = class_type_parameters.clone();
    class_header_type_parameters.extend(semantic_named_type_parameters(
        tail.type_params.iter().map(String::as_str),
        tail.type_param_bounds
            .iter()
            .map(|parameter| parameter.semantic_name.as_str()),
    ));
    let tparam_msgs: Vec<Pb> = tail
        .type_param_bounds
        .iter()
        .enumerate()
        .map(|(i, parameter)| {
            encode_metadata_type_parameter(
                st,
                captured_count + i,
                &MetadataTypeParameter {
                    name: parameter.name.clone(),
                    reified: false,
                    variance: parameter.variance,
                    upper_bounds: parameter.bounds.iter().map(|(bound, _)| *bound).collect(),
                    upper_bound_spellings: tail
                        .spellings
                        .type_param_bounds
                        .get(i)
                        .cloned()
                        .unwrap_or_default(),
                },
                &class_header_type_parameters,
            )
            .unwrap_or_else(|error| panic!("invalid emitted metadata type parameter: {error}"))
        })
        .collect();

    // An enum lists its declared interfaces, then the implicit `Enum<E>`; a class without declared
    // supertypes lists `Any`.
    let mut supertype_msgs: Vec<Pb> = Vec::new();
    if tail.is_enum {
        for (index, supertype) in tail.supertypes.iter().enumerate() {
            if matches!(supertype, Ty::Obj(classifier, _) if *classifier == crate::types::wk::kotlin_enum())
            {
                continue;
            }
            supertype_msgs.push(type_pb_declared(
                st,
                *supertype,
                tail.supertype_spellings
                    .get(index)
                    .unwrap_or(crate::spelling::Spelled::NONE),
                &class_header_type_parameters,
            ));
        }
        supertype_msgs.push(type_pb(
            st,
            Ty::obj_args_name(
                crate::types::wk::kotlin_enum(),
                &[Ty::obj_name(class_internal)],
            ),
            &class_header_type_parameters,
        ));
    } else if tail.supertypes.is_empty() {
        supertype_msgs.push(type_pb(
            st,
            Ty::obj("kotlin/Any"),
            &class_header_type_parameters,
        ));
    } else {
        for (index, supertype) in tail.supertypes.iter().enumerate() {
            supertype_msgs.push(type_pb_declared(
                st,
                *supertype,
                tail.supertype_spellings
                    .get(index)
                    .unwrap_or(crate::spelling::Spelled::NONE),
                &class_header_type_parameters,
            ));
        }
    }

    // A KLIB references its supertypes by type-table id, taken as they are attached.
    let mut supertype_ids = Vec::new();
    supertype_msgs.retain(|supertype| match st.type_id(supertype) {
        Some(id) => {
            supertype_ids.push(u64::from(id));
            false
        }
        None => true,
    });

    // f8 = constructors: the primary (flags 0), then any secondary constructors — each interning in
    // order (kotlinc emits the ctor JVM name `<init>` explicitly, not omitted).
    let ctor_param_tparams = tail
        .ctor_param_tparams
        .iter()
        .map(|parameter| parameter.map(|index| index + captured_count as u32))
        .collect::<Vec<_>>();
    let mut ctor_msgs = if tail.emit_primary_ctor {
        vec![build_ctor(
            st,
            carrier,
            CtorShape {
                params: ctor_params,
                // A primary-constructor parameter's spelling is part of the CLASS HEADER.
                param_spellings: &tail.spellings.params,
                slot: ConstructorSlot::Primary,
                flags: tail.primary_ctor_flags,
                param_defaults: tail.ctor_param_defaults,
                param_tparams: &ctor_param_tparams,
                param_annotations: tail.ctor_param_annotations,
                vararg_index: tail.ctor_vararg_index,
                annotations: tail.primary_ctor_annotations,
                annotations_in_metadata,
            },
            &class_type_parameters.member(0).0,
        )]
    } else {
        Vec::new()
    };
    for (index, sc) in tail.secondary_ctors.iter().enumerate() {
        ctor_msgs.push(build_ctor(
            st,
            carrier,
            CtorShape {
                params: sc.params,
                param_spellings: sc.param_spellings,
                slot: ConstructorSlot::Secondary(index),
                flags: sc.flags,
                param_defaults: sc.param_defaults,
                param_tparams: &[],
                param_annotations: &[],
                vararg_index: sc.vararg_index,
                annotations: sc.annotations,
                annotations_in_metadata,
            },
            &class_type_parameters.member(0).0,
        ));
    }

    let build_prop = |st: &mut StringTable<'_>, index: usize| {
        let p = &props[index];
        let mut prop = Pb::new();
        // kotlinc's serializer names a type parameter the declaration being written owns
        // (`Type.type_parameter_name`) and addresses an enclosing class's by table id.
        let (mut property_type_parameters, first_own) =
            class_type_parameters.member(p.type_params.len());
        property_type_parameters.extend(semantic_named_type_parameters(
            p.type_params
                .iter()
                .map(|parameter| parameter.name.as_str()),
            p.type_params
                .iter()
                .map(|parameter| parameter.semantic_name.as_str()),
        ));
        let return_type = |st: &mut StringTable<'_>, type_parameters: &TypeParameters| {
            type_pb_tp(
                st,
                p.ty,
                p.tparam.map(|index| index + captured_count as u32),
                &p.spellings.ret,
                type_parameters,
            )
        };
        // The setter is a declaration of its own: the property's type parameters are not its own,
        // so its value parameter addresses them by table id.
        let (mut setter_type_parameters, _) = property_type_parameters.member(0);
        for (index, parameter) in p.type_params.iter().enumerate() {
            let id = TypeParameterRef::Id(first_own + index as u64);
            setter_type_parameters.insert(parameter.name.clone(), id.clone());
            setter_type_parameters.insert(parameter.semantic_name.clone(), id);
        }
        // kotlinc records the setter's value parameter exactly when the setter word is not the
        // default one (`Flags.IS_NOT_DEFAULT`), and serializes it before the property's own name,
        // so its strings come first in `d2`. An unnamed parameter is `value` on a source-declared
        // setter (`private set`) and `<set-?>` on a delegated property's generated one.
        let setter_parameter = setter_is_not_default(p).then(|| {
            let name = p
                .setter_parameter_name
                .as_deref()
                .unwrap_or(if p.modifiers.delegated {
                    IMPLICIT_SETTER_PARAMETER
                } else {
                    DECLARED_SETTER_PARAMETER
                });
            let annotations = Some(&p.accessor_annotations.setter_parameter);
            let mut parameter = Pb::new();
            let flags = param_annotation_flags(annotations);
            if flags != 0 {
                parameter.field_varint(1, flags); // ValueParameter.flags = 1
            }
            parameter.field_varint(2, st.local(name) as u64); // ValueParameter.name = 2
            let ty = return_type(st, &setter_type_parameters);
            st.put_type(&mut parameter, 3, 5, &ty); // ValueParameter.type(_id) = 3 / 5
            append_param_annotations(
                st,
                &mut parameter,
                annotations,
                annotations_in_metadata,
                carrier.annotation_field(AnnotationSite::ValueParameter),
            );
            parameter
        });
        // kotlinc interns the type before the type parameters, as at the top level.
        prop.field_varint(2, st.local(&p.name) as u64); // Property.name = 2
        let ty = return_type(st, &property_type_parameters);
        st.put_type(&mut prop, 3, 9, &ty); // Property.return_type(_id) = 3 / 9
        for (index, parameter) in p.type_params.iter().enumerate() {
            let id = first_own as usize + index;
            let parameter = encode_metadata_type_parameter(
                st,
                id,
                &MetadataTypeParameter {
                    name: parameter.name.clone(),
                    reified: false,
                    variance: parameter.variance,
                    upper_bounds: parameter.bounds.iter().map(|(bound, _)| *bound).collect(),
                    upper_bound_spellings: p
                        .spellings
                        .type_param_bounds
                        .get(index)
                        .cloned()
                        .unwrap_or_default(),
                },
                &property_type_parameters,
            )
            .unwrap_or_else(|error| panic!("invalid emitted property type parameter: {error}"));
            prop.repeated_message(4, &parameter);
        }
        if let Some(recv) = p.receiver {
            // Property.receiver_type = 5 — a member EXTENSION property's declared receiver;
            // its presence is what makes the record an extension.
            let rt = type_pb_declared(st, recv, &p.spellings.receiver, &property_type_parameters);
            st.put_type(&mut prop, 5, 10, &rt); // Property.receiver_type(_id) = 5 / 10
        }
        if let Some(parameter) = &setter_parameter {
            prop.field_message(6, parameter); // Property.setter_value_parameter = 6
        }
        let mut context_receiver_ids = Vec::new();
        for (name, kind, ty) in &p.context_params {
            // Kotlin keeps the type-only compatibility list for every context entry, including a
            // named context parameter that is also published below as `context_parameter`.
            let ty = type_pb_declared(
                st,
                *ty,
                crate::spelling::Spelled::NONE,
                &property_type_parameters,
            );
            match st.type_id(&ty) {
                Some(id) => context_receiver_ids.push(u64::from(id)),
                None => prop.repeated_message(12, &ty), // Property.context_receiver_type = 12
            }
            if *kind == crate::types::ContextParameterKind::LegacyReceiver {
                continue;
            }
            let name = if *kind == crate::types::ContextParameterKind::Anonymous {
                "<unused var>"
            } else {
                name
            };
            let mut parameter = Pb::new();
            parameter.field_varint(2, st.local(name) as u64); // ValueParameter.name = 2
            st.put_type(&mut parameter, 3, 5, &ty); // ValueParameter.type(_id) = 3 / 5
            prop.repeated_message(17, &parameter); // Property.context_parameter = 17
        }
        // Property.context_receiver_type_id = 13, packed.
        prop.field_packed_varints(13, &context_receiver_ids);
        // `HAS_ANNOTATIONS` (bit 0) follows from the property's applied annotations, on EITHER use
        // site: kotlinc sets it for a field-targeted annotation too, and keeps it at metadata
        // versions where the records below are gated off.
        let annotated =
            p.annotations.declares_annotations() || p.field_annotations.declares_annotations();
        let pflags = property_flags(p) | u64::from(annotated);
        // An accessor's flags word is emitted when it differs from the DEFAULT one, which kotlinc
        // derives from the PROPERTY (its `hasAnnotations` bit included). An annotated property whose
        // getter carries no annotation of its own therefore writes its plain accessor word out.
        //
        // That derivation is `Flags.getAccessorFlags(visibility, modality)` over the PROPERTY's own
        // word — the two share bits 1-5 — so a `protected`/`internal` declaration's accessors carry
        // ITS visibility, not the public default (`@Mark protected val` records getter_flags 4, and
        // an `internal` one records 0). An accessor source declares, or a delegated property's, is
        // not the default one and sets `isNotDefault`; a `private set` also narrows the setter's
        // visibility.
        let shared = pflags & (property_flags::VISIBILITY_MASK | property_flags::MODALITY_MASK);
        let default_accessor = shared | u64::from(annotated);
        let not_default = |declared: bool| {
            if declared {
                property_flags::ACCESSOR_IS_NOT_DEFAULT
            } else {
                0
            }
        };
        let accessors = &p.accessor_annotations;
        let getter_flags = shared
            | not_default(p.modifiers.declared_getter)
            | u64::from(accessors.getter.declares_annotations());
        if getter_flags != default_accessor {
            prop.field_varint(7, getter_flags); // Property.getter_flags = 7
        }
        // The setter word rides on the DECLARATION being a `var`, not on a JVM setter signature
        // being recorded: a `@JvmField var` has no setter method at all and still records the word.
        if p.is_var {
            let setter_flags = (shared & !property_flags::VISIBILITY_MASK)
                | visibility_flags(p.setter_visibility)
                | not_default(setter_is_not_default(p))
                | u64::from(accessors.setter.declares_annotations());
            if setter_flags != default_accessor {
                prop.field_varint(8, setter_flags); // Property.setter_flags = 8
            }
        }
        if pflags != property_flags::DEFAULT {
            prop.field_varint(11, pflags); // Property.flags = 11
        }
        // The carrier's extensions (the JVM signature) intern before the annotation records.
        let extensions = carrier.property_extensions(st, index);
        // Property.annotation = 14 / the backing field's = 34, both interning after the signature's
        // strings (kotlinc's serializer writes the JVM extension first). A disabled source feature
        // keeps the `HAS_ANNOTATIONS` flag above but writes no records.
        let records = |st: &mut StringTable<'_>,
                       annotations: &crate::metadata::MetadataAnnotations| {
            if annotations_in_metadata {
                annotations
                    .records()
                    .iter()
                    .map(|annotation| crate::metadata::builder::annotation_pb(st, annotation))
                    .collect()
            } else {
                Vec::new()
            }
        };
        let annotations: Vec<Pb> = records(st, &p.annotations);
        let getter_annotations = records(st, &accessors.getter);
        let setter_annotations = records(st, &accessors.setter);
        let field_annotations = records(st, &p.field_annotations);
        let fields = [
            AnnotationSite::Property,
            AnnotationSite::Getter,
            AnnotationSite::Setter,
            AnnotationSite::BackingField,
        ]
        .map(|site| carrier.annotation_field(site));
        for (field, records) in fields.into_iter().zip([
            &annotations,
            &getter_annotations,
            &setter_annotations,
            &field_annotations,
        ]) {
            for annotation in records {
                prop.repeated_message(field, annotation);
            }
        }
        prop.append(&extensions);
        let trailer = carrier.property_trailer(st, index);
        prop.append(&trailer);
        prop
    };

    // Member functions (name f2, return_type f3, value_parameter f6, flags f9; JVM sig derivable).
    let build_func = |st: &mut StringTable<'_>,
                      requirements: &mut VersionRequirementTable,
                      contract_types: &mut ContractTypeTable,
                      index: usize| {
        let m = &methods[index];
        let mut func = Pb::new();
        func.field_varint(2, st.local(&m.name) as u64);
        let (mut function_type_parameters, first_own) =
            class_type_parameters.member(m.type_params.len());
        assert_eq!(
            m.semantic_type_params.len(),
            m.type_params.len(),
            "metadata member type parameters require semantic identities"
        );
        // Own type parameters are named, an enclosing class's addressed by id; see `build_prop`.
        function_type_parameters.extend(semantic_named_type_parameters(
            m.type_params.iter().map(String::as_str),
            m.semantic_type_params.iter().map(String::as_str),
        ));
        // kotlinc interns the return type before the type parameters, as at the top level.
        let ret = crate::metadata::type_encoder::encode_declared_type(
            st,
            m.ret,
            &m.spellings.ret,
            &function_type_parameters,
        )
        .unwrap_or_else(|error| {
            panic!(
                "invalid emitted metadata return type for '{class_internal}.{}': {error}",
                m.name
            )
        });
        st.put_type(&mut func, 3, 7, &ret); // Function.return_type(_id) = 3 / 7
        for (index, name) in m.type_params.iter().enumerate() {
            let id = first_own as usize + index;
            let parameter = encode_metadata_type_parameter(
                st,
                id,
                &MetadataTypeParameter {
                    name: name.clone(),
                    reified: false,
                    variance: crate::types::TypeVariance::Invariant,
                    upper_bounds: m.type_param_bounds.get(index).cloned().unwrap_or_default(),
                    upper_bound_spellings: m
                        .spellings
                        .type_param_bounds
                        .get(index)
                        .cloned()
                        .unwrap_or_default(),
                },
                &function_type_parameters,
            )
            .unwrap_or_else(|error| panic!("invalid emitted metadata type parameter: {error}"));
            func.repeated_message(4, &parameter);
        }
        if let Some(recv) = m.receiver {
            // Function.receiver_type = 5 — a MEMBER EXTENSION's receiver, restored from the
            // physical `params[0]` realization so consumers see the LOGICAL shape.
            let rt = crate::metadata::type_encoder::encode_declared_type(
                st,
                recv,
                &m.spellings.receiver,
                &function_type_parameters,
            )
            .unwrap_or_else(|error| {
                panic!(
                    "invalid emitted metadata receiver for '{class_internal}.{}': {error}",
                    m.name
                )
            });
            st.put_type(&mut func, 5, 8, &rt); // Function.receiver_type(_id) = 5 / 8
        }
        assert_eq!(
            m.context_parameter_kinds.len(),
            m.context_count,
            "metadata member context roles must match the leading parameter prefix"
        );
        let mut context_receiver_ids = Vec::new();
        for (i, (pname, pty)) in m.params.iter().enumerate() {
            let context_kind = m.context_parameter_kinds.get(i);
            if context_kind.is_some() {
                // Kotlin keeps the type-only compatibility list for named context parameters too.
                let ty =
                    type_pb_declared(st, *pty, m.spellings.param(i), &function_type_parameters);
                match st.type_id(&ty) {
                    Some(id) => context_receiver_ids.push(u64::from(id)),
                    None => func.repeated_message(10, &ty), // Function.context_receiver_type = 10
                }
            }
            if context_kind == Some(&crate::types::ContextParameterKind::LegacyReceiver) {
                continue;
            }
            let mut vp = Pb::new();
            let annotations = m.param_annotations.get(i);
            // `ValueParameter.flags` (f1): DECLARES_DEFAULT_VALUE for a defaulted parameter,
            // HAS_ANNOTATIONS when it carries annotations (the f7 records below are gated on the
            // source feature; the bit is not). Both precede the name.
            let declared = m.param_modifiers.get(i).copied().unwrap_or_default();
            let flags = if m.params_have_defaults {
                DECLARES_DEFAULT_VALUE
            } else {
                0
            } | declared.flags()
                | param_annotation_flags(annotations);
            if flags != 0 {
                vp.field_varint(1, flags); // ValueParameter.flags = 1
            }
            vp.field_varint(2, st.local(pname) as u64);
            // A `vararg` parameter is SPELLED as its element but RECORDED as the array; see the
            // package-function writer for why the spelling is lifted rather than applied.
            let (declared_ty, declared_spelling) = if m.vararg_index == Some(i) {
                crate::metadata::vararg_recorded_declaration(*pty, m.spellings.param(i))
            } else {
                (*pty, m.spellings.param(i).clone())
            };
            let ty = crate::metadata::type_encoder::encode_declared_type(
                st,
                declared_ty,
                &declared_spelling,
                &function_type_parameters,
            )
            .unwrap_or_else(
                    |error| {
                        panic!(
                            "invalid emitted metadata parameter '{pname}' for '{class_internal}.{}': {error}",
                            m.name
                        )
                    },
                );
            st.put_type(&mut vp, 3, 5, &ty); // ValueParameter.type(_id) = 3 / 5
            if m.vararg_index == Some(i) {
                // ValueParameter.vararg_element_type = 4 — the ELEMENT next to the array type.
                let elem = pty
                    .array_elem()
                    .or_else(|| pty.type_args().first().copied());
                if let Some(elem) = elem {
                    let et = crate::metadata::type_encoder::encode_declared_type(
                        st,
                        elem,
                        m.spellings.param(i),
                        &function_type_parameters,
                    )
                    .unwrap_or_else(|error| {
                        panic!(
                            "invalid emitted metadata vararg element for \
                                     '{class_internal}.{}': {error}",
                            m.name
                        )
                    });
                    st.put_type(&mut vp, 4, 6, &et); // ValueParameter.vararg_element_type(_id) = 4 / 6
                }
            }
            // f7 AFTER the type and vararg element: kotlinc interns a parameter's annotation class
            // id following that parameter's own name and type.
            append_param_annotations(
                st,
                &mut vp,
                annotations,
                annotations_in_metadata,
                carrier.annotation_field(AnnotationSite::ValueParameter),
            );
            if i < m.context_count {
                // Leading context parameters → Function.context_parameter = 13 (filled implicitly
                // by callers), NOT the positional value_parameter list.
                func.repeated_message(13, &vp);
            } else {
                func.repeated_message(6, &vp); // Function.value_parameter = 6
            }
        }
        // Function.context_receiver_type_id = 11, packed.
        func.field_packed_varints(11, &context_receiver_ids);
        // An annotated declaration sets `HAS_ANNOTATIONS` (bit 0) on top of whatever the caller
        // derived — the bit is a function OF the records below, never an independent input.
        let flags = m.flags | u64::from(m.annotations.declares_annotations());
        // Omitted at the public-final-declaration default, exactly like `Class.flags`.
        if flags != DEFAULT_FUNCTION_FLAGS {
            func.field_varint(9, flags); // Function.flags = 9
        }
        // The `JvmMethodSignature` (f100) INTERNS before the annotations even though it SERIALIZES
        // after them — kotlinc's serializer writes the extension first, so a suspend member's
        // descriptor precedes `Lp/Mark;` in d2. Build both, then append in field order.
        // Each half is independent, like kotlinc's serializer: the NAME rides along only when a
        // realization renamed the method (value-class mangle, `@JvmName`), the DESC only when the
        // proto types alone don't pin the JVM descriptor (erasure, boxed nullable primitive). A
        // mangled member with a derivable descriptor (`f(): V?` — nullable value classes box) is
        // name-only; a renamed-and-erased one carries both.
        // The declared contract (Function.contract = 32) interns where a package function's does:
        // before the signature, or after the annotations of an annotated member.
        let mut contract = |st: &mut StringTable<'_>| {
            m.contract.as_deref().map(|contract| {
                crate::metadata::builder::contract_pb(
                    st,
                    contract_types,
                    contract,
                    &function_type_parameters,
                )
            })
        };
        let early_contract = m
            .annotations
            .records()
            .is_empty()
            .then(|| contract(st))
            .flatten();
        let extensions = carrier.function_extensions(st, index);
        // Function.annotation = 12 — the applied annotations, each an `Annotation.id` (f1) naming the
        // annotation class through the string table's `DESC_TO_CLASS_ID` form, plus its arguments.
        // A disabled source feature keeps the `HAS_ANNOTATIONS` flag above but writes no records.
        let annotations: Vec<Pb> = if annotations_in_metadata {
            m.annotations
                .records()
                .iter()
                .map(|annotation| crate::metadata::builder::annotation_pb(st, annotation))
                .collect()
        } else {
            Vec::new()
        };
        let annotation_field = carrier.annotation_field(AnnotationSite::Function);
        for annotation in &annotations {
            func.repeated_message(annotation_field, annotation);
        }
        let contract = early_contract.or_else(|| contract(st));
        // Function.version_requirement = 31: an index into this class's requirement table.
        if needs_inline_parameter_null_check(
            flags,
            m.has_function_typed_parameter,
            tail.param_assertions,
        ) {
            func.field_varint(31, requirements.index(INLINE_PARAMETER_NULL_CHECK));
        }
        if let Some(contract) = &contract {
            func.field_message(32, contract); // Function.contract = 32
        }
        func.append(&extensions);
        let trailer = carrier.function_trailer(st, m);
        func.append(&trailer);
        func
    };
    // Members add their requirements in serialization order; the class's own follow them.
    let mut requirements = VersionRequirementTable::default();
    let mut contract_types = ContractTypeTable::default();

    // kotlinc adds each record to its list as it builds it, so a list serializes in build order,
    // not in the caller's index order: a member extension property declared before a plain one
    // stays first. Each slot keeps its build sequence number beside the record.
    let mut built = 0..;
    let mut prop_msgs: Vec<Option<(usize, Pb)>> = (0..props.len()).map(|_| None).collect();
    let mut func_msgs: Vec<Option<(usize, Pb)>> = (0..methods.len()).map(|_| None).collect();
    let mut alias_msgs: Vec<Option<(usize, Pb)>> =
        (0..tail.type_aliases.len()).map(|_| None).collect();
    let mut enum_msgs: Vec<Option<Pb>> = (0..enum_entries.len()).map(|_| None).collect();
    for member in tail.member_order {
        match *member {
            ClassMemberOrder::Property(index)
                if index < props.len() && prop_msgs[index].is_none() =>
            {
                prop_msgs[index] = Some((built.next().expect("unbounded"), build_prop(st, index)));
            }
            ClassMemberOrder::Function(index)
                if index < methods.len() && func_msgs[index].is_none() =>
            {
                func_msgs[index] = Some((
                    built.next().expect("unbounded"),
                    build_func(st, &mut requirements, &mut contract_types, index),
                ));
            }
            ClassMemberOrder::TypeAlias(index)
                if index < tail.type_aliases.len() && alias_msgs[index].is_none() =>
            {
                alias_msgs[index] = Some((
                    built.next().expect("unbounded"),
                    crate::metadata::builder::type_alias_pb(st, &tail.type_aliases[index]),
                ));
            }
            ClassMemberOrder::EnumEntry(index)
                if index < enum_entries.len() && enum_msgs[index].is_none() =>
            {
                enum_msgs[index] = Some(enum_entry_pb(
                    st,
                    &enum_entries[index],
                    annotations_in_metadata,
                    carrier.annotation_field(AnnotationSite::EnumEntry),
                ));
            }
            _ => {}
        }
    }
    // Unscheduled records are compiler/plugin synthetics or callers using the legacy/default shape.
    // Preserve their established property-then-function order after all explicitly ordered members.
    for index in 0..props.len() {
        if prop_msgs[index].is_none() {
            prop_msgs[index] = Some((built.next().expect("unbounded"), build_prop(st, index)));
        }
    }
    for index in 0..methods.len() {
        if func_msgs[index].is_none() {
            func_msgs[index] = Some((
                built.next().expect("unbounded"),
                build_func(st, &mut requirements, &mut contract_types, index),
            ));
        }
    }
    for (index, alias) in tail.type_aliases.iter().enumerate() {
        if alias_msgs[index].is_none() {
            alias_msgs[index] = Some((
                built.next().expect("unbounded"),
                crate::metadata::builder::type_alias_pb(st, alias),
            ));
        }
    }
    let prop_msgs = in_build_order(prop_msgs, "property");
    let func_msgs = in_build_order(func_msgs, "function");
    let alias_msgs = in_build_order(alias_msgs, "type-alias");

    // Entries the caller did not schedule among the members intern after them, in entry order.
    let enum_msgs: Vec<Pb> = enum_msgs
        .into_iter()
        .zip(enum_entries)
        .map(|(message, entry)| {
            message.unwrap_or_else(|| {
                enum_entry_pb(
                    st,
                    entry,
                    annotations_in_metadata,
                    carrier.annotation_field(AnnotationSite::EnumEntry),
                )
            })
        })
        .collect();

    // A `@JvmInline value class`'s underlying property name + type (`Class` f17/f18). Interned with the
    // members (before the companion/nested tail) so the d2 order matches kotlinc.
    // A KLIB references the type by id (19) instead of carrying it inline (18).
    let inline_underlying: Option<(u32, Option<Pb>)> = tail.inline_underlying.map(|(name, ty)| {
        let name = st.local(name);
        let ty = ty.map(|ty| {
            let encoded = type_pb(st, ty, &class_header_type_parameters);
            let mut field = Pb::new();
            st.put_type(&mut field, 18, 19, &encoded);
            field
        });
        (name, ty)
    });

    // Nested + companion class names intern LAST (kotlinc's d2 places them after all members) —
    // NESTED names first, then the companion's, even though the companionObjectName FIELD serializes
    // before the nested list (kotlinc registers the strings in that order).
    let nested_idxs: Vec<u32> = nested_class_names.iter().map(|n| st.local(n)).collect();
    let companion_idx = companion_name.map(|c| st.local(c));
    // Sealed subclass IDs precede the module name in kotlinc's string table.
    let sealed_idxs: Vec<u32> = tail
        .sealed_subclasses
        .iter()
        .map(|&subclass| st.class_id(subclass))
        .collect();
    // The carrier's class extensions (the JVM module name, then its local delegated properties)
    // intern after every structural string and before the annotations.
    let class_extensions = carrier.class_extensions(st, &class_type_parameters);
    // Class.annotation = f25. kotlinc interns the annotation strings LAST of all — after nested +
    // companion names, sealed subclass ids, and the module name (measured on 2.4.10: an annotated
    // class under `-module-name` puts the module string BEFORE the annotation descriptor) — even
    // though the `annotation` FIELD serializes before all of those. A disabled source feature
    // (`annotations_in_metadata` = false) writes no records and interns nothing here.
    let annotation_msgs: Vec<Pb> = if annotations_in_metadata {
        tail.annotations
            .records()
            .iter()
            .map(|annotation| crate::metadata::builder::annotation_pb(st, annotation))
            .collect()
    } else {
        Vec::new()
    };

    // Assemble the `Class` message in FIELD order: f1 flags, f3 fq_name, f4 companionObjectName,
    // f6 supertype, f7 nestedClassName (packed repeated int32), f8 ctors, f9 functions, f10 properties,
    // f13 enum entries.
    let mut class = Pb::new();
    // kotlinc writes `flags` only when it differs from the public-final-class default.
    if class_flags != DEFAULT_CLASS_FLAGS {
        class.field_varint(1, class_flags);
    }
    class.field_varint(3, fq as u64);
    for tp in &tparam_msgs {
        class.repeated_message(5, tp); // Class.type_parameter = 5
    }
    if let Some(ci) = companion_idx {
        class.field_varint(4, ci as u64); // Class.companion_object_name = 4
    }
    for supertype in &supertype_msgs {
        class.repeated_message(6, supertype); // Class.supertype = 6 (repeated)
    }
    class.field_packed_varints(2, &supertype_ids); // Class.supertype_id = 2, packed
    if !nested_idxs.is_empty() {
        let mut packed = Pb::new();
        for &n in &nested_idxs {
            packed.varint(n as u64);
        }
        class.field_bytes(7, packed.as_bytes()); // Class.nested_class_name = 7 (packed)
    }
    for ctor in &ctor_msgs {
        class.repeated_message(8, ctor); // Class.constructor = 8
    }
    for func in &func_msgs {
        class.repeated_message(9, func); // Class.function = 9
    }
    for prop in &prop_msgs {
        class.repeated_message(10, prop); // Class.property = 10
    }
    for alias in &alias_msgs {
        class.repeated_message(11, alias); // Class.type_alias = 11
    }
    for ee in &enum_msgs {
        class.repeated_message(13, ee); // Class.enum_entry = 13
    }
    if !sealed_idxs.is_empty() {
        let mut packed = Pb::new();
        for &idx in &sealed_idxs {
            packed.varint(idx as u64);
        }
        class.field_bytes(16, packed.as_bytes());
    }
    if let Some((name_id, ty_pb)) = &inline_underlying {
        class.field_varint(17, *name_id as u64); // Class.inlineClassUnderlyingPropertyName = 17
        if let Some(field) = ty_pb {
            // Class.inlineClassUnderlyingType(_id) = 18 / 19, already framed.
            class.append(field);
        }
    }
    let annotation_field = carrier.annotation_field(AnnotationSite::Class);
    for annotation in &annotation_msgs {
        class.repeated_message(annotation_field, annotation);
    }
    if let Some(requirement) = tail.compiler_version_requirement {
        // Class.versionRequirement (f31) indexes Class.versionRequirementTable (f32).
        class.field_varint(31, requirements.index(requirement));
    }
    // Class.type_table = 30: a KLIB class's own types, or the types `@Metadata` contracts name.
    if let Some(table) = st.close_type_table().or_else(|| contract_types.encode()) {
        class.field_message(30, &table);
    }
    if let Some(table) = requirements.encode() {
        class.field_message(32, &table); // Class.versionRequirementTable = 32
    }
    class.append(&class_extensions);
    let trailer = carrier.class_trailer(st);
    class.append(&trailer);

    class.canonical()
}

/// Build the `(d1, d2)` payload for an ANONYMOUS class (`object : P2 {}` inside a function):
/// kotlinc's record is `Class { flags = LOCAL visibility (10), fq_name = <raw internal, marked
/// localName in the string table>, supertype* }` — no members, no constructor record.

/// A member list in the order its records were built, which is the order kotlinc serializes it.
fn in_build_order(slots: Vec<Option<(usize, Pb)>>, kind: &str) -> Vec<Pb> {
    let mut built: Vec<(usize, Pb)> = slots
        .into_iter()
        .map(|slot| slot.unwrap_or_else(|| panic!("every {kind} metadata record is built")))
        .collect();
    built.sort_by_key(|(sequence, _)| *sequence);
    built.into_iter().map(|(_, record)| record).collect()
}
