use super::*;
use crate::libraries::{DefaultCallRealization, GenericReturnPolicy};
use crate::symbol_source::SymbolSource;
use crate::types::type_name;

struct DefaultSource {
    bridge: FunctionInfo,
    base: FunctionInfo,
}

impl SymbolSource for DefaultSource {
    fn symbols(
        &self,
        namespace: SymbolNamespace,
        name: &str,
    ) -> std::rc::Rc<crate::libraries::ResolvedSymbols> {
        let overloads = match (namespace, name) {
            (SymbolNamespace::Package(TypeName::ROOT), "same") => vec![self.base.clone()],
            (SymbolNamespace::Package(TypeName::ROOT), "same$default") => {
                vec![self.bridge.clone()]
            }
            _ => Vec::new(),
        };
        std::rc::Rc::new(crate::libraries::ResolvedSymbols {
            callables: crate::libraries::Callables::Functions(FunctionSet { overloads }),
            ..Default::default()
        })
    }
}

impl SemanticPlatform for DefaultSource {}

#[test]
fn default_realization_pairs_distinct_declaration_owned_formals() {
    let any = Ty::obj("kotlin/Any");
    let nullable_any = Ty::nullable(any);
    let bridge_formal = crate::types::metadata_type_parameter("T");
    let bridge_parameter = Ty::ty_param(bridge_formal, nullable_any);
    let mut callable = LibraryCallable::library(
        "demo/AssertionsKt",
        "same",
        vec![bridge_parameter, bridge_parameter, Ty::nullable(Ty::String)],
        Ty::Unit,
        Ty::Unit,
        "(Ljava/lang/Object;Ljava/lang/Object;Ljava/lang/String;)V",
    );
    callable.physical_name = Some("same$default".to_string());
    callable.default_call = true;
    let mut bridge = FunctionInfo::plain(FnKind::TopLevel, None, callable);
    bridge.call_sig.required = 2;
    bridge.call_sig.param_defaults = vec![false, false, true];
    bridge.generic_sig = Some(GenericSig {
        formals: vec![bridge_formal.to_string()],
        formal_bounds: vec![vec![nullable_any]],
        receiver: None,
        params: bridge.callable.params.clone(),
        ret: Ty::Unit,
        return_policy: GenericReturnPolicy::Exact,
    });
    let mut base = bridge.clone();
    base.callable.physical_name = None;
    base.callable.default_call = false;
    let base_formal = crate::types::metadata_type_parameter("T");
    let base_parameter = Ty::ty_param(base_formal, nullable_any);
    base.callable.params = vec![base_parameter, base_parameter, Ty::nullable(Ty::String)];
    base.generic_sig = Some(GenericSig {
        formals: vec![base_formal.to_string()],
        formal_bounds: vec![vec![nullable_any]],
        receiver: None,
        params: base.callable.params.clone(),
        ret: Ty::Unit,
        return_policy: GenericReturnPolicy::Exact,
    });
    base.callable.default_realization = Some(Box::new(DefaultCallRealization {
        owner: bridge.callable.owner,
        name: bridge.callable.physical_name().to_string(),
        descriptor: bridge.callable.descriptor.clone(),
        declaration_owner: bridge.callable.owner,
        real_params: base.callable.physical_params.clone(),
        mask_count: 1,
        ret: bridge.callable.physical_ret,
        suspend: bridge.callable.suspend,
    }));
    let source = DefaultSource { bridge, base };
    let scopes = [type_name("")];
    let resolver = SymbolResolver::new_scoped(&source, &scopes);
    let call = resolver
        .resolve_symbol(
            SymRecv::TopLevel,
            "same",
            &[Ty::nullable(Ty::String), Ty::String],
            &[],
        )
        .and_then(Symbol::top_level_call)
        .expect("generic default callable should resolve");

    assert!(call.default_call);
    assert_eq!(
        call.params,
        vec![
            Ty::nullable(Ty::String),
            Ty::nullable(Ty::String),
            Ty::nullable(Ty::String)
        ]
    );
}

struct EmptySource;
impl SymbolSource for EmptySource {}
impl SemanticPlatform for EmptySource {}

#[test]
fn top_level_specificity_alpha_normalizes_each_declarations_unbound_formals() {
    let nullable_any = Ty::nullable(Ty::obj("kotlin/Any"));
    let mode = Ty::obj("kotlin/LazyThreadSafetyMode");
    let overload = |first, formal: &'static str, descriptor| {
        let result = Ty::ty_param(formal, nullable_any);
        let block = Ty::fun(Vec::new(), result);
        let callable = LibraryCallable::library(
            "kotlin/LazyKt",
            "lazy",
            vec![first, block],
            Ty::obj_args("kotlin/Lazy", &[result]),
            Ty::obj("kotlin/Lazy"),
            descriptor,
        );
        FunctionInfo {
            generic_sig: Some(GenericSig {
                formals: vec![formal.to_string()],
                formal_bounds: vec![vec![nullable_any]],
                receiver: None,
                params: vec![first, block],
                ret: Ty::obj_args("kotlin/Lazy", &[result]),
                return_policy: GenericReturnPolicy::Exact,
            }),
            ..FunctionInfo::plain(FnKind::TopLevel, None, callable)
        }
    };
    let mode_formal = crate::types::metadata_type_parameter("T");
    let lock_formal = crate::types::metadata_type_parameter("T");
    let mode_overload = overload(
        mode,
        mode_formal,
        "(Lkotlin/LazyThreadSafetyMode;Lkotlin/jvm/functions/Function0;)Lkotlin/Lazy;",
    );
    let lock_overload = overload(
        Ty::nullable(Ty::obj("kotlin/Any")),
        lock_formal,
        "(Ljava/lang/Object;Lkotlin/jvm/functions/Function0;)Lkotlin/Lazy;",
    );
    let scopes = [type_name("")];
    let resolver = SymbolResolver::new_scoped(&EmptySource, &scopes);

    let selected = resolver
        .pick_top_level(
            "lazy",
            &FunctionSet {
                overloads: vec![lock_overload, mode_overload],
            },
            &[
                CallArgKind::Typed(mode),
                CallArgKind::LambdaLiteral(Ty::Error),
            ],
            &[],
            None,
        )
        .expect("the concrete mode overload should dominate the lock overload");

    assert_eq!(
        selected.descriptor,
        "(Lkotlin/LazyThreadSafetyMode;Lkotlin/jvm/functions/Function0;)Lkotlin/Lazy;"
    );
    assert_eq!(
        selected.params,
        vec![
            mode,
            Ty::fun(Vec::new(), Ty::ty_param(mode_formal, nullable_any))
        ]
    );
}
