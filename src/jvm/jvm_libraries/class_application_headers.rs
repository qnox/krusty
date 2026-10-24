//! Complete generic application layouts from declaration headers, without member publication.

use super::generic_signatures::{
    capture_enclosing_type_parameters, formal_inline_bounds, hydrate_formal_bounds,
    parse_class_gsig,
};
use super::JvmLibraries;
use crate::fx_hash::HashSet;
use crate::jvm::classreader::{ClassInfo, ACC_STATIC};
use crate::types::{type_name, Ty, TypeName, TypeParameters, TypeVariance};

#[derive(Debug)]
pub(super) struct ClassApplicationHeader {
    pub(super) own_count: usize,
    pub(super) parameters: TypeParameters<Vec<Vec<Ty>>>,
    pub(super) outer_instance: Option<TypeName>,
}

struct BuiltinParameters {
    identity: TypeName,
    parameters: TypeParameters<Vec<Vec<Ty>>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ClassHeaderError {
    MissingHeader(TypeName),
    CyclicOwners(TypeName),
    IdentityMismatch(TypeName),
    InvalidSignature(TypeName),
    InvalidFormals(TypeName),
    InvalidNesting(TypeName),
    MissingBuiltinLayout(TypeName),
    MissingFormalScope(TypeName),
}

impl JvmLibraries {
    pub(super) fn class_application_header(
        &self,
        identity: TypeName,
    ) -> Result<ClassApplicationHeader, ClassHeaderError> {
        read_header(self, identity, &mut HashSet::default())
    }
}

fn read_header(
    libraries: &JvmLibraries,
    identity: TypeName,
    active: &mut HashSet<TypeName>,
) -> Result<ClassApplicationHeader, ClassHeaderError> {
    let physical = crate::jvm::jvm_class_map::to_jvm_type_name(identity);
    if !active.insert(physical) {
        return Err(ClassHeaderError::CyclicOwners(identity));
    }
    let result = read_entered_header(libraries, identity, active);
    active.remove(&physical);
    result
}

fn read_entered_header(
    libraries: &JvmLibraries,
    identity: TypeName,
    active: &mut HashSet<TypeName>,
) -> Result<ClassApplicationHeader, ClassHeaderError> {
    let Some(class) = libraries.cp.find_name(identity) else {
        return classless_builtin(libraries, identity);
    };
    if class.this_class != crate::jvm::jvm_class_map::to_jvm_type_name(identity) {
        return Err(ClassHeaderError::IdentityMismatch(identity));
    }
    let mut own = own_parameters(libraries, identity, &class)?;
    let own_count = own.type_params().len();
    let outer_instance = enclosing_instance(identity, &class)?;
    let parameters = if let Some(outer) = outer_instance {
        let enclosing = read_header(libraries, outer, active)?;
        let enclosing_bounds = formal_inline_bounds(
            enclosing.parameters.type_params(),
            enclosing.parameters.type_param_bounds(),
            &crate::fx_hash::HashMap::default(),
        );
        let names = own.type_params().clone();
        own.map_bounds(|bounds| {
            hydrate_formal_bounds(&names, bounds, &enclosing_bounds);
        });
        capture_enclosing_type_parameters(identity, &own, &enclosing.parameters)
    } else {
        own
    };
    validate_parameters(identity, &parameters)?;
    Ok(ClassApplicationHeader {
        own_count,
        parameters,
        outer_instance,
    })
}

fn enclosing_instance(
    identity: TypeName,
    class: &ClassInfo,
) -> Result<Option<TypeName>, ClassHeaderError> {
    let outer = match class.inner_class_self() {
        Some(entry) if entry.access & ACC_STATIC == 0 => entry.outer.as_deref().map(type_name),
        _ => None,
    };
    if class.meta.class_visibility.is_some() && class.meta.is_inner != outer.is_some() {
        return Err(ClassHeaderError::InvalidNesting(identity));
    }
    Ok(outer)
}

fn own_parameters(
    libraries: &JvmLibraries,
    identity: TypeName,
    class: &ClassInfo,
) -> Result<TypeParameters<Vec<Vec<Ty>>>, ClassHeaderError> {
    if class.meta.class_visibility.is_some() {
        return Ok(class.meta.class_type_parameters.clone());
    }
    if class.meta.is_present() {
        return Err(ClassHeaderError::InvalidFormals(identity));
    }
    if let Some(builtin) = builtin_parameters(libraries, identity)? {
        return Ok(builtin.parameters);
    }
    let Some(signature) = class.signature.as_deref() else {
        return Ok(TypeParameters::default());
    };
    let (names, bounds, supers) =
        parse_class_gsig(signature).ok_or(ClassHeaderError::InvalidSignature(identity))?;
    if supers.is_empty() || names.len() != bounds.len() {
        return Err(ClassHeaderError::InvalidSignature(identity));
    }
    let variances = vec![TypeVariance::Invariant; names.len()];
    Ok(TypeParameters::new(names, bounds, variances))
}

fn builtin_parameters(
    libraries: &JvmLibraries,
    identity: TypeName,
) -> Result<Option<BuiltinParameters>, ClassHeaderError> {
    let physical_mapping = crate::jvm::jvm_class_map::jvm_to_kotlin_builtin_metadata_name(identity);
    let query = physical_mapping.unwrap_or(identity);
    let Some(canonical) = libraries.cp.builtin_classifier_name(query) else {
        return Ok(None);
    };
    let signature = libraries
        .cp
        .builtin_class_signature_name(canonical)
        .ok_or(ClassHeaderError::MissingBuiltinLayout(identity))?;
    // Physical Java classifiers keep invariant formals; their Kotlin views may declare variance.
    // This matches Classpath::builtin_class_variances_name's physical-request policy.
    let variances = if physical_mapping.is_some() {
        vec![TypeVariance::Invariant; signature.formals.len()]
    } else {
        libraries
            .cp
            .builtin_class_variances_name(canonical)
            .ok_or(ClassHeaderError::MissingBuiltinLayout(identity))?
    };
    if signature.formals.len() != signature.formal_bounds.len()
        || signature.formals.len() != variances.len()
    {
        return Err(ClassHeaderError::InvalidFormals(identity));
    }
    Ok(Some(BuiltinParameters {
        identity: canonical,
        parameters: TypeParameters::new(signature.formals, signature.formal_bounds, variances),
    }))
}

fn classless_builtin(
    libraries: &JvmLibraries,
    identity: TypeName,
) -> Result<ClassApplicationHeader, ClassHeaderError> {
    let builtin = builtin_parameters(libraries, identity)?
        .ok_or(ClassHeaderError::MissingHeader(identity))?;
    let (_, _, nested, access) = libraries
        .cp
        .builtin_classifier_shape_name(builtin.identity)
        .ok_or(ClassHeaderError::MissingBuiltinLayout(identity))?;
    // A classless inner builtin needs an authenticated enclosing layout. A name separator does
    // not provide that evidence; static/non-nested declarations capture no enclosing formals.
    if nested && access & ACC_STATIC == 0 {
        return Err(ClassHeaderError::MissingBuiltinLayout(identity));
    }
    let parameters = builtin.parameters;
    validate_parameters(identity, &parameters)?;
    Ok(ClassApplicationHeader {
        own_count: parameters.type_params().len(),
        parameters,
        outer_instance: None,
    })
}

fn validate_parameters(
    identity: TypeName,
    parameters: &TypeParameters<Vec<Vec<Ty>>>,
) -> Result<(), ClassHeaderError> {
    let names = parameters.type_params();
    let unique: HashSet<_> = names.iter().collect();
    if unique.len() != names.len() {
        return Err(ClassHeaderError::InvalidFormals(identity));
    }
    if parameters
        .type_param_bounds()
        .iter()
        .flatten()
        .any(|bound| crate::types::ty_mentions_undeclared_param(*bound, names))
    {
        // A local class can name an enclosing method's formal. Until that lexical scope is
        // available here, neither its bound nor a replacement `Any` has declaration evidence.
        return Err(ClassHeaderError::MissingFormalScope(identity));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jvm::classfile::{ClassWriter, InnerClassSpec, ACC_PUBLIC};
    use crate::jvm::classpath::Classpath;

    fn class_header(
        name: &str,
        signature: Option<&str>,
        enclosing: Option<(&str, u16)>,
    ) -> (String, Vec<u8>) {
        let mut writer = ClassWriter::new(name, "java/lang/Object");
        if let Some(signature) = signature {
            writer.set_signature(signature);
        }
        if let Some((outer, access)) = enclosing {
            writer.add_inner_class(InnerClassSpec {
                inner: name.to_owned(),
                outer: Some(outer.to_owned()),
                name: Some("Member".to_owned()),
                access,
            });
        }
        (name.to_owned(), writer.finish())
    }

    fn libraries(headers: Vec<(String, Vec<u8>)>) -> JvmLibraries {
        let classpath = std::rc::Rc::new(Classpath::new(Vec::new()));
        classpath.set_stub_overlay(headers);
        JvmLibraries::new(classpath).expect("header-only fixture")
    }

    #[test]
    fn non_generic_inner_classes_still_capture_their_enclosing_formals() {
        let source = libraries(vec![
            class_header(
                "header/Outer",
                Some("<S:Ljava/lang/CharSequence;>Ljava/lang/Object;"),
                None,
            ),
            class_header(
                "header/Outer$Inner",
                None,
                Some(("header/Outer", ACC_PUBLIC)),
            ),
        ]);
        let header = source
            .class_application_header(type_name("header/Outer$Inner"))
            .expect("captured layout");
        assert_eq!(header.own_count, 0);
        assert_eq!(header.outer_instance, Some(type_name("header/Outer")));
        assert_eq!(header.parameters.type_params(), &["S".to_owned()]);
        assert_eq!(
            header.parameters.type_param_bounds(),
            &[vec![Ty::platform_nullable(Ty::obj("kotlin/CharSequence"))]]
        );
    }

    #[test]
    fn shadowed_captured_formals_keep_their_declared_recursive_and_cross_bounds() {
        let source = libraries(vec![
            class_header(
                "header/Outer",
                Some("<T::Ljava/lang/Comparable<TT;>;U:TT;>Ljava/lang/Object;"),
                None,
            ),
            class_header(
                "header/Outer$Inner",
                Some("<T:Ljava/lang/Object;>Ljava/lang/Object;"),
                Some(("header/Outer", ACC_PUBLIC)),
            ),
        ]);
        let identity = type_name("header/Outer$Inner");
        let header = source
            .class_application_header(identity)
            .expect("shadowed layout");
        let hidden = crate::types::hidden_captured_type_parameter(identity, 1, "T");
        assert_eq!(header.own_count, 1);
        assert_eq!(header.parameters.type_params(), &["T", hidden, "U"]);
        for bounds in &header.parameters.type_param_bounds()[1..] {
            assert!(bounds
                .iter()
                .any(|bound| crate::types::ty_mentions_param(*bound, &[hidden.to_owned()])));
            assert!(!bounds
                .iter()
                .any(|bound| crate::types::ty_mentions_param(*bound, &["T".to_owned()])));
        }
    }

    #[test]
    fn static_nested_classes_do_not_require_an_enclosing_generic_header() {
        let source = libraries(vec![class_header(
            "header/Missing$Nested",
            Some("<T:Ljava/lang/Object;>Ljava/lang/Object;"),
            Some(("header/Missing", ACC_PUBLIC | ACC_STATIC)),
        )]);
        let header = source
            .class_application_header(type_name("header/Missing$Nested"))
            .expect("static layout");
        assert_eq!(header.own_count, 1);
        assert_eq!(header.outer_instance, None);
        assert_eq!(header.parameters.type_params(), &["T".to_owned()]);
    }

    #[test]
    fn a_binary_dollar_does_not_invent_an_enclosing_instance() {
        let identity = type_name("header/Dollar$Name");
        let source = libraries(vec![class_header(
            "header/Dollar$Name",
            Some("<T:Ljava/lang/Object;>Ljava/lang/Object;"),
            None,
        )]);
        let header = source
            .class_application_header(identity)
            .expect("top-level layout");
        assert_eq!(header.own_count, 1);
        assert_eq!(header.outer_instance, None);
    }

    #[test]
    fn local_and_anonymous_self_entries_without_an_outer_remain_valid_headers() {
        for simple_name in [Some("Local"), None] {
            let identity = type_name("header/Local");
            let mut writer = ClassWriter::new("header/Local", "java/lang/Object");
            writer.set_signature("<T:Ljava/lang/Object;>Ljava/lang/Object;");
            writer.add_inner_class(InnerClassSpec {
                inner: "header/Local".to_owned(),
                outer: None,
                name: simple_name.map(str::to_owned),
                access: ACC_PUBLIC,
            });
            let bytes = writer.finish();
            let source = libraries(vec![("header/Local".to_owned(), bytes.clone())]);
            let header = source
                .class_application_header(identity)
                .expect("local header");
            assert_eq!(header.own_count, 1);
            assert_eq!(header.outer_instance, None);

            let mut metadata_class =
                crate::jvm::classreader::parse_class(&bytes).expect("local class");
            metadata_class.meta.class_visibility = Some(crate::types::Visibility::Private);
            metadata_class.meta.is_inner = false;
            assert_eq!(enclosing_instance(identity, &metadata_class), Ok(None));
            metadata_class.meta.is_inner = true;
            assert_eq!(
                enclosing_instance(identity, &metadata_class),
                Err(ClassHeaderError::InvalidNesting(identity))
            );
        }
    }

    #[test]
    fn missing_enclosing_evidence_is_not_an_own_only_layout() {
        let source = libraries(vec![class_header(
            "header/Missing$Inner",
            None,
            Some(("header/Missing", ACC_PUBLIC)),
        )]);
        assert_eq!(
            source
                .class_application_header(type_name("header/Missing$Inner"))
                .err(),
            Some(ClassHeaderError::MissingHeader(type_name("header/Missing")))
        );
    }

    #[test]
    fn enclosing_owner_cycles_are_refused_before_any_member_record_is_built() {
        let source = libraries(vec![
            class_header("header/A", None, Some(("header/B", ACC_PUBLIC))),
            class_header("header/B", None, Some(("header/A", ACC_PUBLIC))),
        ]);
        assert_eq!(
            source.class_application_header(type_name("header/A")).err(),
            Some(ClassHeaderError::CyclicOwners(type_name("header/A")))
        );
        assert!(source
            .cp
            .cached_library_type_name(source.api_version, type_name("header/A"))
            .is_none());
        assert!(source
            .cp
            .cached_library_type_name(source.api_version, type_name("header/B"))
            .is_none());
    }

    #[test]
    fn malformed_signatures_are_not_nongeneric_headers() {
        for signature in ["<T:", "<T:Ljava/lang/Object;>"] {
            let identity = type_name("header/Broken");
            let source = libraries(vec![class_header("header/Broken", Some(signature), None)]);
            assert_eq!(
                source.class_application_header(identity).err(),
                Some(ClassHeaderError::InvalidSignature(identity))
            );
        }
    }

    #[test]
    fn unknown_bound_formals_are_not_erased_to_placeholder_bounds() {
        let identity = type_name("header/Unbound");
        let source = libraries(vec![class_header(
            "header/Unbound",
            Some("<T:TS;>Ljava/lang/Object;"),
            None,
        )]);
        assert_eq!(
            source.class_application_header(identity).err(),
            Some(ClassHeaderError::MissingFormalScope(identity))
        );
    }

    #[test]
    fn classless_builtins_keep_their_authoritative_formal_variances() {
        let stdlib = crate::toolchain::stdlib_jar().expect("selected reference stdlib");
        let source = JvmLibraries::new(std::rc::Rc::new(Classpath::new(vec![stdlib])))
            .expect("builtin provider");
        let identity = type_name("kotlin/collections/List");
        assert!(source.cp.find_name(identity).is_none());
        let header = source
            .class_application_header(identity)
            .expect("builtin layout");
        assert_eq!(header.own_count, 1);
        assert_eq!(header.outer_instance, None);
        assert_eq!(
            header.parameters.type_param_variances(),
            &[TypeVariance::Out]
        );
    }

    #[test]
    fn a_physical_mapped_class_keeps_its_invariant_java_formal_policy() {
        let stdlib = crate::toolchain::stdlib_jar().expect("selected reference stdlib");
        let source = JvmLibraries::new(std::rc::Rc::new(Classpath::new(vec![stdlib])))
            .expect("builtin provider");
        let physical = type_name("java/util/List");
        assert_eq!(source.cp.builtin_class_variances_name(physical), None);
        let header = source
            .class_application_header(physical)
            .expect("mapped physical layout");
        assert_eq!(header.own_count, 1);
        assert_eq!(
            header.parameters.type_param_variances(),
            &[TypeVariance::Invariant]
        );
        assert_eq!(
            source
                .class_application_header(type_name("kotlin/collections/List"))
                .expect("semantic readonly layout")
                .parameters
                .type_param_variances(),
            &[TypeVariance::Out]
        );
    }
}
