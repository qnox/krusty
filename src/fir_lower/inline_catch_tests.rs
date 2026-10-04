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

    let default_catch_types = ir
        .value_names
        .iter()
        .filter_map(|(&declaration, name)| {
            if name != "result" {
                return None;
            }
            let IrExpr::Variable {
                init: Some(initial),
                ..
            } = ir.expr(declaration)
            else {
                return None;
            };
            let IrExpr::Try { catches, .. } = ir.expr(*initial) else {
                return None;
            };
            catches.first().map(|catch| catch.ty)
        })
        .collect::<Vec<_>>();

    assert_eq!(default_catch_types.len(), 2, "{default_catch_types:?}");
    assert_eq!(
        default_catch_types
            .iter()
            .filter(|&&ty| ty == Ty::obj("ParentFailure"))
            .count(),
        1,
        "{default_catch_types:?}"
    );
    assert_eq!(
        default_catch_types
            .iter()
            .filter(|&&ty| ty == Ty::obj("ChildFailure"))
            .count(),
        1,
        "{default_catch_types:?}"
    );
}
