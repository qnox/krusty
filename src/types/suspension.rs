//! How a `suspend` function's signature changes once it passes its continuation.

use super::{wk, Ty};

/// What a suspend function answering `result` takes last and answers once it passes its
/// continuation: `Continuation<result>`, and `Any?`.
pub fn continuation_passing(result: Ty) -> (Ty, Ty) {
    (
        Ty::obj_args_name(wk::continuation(), &[result]),
        Ty::nullable(Ty::obj_name(wk::any())),
    )
}
