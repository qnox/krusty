//! Suspend code an inline function creates or splices: suspend lambdas and objects regenerated at
//! the call around a `crossinline` lambda, and a `suspend inline` function called with its trailing
//! lambda.

use super::common;

#[path = "../build840_nn1_suspend_inline_withlock_e2e.rs"]
mod build840_nn1_suspend_inline_withlock_e2e;
#[path = "../crossinline_suspend_object_e2e.rs"]
mod crossinline_suspend_object_e2e;
#[path = "../suspend_lambda_regeneration_e2e.rs"]
mod suspend_lambda_regeneration_e2e;
#[path = "../suspend_value_invokes_e2e.rs"]
mod suspend_value_invokes_e2e;
