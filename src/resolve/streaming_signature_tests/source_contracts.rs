use super::*;

#[test]
fn bounded_contract_declaration_publishes_by_stable_identity() {
    let source = r#"// WITH_STDLIB
        import kotlin.contracts.ExperimentalContracts
        import kotlin.contracts.contract

        sealed class Status {
            class Error(val text: String) : Status()
        }

        @OptIn(ExperimentalContracts::class)
        fun Status.isError(): Boolean {
            contract { returns(true) implies (this@isError is Status.Error) }
            return this is Status.Error
        }

        fun read(status: Status): String {
            if (status.isError()) return status.text
            return "OK"
        }
    "#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("BoundedSourceContract")];
    let mut classpath = crate::toolchain::classpath_jars_for(source);
    if let Some(jdk) = crate::toolchain::jdk_modules() {
        classpath.push(jdk);
    }
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        crate::frontend::PlatformProvider::jvm(Box::new(
            crate::jvm::jvm_libraries::JvmLibraries::new(std::rc::Rc::new(
                crate::jvm::classpath::Classpath::new(classpath),
            ))
            .expect("JVM provider initialization"),
        )),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("source contracts must finalize in Pass 1")
        .module
        .index();
    let declaration = (0..index.declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .find(|declaration| index.declaration_name(*declaration) == Some("isError"))
        .expect("stable isError declaration");
    let contract = index
        .contract(declaration)
        .expect("Pass 1 must publish the source contract")
        .as_contract();
    let [crate::contracts::Effect::ConditionalReturns {
        returns: crate::contracts::ReturnsValue::Bool(true),
        conclusion:
            crate::contracts::Condition::IsType {
                param: crate::contracts::ParamRef::Receiver,
                ty: crate::contracts::ConditionType::Metadata(ty),
                negated: false,
            },
    }] = contract.effects.as_slice()
    else {
        panic!("unexpected resolved contract: {contract:?}");
    };
    assert_eq!(*ty, Ty::obj("Status$Error"));
    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
}

#[test]
fn generic_source_contract_uses_callee_type_parameter_identity_at_call_site() {
    let source = r#"// WITH_STDLIB
        import kotlin.contracts.contract
        import kotlin.contracts.ExperimentalContracts

        @OptIn(ExperimentalContracts::class)
        inline fun <T, reified R> Refinement<T, R>.validate(value: T): Boolean {
            contract { returns() implies (value is R) }
            return true
        }
        class Refinement<T, R>
        fun narrowed(refinement: Refinement<Any, String>, value: Any): String {
            refinement.validate(value)
            return value
        }
    "#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("GenericSourceContract")];
    let mut classpath = crate::toolchain::classpath_jars_for(source);
    if let Some(jdk) = crate::toolchain::jdk_modules() {
        classpath.push(jdk);
    }
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        crate::frontend::PlatformProvider::jvm(Box::new(
            crate::jvm::jvm_libraries::JvmLibraries::new(std::rc::Rc::new(
                crate::jvm::classpath::Classpath::new(classpath),
            ))
            .expect("JVM provider initialization"),
        )),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("source contracts must finalize in Pass 1")
        .module
        .index();
    let declaration = (0..index.declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .find(|declaration| index.declaration_name(*declaration) == Some("validate"))
        .expect("stable validate declaration");
    let contract = index
        .contract(declaration)
        .expect("Pass 1 must publish the source contract")
        .as_contract();
    let [crate::contracts::Effect::ConditionalReturns {
        conclusion:
            crate::contracts::Condition::IsType {
                ty: crate::contracts::ConditionType::Metadata(ty),
                ..
            },
        ..
    }] = contract.effects.as_slice()
    else {
        panic!("contract condition must be normalized to semantic type: {contract:?}");
    };
    assert!(
        ty.ty_param_name().is_some(),
        "the conclusion names the callee's type parameter: {ty:?}"
    );
    // The call site substitutes `R := String` through that identity, so `value` is narrowed.
    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
}

#[test]
fn pass_one_contract_is_visible_when_caller_source_streams_first() {
    let caller = r#"
        fun read(status: Status): String {
            if (status.isError()) return status.text
            return "OK"
        }
    "#;
    let declaration = r#"// WITH_STDLIB
        import kotlin.contracts.ExperimentalContracts
        import kotlin.contracts.contract

        sealed class Status {
            class Error(val text: String) : Status()
        }

        @OptIn(ExperimentalContracts::class)
        fun Status.isError(): Boolean {
            contract { returns(true) implies (this@isError is Status.Error) }
            return this is Status.Error
        }
    "#;
    let inputs = [
        SourceInput::kotlin(caller).with_file_stem("Caller"),
        SourceInput::kotlin(declaration).with_file_stem("Declaration"),
    ];
    let mut classpath = crate::toolchain::classpath_jars_for(declaration);
    if let Some(jdk) = crate::toolchain::jdk_modules() {
        classpath.push(jdk);
    }
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        crate::frontend::PlatformProvider::jvm(Box::new(
            crate::jvm::jvm_libraries::JvmLibraries::new(std::rc::Rc::new(
                crate::jvm::classpath::Classpath::new(classpath),
            ))
            .expect("JVM provider initialization"),
        )),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert_eq!(census.bodies, 4);
    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
}
