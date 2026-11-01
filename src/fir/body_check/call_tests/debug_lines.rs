use super::*;
use crate::fir::FirExpressionDebugLines;

#[test]
fn multiline_member_call_keeps_the_selected_call_source_and_end_lines() {
    let (body, index) = checked_function_body(
        "class Recorder {\n\
             suspend fun save(label: String, value: Int): Int = value\n\
         }\n\
         suspend fun work(recorder: Recorder): Int {\n\
             val result = recorder.save(\n\
                 value = 3,\n\
                 label = \"entry\",\n\
             )\n\
             return result\n\
         }\n",
        "work",
    );
    let call = (0..body.expression_count())
        .map(|raw| FirExprId::from_raw(raw as u32))
        .find(|expression| {
            let Some(FirExpr {
                kind: FirExprKind::Call(call),
                ..
            }) = body.expr(*expression)
            else {
                return false;
            };
            call.target
                .module()
                .and_then(|target| index.callable_name(target))
                == Some("save")
        })
        .expect("selected suspend member call");
    assert_eq!(
        body.expression_debug_lines(call),
        FirExpressionDebugLines {
            start: 5,
            source: 5,
            end: 8,
            positionless: false
        }
    );
}
