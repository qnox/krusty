use super::*;
use crate::backend::{BackendModuleFacts, CheckedBackendClassifiers};
use crate::metadata::type_encoder::{
    encode_type, StringTable, TypeEncodeError, TypeParameterRef, TypeParameters,
};

fn copy_application(source: &str, own: usize, captured: usize) -> Vec<Ty> {
    let ir =
        crate::fir_lower::tests::lower_single_source_with_jvm_stdlib(source, "CopySelfApplication");
    let classes = ir
        .classes
        .iter()
        .filter(|class| class.is_data)
        .collect::<Vec<_>>();
    let [class] = classes.as_slice() else {
        panic!("one checked data-class declaration")
    };
    assert_eq!(class.type_params.len(), own);
    assert_eq!(class.captured_type_params.len(), captured);
    let record = build(&ir, class).expect("complete generated member declarations");
    let copies = record
        .methods
        .iter()
        .zip(&record.method_origins)
        .filter(|(_, origin)| {
            matches!(
                origin,
                FunctionOrigin::DataMember(IrDataClassMemberRole::Copy)
            )
        })
        .map(|(method, _)| method)
        .collect::<Vec<_>>();
    let [copy] = copies.as_slice() else {
        panic!("one typed generated Copy record")
    };
    let owner = class.fq_name_id();
    assert_eq!(
        copy.ret,
        ir.declared_class_self_type(owner)
            .expect("checked complete self application")
    );
    assert_eq!(copy.ret.type_args().len(), own + captured);
    assert_eq!(
        copy.ret.type_args()[own..]
            .iter()
            .map(|argument| argument
                .ty_param_name()
                .expect("captured semantic parameter"))
            .collect::<Vec<_>>(),
        class
            .captured_type_params
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
    );

    let module =
        BackendModuleFacts::from_classifiers([], []).expect("no external classifier facts");
    let classifiers = CheckedBackendClassifiers::new(&module, &crate::libraries::EmptySymbolSource);
    let applications = |classifier| {
        crate::metadata::classifier_applications::classifier_application(
            &ir,
            &classifiers,
            classifier,
        )
    };
    let direct = |_| Ok(crate::metadata::ClassifierApplication::Direct);
    let local = local_classifiers::names(&ir);
    let namespaces = local_classifiers::enum_entry_bodies(&ir);
    let anonymous = local_classifiers::anonymous_names(&ir);
    let mut parameters = TypeParameters::classifier(own, class.captured_type_params.clone());
    if let Some(signature) = ir.class_signature(&class.fq_name()) {
        for (ordinal, parameter) in signature.type_params.iter().enumerate() {
            parameters.insert(
                parameter.semantic_name.clone(),
                TypeParameterRef::Id(ordinal as u64),
            );
        }
    }
    let mut actual =
        StringTable::with_local_classifiers(&local, &namespaces, &anonymous, None, &applications);
    let mut expected =
        StringTable::with_local_classifiers(&local, &namespaces, &anonymous, None, &direct);
    let encoded = encode_type(&mut actual, copy.ret, &parameters)
        .expect("complete self application passes strict metadata validation");
    let visible = Ty::obj_args_name(owner, &copy.ret.type_args()[..own]);
    let projected =
        encode_type(&mut expected, visible, &parameters).expect("own declaration application");
    assert_eq!(encoded.as_bytes(), projected.as_bytes());
    assert_eq!(
        actual.serialize_types().as_bytes(),
        expected.serialize_types().as_bytes()
    );
    assert_eq!(actual.into_strings(), expected.into_strings());
    if captured != 0 {
        let mut incomplete = StringTable::with_local_classifiers(
            &local,
            &namespaces,
            &anonymous,
            None,
            &applications,
        );
        let Err(error) = encode_type(&mut incomplete, visible, &parameters) else {
            panic!("the previous own-only application remains invalid")
        };
        assert_eq!(
            error,
            TypeEncodeError::LexicalClassifierArity {
                classifier: owner,
                arguments: own,
                own,
                captured
            }
        );
    }
    copy.ret.type_args().to_vec()
}

#[test]
fn captured_only_copy_uses_complete_semantic_parameters_and_bounds() {
    let arguments = copy_application(
        r#"
fun <A : Number, B : CharSequence> captured(first: A, second: B): Any {
    data class Local(val first: A, val second: B)
    return Local(first, second)
}
"#,
        0,
        2,
    );
    for (argument, bound) in arguments
        .into_iter()
        .zip([Ty::obj("kotlin/Number"), Ty::obj("kotlin/CharSequence")])
    {
        assert_eq!(
            argument,
            Ty::ty_param(argument.ty_param_name().unwrap(), bound)
        );
    }
}

#[test]
fn copy_keeps_own_arguments_before_lexical_captures() {
    copy_application(
        r#"
fun <A : Number, B : CharSequence> captured(first: A, second: B): Any {
    data class Local<C>(val payload: C, val first: A, val second: B)
    return Local(1, first, second)
}
"#,
        1,
        2,
    );
}

#[test]
fn ordinary_generic_copy_retains_its_own_application() {
    copy_application("data class Generic<A : Number>(val value: A)", 1, 0);
}

#[test]
fn nongeneric_copy_retains_the_plain_checked_self_type() {
    copy_application("data class Plain(val value: String)", 0, 0);
}
