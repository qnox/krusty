//! A class's `@kotlin.Metadata`: its declaration record ([`crate::metadata::class_declarations`])
//! with how the JVM realizes each declaration ([`JvmClassSignatures`]) beside it.

use super::*;
use crate::metadata::class_builder::{
    FnMeta, JvmClassSignatures, JvmConstructorSignature, JvmFieldSignature, JvmFunctionSignature,
    JvmPropertySignature, PropMeta,
};
use crate::metadata::class_declarations::{
    ClassDeclarationRecord, ClassTailOptions, FunctionOrigin, PropertyOrigin,
};

/// Common class-shape admission shared by the writer and transitive value-class readability. Keeping
/// these kind/constructor bails in one predicate is correctness-critical: if the writer withholds a
/// value class but the transitive check independently admits it, a mentioning class publishes a type
/// a downstream compiler reads as an ordinary box.
pub(super) fn class_metadata_common_shape_admitted(_ir: &IrFile, c: &crate::ir::IrClass) -> bool {
    !(c.prop_ref.is_some()
        || c.func_ref.is_some()
        // A published secondary constructor is described from its recorded semantic parameter
        // identities. A malformed publication contract would advertise the wrong parameter list,
        // so the class declines instead. Unpublished target realizations carry no record.
        || c.secondary_ctors
            .iter()
            .any(|sc| sc.metadata_visibility.is_some() && sc.named_params.len() != sc.params.len())
        || (!c.has_primary_ctor
            && c.secondary_ctors.is_empty()
            && !c.is_interface
            && !c.is_enum)
        || (c.fields.len() as u32) < c.ctor_param_count)
}

/// The single admission predicate for a VALUE class's own metadata record. Both the class writer
/// and transitive value-class readability call it, so adding a new write-side bail cannot silently
/// let a different class describe the withheld value class downstream.
pub(super) fn value_class_metadata_shape_admitted(ir: &IrFile, c: &crate::ir::IrClass) -> bool {
    c.is_value
        && class_metadata_common_shape_admitted(ir, c)
        // A value class's ctors realize as mangled static `constructor-impl` overloads, which the
        // secondary-ctor record path does not model — keep declining that combination.
        && c.secondary_ctors.is_empty()
        && c.fields.len() == 1
        && c.fields[0].is_final()
        && !ir.has_value_param_ctor(&c.fq_name())
}

/// Compute a class's `@kotlin.Metadata` from its IR: the class's declaration record and its JVM
/// signatures. Returns `None` for a shape the record cannot describe yet, so that class emits no
/// `@Metadata`.
pub(super) fn build_class_metadata(
    ir: &IrFile,
    c: &crate::ir::IrClass,
    opts: &EmitOptions,
    env: &EmitEnv<'_>,
) -> Option<KotlinMetadata> {
    build_class_metadata_with_facts(
        ir,
        env.override_results,
        c,
        opts,
        env.local_delegated(),
        env.signature_symbols,
        env.run,
    )
}

pub(super) fn build_class_metadata_with_facts(
    ir: &IrFile,
    override_results: &crate::jvm::override_results::OverrideResults,
    c: &crate::ir::IrClass,
    opts: &EmitOptions,
    locals: &crate::jvm::property_references::local_delegated_properties::LocalDelegatedProperties,
    signature_symbols: &dyn crate::backend::BackendClassifierSource,
    run: &EmitRun,
) -> Option<KotlinMetadata> {
    use crate::metadata::class_builder::build_class;
    if is_coroutine_state_machine(c) {
        return Some(KotlinMetadata {
            k: 3,
            stamp: opts.metadata_stamp(),
            xi: synthetic_class_xi(if is_continuation_class(c) {
                SYNTHETIC_PROTECTED
            } else {
                SYNTHETIC_LOCAL
            }),
            d1: vec![],
            d2: vec![],
        });
    }
    // A class a plugin generates with no declaration behind it has a synthetic-class record, whose
    // `xi` carries no visibility, since no lowering of kotlinc's own made it.
    if ir.is_declarationless_class(c.fq_name_id()) {
        return Some(KotlinMetadata {
            k: 3,
            stamp: opts.metadata_stamp(),
            xi: super::metadata_policy::class_xi(ir, c),
            d1: vec![],
            d2: vec![],
        });
    }
    if !class_metadata_common_shape_admitted(ir, c) {
        return None;
    }
    if c.is_value && !value_class_metadata_shape_admitted(ir, c) {
        return None;
    }
    let record = ir
        .class_declarations
        .get(&c.fq_name_id())
        .expect("a described class has the declaration record built before JVM lowering")
        .as_ref()?;
    if mentions_undescribed_value_class(ir, c, record) {
        return None;
    }
    let signature_formatter = JvmSignatureFormatter::with_symbols(ir, signature_symbols, run);
    let realization = JvmClassRealization {
        ir,
        c,
        run,
        override_results,
        local_classifiers: unmapped_classifiers(ir),
    };
    let signatures = realization.signatures(record, opts, locals);
    let approximate_intersection = |ty| signature_formatter.declaration_approximation(ty);
    let inner_classifier = |classifier| signature_formatter.inner_classifier(classifier);
    // The JVM lists the primary constructor's annotations in its classfile order: the runtime-
    // visible ones first.
    let primary_ctor_annotations = crate::metadata::MetadataAnnotations::with_records(
        &c.primary_ctor_annotations,
        primary_ctor_annotations(c),
    );
    let secondary_ctors = record.secondary_constructors();
    // The JVM's local-class names decide which classes it lists as nested: an object in a
    // constructor's default argument is named, and listed, as a nested class of its owner.
    let nested_names = crate::metadata::class_declarations::nested_classifiers(ir, c);
    let nested = nested_names.iter().map(String::as_str).collect::<Vec<_>>();
    let enum_entries = record.enum_entries();
    let tail = record.tail(
        ClassTailOptions {
            param_assertions: opts.param_assertions,
            // kotlinc's `LanguageFeature.AnnotationsInMetadata` (since language level 2.4): an
            // older source-language level keeps the HAS_ANNOTATIONS flags but writes no records.
            // The physical metadata stamp is deliberately not consulted here.
            annotations_in_metadata: opts.annotations_in_metadata,
            // Kotlin 1.4 introduced JVM default methods without compatibility holders. Older
            // consumers must reject this metadata instead of assuming the legacy `$DefaultImpls`
            // realization, so kotlinc attaches a compiler-version requirement to every interface.
            compiler_version_requirement: (c.is_interface
                && opts.jvm_default == JvmDefaultMode::NoCompatibility)
                .then_some(
                    crate::metadata::version_requirements::VersionRequirement::compiler(1, 4, 0),
                ),
            intersection: Some(&approximate_intersection),
            inner_classifiers: &inner_classifier,
            primary_ctor_annotations: Some(&primary_ctor_annotations),
        },
        &secondary_ctors,
        &nested,
    );
    let classifier = record.classifier();
    let class_id = if tail.local_classifiers.contains(&classifier) {
        crate::metadata::class_id::ResolvedClassId::local(classifier)
    } else {
        match crate::metadata::class_id::ResolvedClassId::resolve(
            crate::libraries::ClassifierDeclarationKey::Ordinary(classifier),
            &|key| signature_symbols.classifier_declaration_name(key),
        ) {
            Ok(name) => name,
            Err(error) => {
                run.set_emit_error(format!("cannot encode class metadata name: {error}"));
                return None;
            }
        }
    };
    let declaration = record.declaration(&class_id, &tail, &enum_entries);
    let (d1_bytes, d2) = match build_class(
        declaration.class_id,
        declaration.ctor_params,
        declaration.props,
        declaration.methods,
        declaration.enum_entries,
        declaration.tail,
        &signatures,
    ) {
        Ok(metadata) => metadata,
        Err(error) => {
            run.set_emit_error(format!("cannot encode class metadata name: {error}"));
            return None;
        }
    };
    // d1 is the protobuf payload as one `char` per byte, in parts that each fit one `CONSTANT_Utf8`
    // entry (the constant pool writes them as modified UTF-8).
    let d1 = crate::metadata::encoding::bytes_to_strings(&d1_bytes);
    Some(KotlinMetadata {
        k: 1,
        stamp: opts.metadata_stamp(),
        xi: super::metadata_policy::class_xi(ir, c),
        d1,
        d2,
    })
}

/// Whether a member of `c` would describe itself in terms of a value class a downstream
/// compilation cannot READ as one (`value_class_is_readable`): it would see an ordinary class, cast
/// the carrier to the box and bind an instance accessor where kotlinc emits the static `-impl` — a
/// ClassCastException. Describing `Holder.make(): A` is only sound once `A` itself is described.
///
/// A described value-class-involved member is otherwise sound: the classpath value-class RETURN
/// model reports that the physical method already hands back the ERASED underlying, so a caller
/// that learns the Kotlin return `K` from `@Metadata` does not also emit kotlinc's boxed sequence
/// over a carrier. Round-tripped by `krusty_roundtrip_class_metadata_e2e`'s value-class cases and
/// pinned by the box corpus's `compileKotlinAgainstKotlin/inlineClasses/*` MODULE chains.
fn mentions_undescribed_value_class(
    ir: &IrFile,
    c: &crate::ir::IrClass,
    record: &ClassDeclarationRecord,
) -> bool {
    let undescribed = |t: &Ty| {
        t.non_null().obj_internal().is_some_and(|fq_name| {
            // Same-file and classpath declarations are in the unified lookup. A sibling source
            // declaration is deliberately not materialized into this file's IR, so the module-origin
            // subset is also positive identity for that one case; it is not a second underlying map.
            (crate::jvm::value_classes::is_boxed_value_class(ir, fq_name)
                || ir.module_source_value_classes.contains(&fq_name))
                && !value_class_is_readable(ir, fq_name)
        })
    };
    record.methods.iter().any(|function| {
        function
            .receiver
            .iter()
            .chain(function.params.iter().map(|(_, ty)| ty))
            .chain(std::iter::once(&function.ret))
            .any(undescribed)
    }) || c
        .properties
        .iter()
        .any(|p| p.getter_jvm_name.is_some() && undescribed(&p.ty))
}

/// The classifiers whose class id a reader cannot map back to their JVM name, so that a signature
/// mentioning one is recorded: the local ones, and in a script every class, since kotlinc gives
/// the script's class an id of its own.
fn unmapped_classifiers(ir: &IrFile) -> crate::fx_hash::HashSet<crate::types::TypeName> {
    let mut classifiers = crate::metadata::local_classifiers::names(ir);
    if ir.is_script() {
        classifiers.extend(ir.classes.iter().map(crate::ir::IrClass::fq_name_id));
    }
    classifiers
}

/// How the JVM realizes a class's declarations, as its `@Metadata` names them.
struct JvmClassRealization<'a> {
    ir: &'a IrFile,
    c: &'a crate::ir::IrClass,
    run: &'a EmitRun,
    override_results: &'a crate::jvm::override_results::OverrideResults,
    local_classifiers: crate::fx_hash::HashSet<crate::types::TypeName>,
}

impl<'a> JvmClassRealization<'a> {
    /// The signatures of every declaration `record` describes, parallel to its lists.
    fn signatures(
        &self,
        record: &ClassDeclarationRecord,
        opts: &'a EmitOptions,
        locals: &'a crate::jvm::property_references::local_delegated_properties::LocalDelegatedProperties,
    ) -> JvmClassSignatures<'a> {
        let c = self.c;
        JvmClassSignatures {
            module_name: opts.module_name.as_deref(),
            // `jvmClassFlags` describes the interface SHAPE this compilation produced, so it tracks
            // `-jvm-default` exactly: a consumer reads it to know whether method bodies live on the
            // interface and whether a `$DefaultImpls` compatibility copy exists.
            class_flags: c
                .is_interface
                .then(|| opts.jvm_default.interface_jvm_class_flags())
                .flatten(),
            local_properties: locals.of(c.fq_name_id()),
            plugin_metadata: Some(crate::plugins::class_metadata::PluginClassMetadata {
                plugins: &opts.plugins,
                ir: self.ir,
                class: c,
            }),
            // Kotlin annotation classes expose a language-level constructor in metadata, but their
            // classfile is an annotation interface and therefore has no `<init>` method.
            primary_constructor: (record.emit_primary_ctor && !c.is_annotation)
                .then(|| self.primary_constructor(record.ctor_params())),
            secondary_constructors: record
                .secondary_ordinals
                .iter()
                .map(|&ordinal| {
                    super::super::constructor_metadata::secondary_constructor_signature(
                        self.ir, c, ordinal,
                    )
                })
                .collect(),
            properties: record
                .prop_origins
                .iter()
                .zip(&record.props)
                .map(|(&origin, record)| self.property(origin, record))
                .collect(),
            functions: record
                .method_origins
                .iter()
                .zip(&record.methods)
                .map(|(&origin, function)| self.function(origin, function))
                .collect(),
        }
    }

    /// A backing field records its descriptor exactly when a reader cannot rebuild it from the
    /// property type (kotlinc's `requiresSignature`).
    fn requires_field_signature(&self, property: Ty, physical: &str) -> bool {
        super::super::metadata_method_signatures::requires_field_signature(
            property,
            physical,
            &self.local_classifiers,
        )
    }

    fn accessor_signature(&self, fid: u32) -> Option<(String, String)> {
        self.ir.functions.get(fid as usize).map(|function| {
            (
                function.name.clone(),
                ir_method_desc(&function.params, &function.ret),
            )
        })
    }

    fn function(&self, origin: FunctionOrigin, record: &FnMeta) -> JvmFunctionSignature {
        let (ir, c) = (self.ir, self.c);
        match origin {
            // A lowering that replaced the declaration's function (CPS, value classes) retargeted
            // its checked callable at the replacement.
            FunctionOrigin::Declared(callable) => {
                self.declared_function(ir.checked_callable_functions[&callable], record)
            }
            FunctionOrigin::Delegation(fid) => self.declared_function(fid, record),
            FunctionOrigin::DataMember(
                role @ (IrDataClassMemberRole::Component(_) | IrDataClassMemberRole::Copy),
            ) => data_class_member_realization(
                ir,
                c.fq_name_id(),
                role,
                record,
                &self.local_classifiers,
            )
            .unwrap_or_default(),
            // A data or boxed value class's `equals`/`hashCode`/`toString` keep `Any`'s derivable
            // signatures; an abstract value class's open ones have no method at all.
            FunctionOrigin::DataMember(_) | FunctionOrigin::OpenValueClassAny(_) => {
                JvmFunctionSignature::default()
            }
            FunctionOrigin::ValueClassAny(member) => value_class_override_metadata::signature(
                &crate::jvm::names::type_descriptor(c.fields[0].ty),
                member,
            ),
            FunctionOrigin::Generated(fid) => {
                super::super::generated_member_metadata::signature(ir, fid, record)
            }
        }
    }

    fn declared_function(&self, fid: u32, record: &FnMeta) -> JvmFunctionSignature {
        let f = &self.ir.functions[fid as usize];
        // Recorded exactly when a reader cannot rebuild the physical descriptor from the
        // declared types (kotlinc's `requiresFunctionSignature`).
        let physical = crate::jvm::names::method_descriptor(
            &f.params,
            self.override_results.physical_result(self.ir, fid),
        );
        let desc = (super::super::metadata_method_signatures::requires_function_signature(
            record.receiver,
            record
                .params
                .iter()
                .enumerate()
                .skip(record.context_count)
                .map(
                    |(index, (_, ty))| match record.vararg_index == Some(index) {
                        true => crate::metadata::vararg_recorded_type(*ty),
                        false => *ty,
                    },
                ),
            record.ret,
            &physical,
            &self.local_classifiers,
        ) || record.spellings.names_actualized_expect())
        .then_some(physical);
        JvmFunctionSignature {
            name: (record.name != f.name).then(|| f.name.clone()),
            desc,
        }
    }

    fn property(&self, origin: PropertyOrigin, record: &PropMeta) -> JvmPropertySignature {
        let (ir, c) = (self.ir, self.c);
        match origin {
            PropertyOrigin::Declared(index) => self.declared_property(index, record),
            // A class `const val` is its static field, read inline through its `ConstantValue`.
            PropertyOrigin::Constant(static_id) => JvmPropertySignature {
                field: Some(JvmFieldSignature::default()),
                synthetic_method: property_marker_signature(
                    ir,
                    c,
                    ir.statics[static_id as usize].source_order,
                ),
                ..JvmPropertySignature::default()
            },
            PropertyOrigin::MemberExtension(index) => {
                self.member_extension(index, &ir.member_ext_props[&c.fq_name_id()][index])
            }
            PropertyOrigin::CompanionBlock(index) => companion_blocks::property_signature(
                ir,
                c,
                ir.companion_blocks
                    .properties_of(c.fq_name_id())
                    .nth(index)
                    .expect("a block property record names its declaration"),
            ),
        }
    }

    fn declared_property(&self, property_index: usize, record: &PropMeta) -> JvmPropertySignature {
        let (ir, c) = (self.ir, self.c);
        let property = &c.properties[property_index];
        let desc = |t: Ty| crate::jvm::names::type_descriptor(t);
        let visibility = property.visibility;
        let backing = property
            .backing_field
            .and_then(|index| c.fields.get(index as usize));
        // A delegated property's storage is its `x$delegate` field.
        let delegate = property
            .delegate_field
            .and_then(|i| c.fields.get(i as usize));
        let hoisted = static_fields::hoisted_static_declaration_for(ir, c, property_index);
        // A delegated companion property's `x$delegate`, hoisted onto the outer class.
        let hoisted_delegate = static_fields::hoisted_delegate_static_for(ir, c, property_index);
        let (default_getter, default_setter) = accessor_jvm_names(c, &property.name);
        // A getter's descriptor is its physical one: a scalar getter result over a reference-returning
        // overridden property returns the wrapper (see `jvm::override_results`).
        let physical_getter = |fid: u32| {
            let function = &ir.functions[fid as usize];
            (
                function.name.clone(),
                ir_method_desc(
                    &function.params,
                    &self.override_results.physical_result(ir, fid),
                ),
            )
        };
        let ordinary_getter = property
            .getter
            .filter(|&fid| (fid as usize) < ir.functions.len())
            .map(physical_getter)
            .or_else(|| {
                backing.and_then(|field| {
                    // `@JvmField` suppresses the accessor pair entirely, so there is no
                    // synthesized getter to derive from the backing field — kotlinc records the
                    // field alone.
                    let boxed = self
                        .override_results
                        .boxes_member_property(c.fq_name, property_index as u32);
                    let result = if boxed {
                        Ty::nullable(property.ty)
                    } else {
                        declared_property_accessor_jvm(ir, property, field)
                    };
                    (!visibility.is_private() && !is_jvm_field(c, &property.name))
                        .then(|| (default_getter, format!("(){}", desc(result))))
                })
            });
        let getter = if c.is_annotation {
            let stored = crate::jvm::annotation_kclass::annotation_member_jvm_type(
                jvm_declared_ty(&property.ty),
            );
            Some((property.name.clone(), format!("(){}", desc(stored))))
        } else {
            ordinary_getter
        };
        let setter = property
            .setter
            .and_then(|fid| ir.functions.get(fid as usize))
            .map(|function| {
                (
                    function.name.clone(),
                    ir_method_desc(&function.params, &function.ret),
                )
            })
            .or_else(|| {
                backing.and_then(|field| {
                    (!visibility.is_private()
                        && property.is_var
                        && !property.has_private_default_setter()
                        && !is_jvm_field(c, &property.name))
                    .then(|| (default_setter, format!("({})V", desc(field.ty))))
                })
            });
        // An annotation class's and an interface's properties have no field; a companion property
        // hoisted onto its outer class is stored there.
        let has_field = !c.is_annotation
            && !c.is_interface
            && (backing.is_some()
                || delegate.is_some()
                || hoisted.is_some()
                || hoisted_delegate.is_some());
        let field = has_field.then(|| JvmFieldSignature {
            // The PHYSICAL field name when the JVM realization mangles it — an instance property
            // beside a same-named hoisted companion static (`result` → `result$1`).
            name: property
                .backing_field
                .filter(|_| backing.is_some())
                .or(property.delegate_field.filter(|_| delegate.is_some()))
                .map(|index| instance_field_jvm_name(ir, self.run, c, index as usize))
                .or_else(|| {
                    hoisted
                        .map(|(static_id, _)| static_id)
                        .or(hoisted_delegate)
                        .map(|static_id| ir.static_field_jvm_name(static_id).to_string())
                })
                .filter(|physical| *physical != property.name),
            desc: backing
                .map(|field| jvm_value_ty(&field.ty))
                .or(delegate.map(|field| jvm_value_ty(&field.ty)))
                .or_else(|| hoisted.map(|(_, storage)| jvm_value_ty(&storage.ty)))
                .or_else(|| {
                    hoisted_delegate
                        .map(|static_id| jvm_value_ty(&ir.statics[static_id as usize].ty))
                })
                .map(desc)
                .filter(|physical| {
                    record.spellings.ret.actualized_expect
                        || self.requires_field_signature(property.ty, physical)
                }),
        });
        JvmPropertySignature {
            getter,
            setter,
            field,
            // A property-targeted annotation lives on its synthetic marker method; the record here
            // is what connects the property to it (and the marker's FINAL name, which the
            // value-class pass may have mangled with the getter's).
            synthetic_method: property_marker_signature(ir, c, property.source_order),
            delegate_method: ir
                .class_id_by_name(c.fq_name)
                .and_then(|class| ir.jvm_member_delegate_method(class, property_index as u32))
                .map(|method| {
                    let function = &ir.functions[method as usize];
                    (
                        function.name.clone(),
                        ir_method_desc(&function.params, &function.ret),
                    )
                }),
            // kotlinc marks an interface companion's `@JvmField` property record: the backing
            // field was MOVED onto the interface itself.
            moved_from_interface_companion: companion_of_interface(ir, c)
                && static_fields::jvm_field_static_for(ir, c, property_index),
        }
    }

    fn member_extension(
        &self,
        property_index: usize,
        ext: &crate::ir::MemberExtProp,
    ) -> JvmPropertySignature {
        let (ir, c) = (self.ir, self.c);
        let ext_delegate = ext
            .delegate_field
            .and_then(|index| c.fields.get(index as usize).map(|field| (index, field)));
        JvmPropertySignature {
            getter: self.accessor_signature(ext.getter),
            setter: ext.setter.and_then(|fid| self.accessor_signature(fid)),
            field: ext_delegate.map(|(index, field)| JvmFieldSignature {
                name: Some(instance_field_jvm_name(ir, self.run, c, index as usize)),
                desc: Some(crate::jvm::names::type_descriptor(jvm_value_ty(&field.ty)))
                    .filter(|physical| self.requires_field_signature(ext.ty, physical)),
            }),
            synthetic_method: property_marker_signature(ir, c, ext.source_order),
            delegate_method: ir
                .class_id_by_name(c.fq_name)
                .and_then(|class| {
                    ir.jvm_member_extension_delegate_method(class, property_index as u32)
                })
                .map(|method| {
                    let function = &ir.functions[method as usize];
                    (
                        function.name.clone(),
                        ir_method_desc(&function.params, &function.ret),
                    )
                }),
            moved_from_interface_companion: false,
        }
    }

    /// The primary constructor's realization of the declared `params`.
    fn primary_constructor(&self, params: &[(String, Ty)]) -> JvmConstructorSignature {
        let (ir, c) = (self.ir, self.c);
        let desc = |t: Ty| crate::jvm::names::type_descriptor(t);
        // A local class takes its captures in their physical order.
        let arguments = super::capture_order::physical(ir, c, c.ctor_args.iter().collect());
        // A value class's primary constructor is realized as the static `constructor-impl` returning
        // the erased underlying, not `<init>`; its `@Metadata` signature records that.
        if c.is_value {
            return JvmConstructorSignature {
                name: "constructor-impl".into(),
                desc: format!("({0}){0}", desc(c.fields[0].ty)),
            };
        }
        // A value-class-parametered primary ctor's physical handle is the PUBLIC synthetic marker
        // ctor (`(…;Lkotlin/jvm/internal/DefaultConstructorMarker;)V`) — the private erased
        // `<init>` is not callable cross-class. The marker ctor spells EVERY parameter, an inner
        // class's leading outer instance included. A private primary is not hidden: kotlinc calls
        // its erased `<init>` directly and names that one, the marker ctor being only its accessor.
        if !c.is_enum
            && ir.vc_ctor_declared_params(c.fq_name_id()).is_some()
            && ir.ctor_visibilities.get(&c.fq_name_id()) != Some(&crate::types::Visibility::Private)
        {
            return JvmConstructorSignature::init(format!(
                "({}Lkotlin/jvm/internal/DefaultConstructorMarker;)V",
                arguments.iter().map(|arg| desc(arg.ty)).collect::<String>()
            ));
        }
        // The named declared parameters take the physical types the value-class pass erased them
        // to: a `Result` parameter requires no hidden constructor and is its `Object` carrier.
        let declared = arguments
            .iter()
            .filter(|arg| arg.name.is_some() && arg.declared_ty.is_some())
            .collect::<Vec<_>>();
        assert_eq!(
            declared.len(),
            params.len(),
            "the primary constructor record of {} names each declared constructor argument",
            c.fq_name.render()
        );
        // An `enum class`'s JVM constructor takes the two synthetic `Enum` parameters first, so its
        // recorded `JvmMethodSignature` is `(Ljava/lang/String;I…)V` — the metadata names the REAL
        // descriptor even though those parameters are not Kotlin-visible.
        JvmConstructorSignature::init(format!(
            "({}{}{})V",
            if !c.is_enum {
                ""
            } else {
                "Ljava/lang/String;I"
            },
            // The physical `<init>` leads with the UNNAMED lowering-added parameters — an inner
            // class's enclosing instance (`Llib/Outer;`) — which `params` (source parameters) never
            // carry. kotlinc's record spells them (`(Llib/Outer;Ljava/lang/String;I)V`); without them
            // a consumer's constructor call is one slot short.
            arguments
                .iter()
                .filter(|arg| arg.name.is_none())
                .map(|arg| desc(arg.ty))
                .collect::<String>(),
            declared.iter().map(|arg| desc(arg.ty)).collect::<String>()
        ))
    }
}
