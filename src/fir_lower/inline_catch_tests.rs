use crate::ir::IrExpr;
use crate::types::Ty;

use super::tests::lower_single_source;

#[test]
fn inline_default_catches_use_each_calls_reified_type() {
    let ir = lower_single_source(
        "open class ParentFailure : Throwable()\n\
         class ChildFailure : ParentFailure()\n\
         inline fun <reified E : Throwable> defaultCatch(\n\
             result: String = try {\n\
                 throw ParentFailure()\n\
             } catch (ignore: E) {\n\
                 \"Y\"\n\
             } catch (throwable: Throwable) {\n\
                 \"N\"\n\
             }\n\
         ): String = result\n\
         fun box(): String = defaultCatch<ParentFailure>() + defaultCatch<ChildFailure>()\n",
        "InlineDefaultCatch",
    );

    let default_catches = ir
        .value_names
        .iter()
        .filter_map(|(&declaration, name)| {
            if name != "result" {
                return None;
            }
            let IrExpr::Variable {
                index,
                init: Some(initial),
                ..
            } = ir.expr(declaration)
            else {
                return None;
            };
            let IrExpr::Try { catches, .. } = ir.expr(*initial) else {
                return None;
            };
            catches.first().map(|catch| {
                (
                    *index,
                    catch.ty,
                    catches.iter().map(|catch| catch.var).collect::<Vec<_>>(),
                )
            })
        })
        .collect::<Vec<_>>();

    assert_eq!(default_catches.len(), 2, "{default_catches:?}");
    for (parameter, _, catches) in &default_catches {
        assert!(
            catches.iter().all(|catch| catch != parameter),
            "the default parameter and its catch bindings need distinct slots: {default_catches:?}"
        );
        assert_eq!(
            catches
                .iter()
                .copied()
                .collect::<std::collections::HashSet<_>>()
                .len(),
            catches.len(),
            "each catch binding needs its own slot: {default_catches:?}"
        );
    }
    assert_eq!(
        default_catches
            .iter()
            .filter(|(_, ty, _)| *ty == Ty::obj("ParentFailure"))
            .count(),
        1,
        "{default_catches:?}"
    );
    assert_eq!(
        default_catches
            .iter()
            .filter(|(_, ty, _)| *ty == Ty::obj("ChildFailure"))
            .count(),
        1,
        "{default_catches:?}"
    );
}
