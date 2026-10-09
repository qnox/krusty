/* The coroutine protocol a suspend function's state machine runs against.

   The compiler turns every suspend function into a continuation class of the program's own (see
   `backend::coroutines` in the compiler), so the runtime holds only what crosses between classes it
   cannot see: the `COROUTINE_SUSPENDED` marker, the failure a continuation is resumed with, and a
   call to `resumeWith` or `context` on a continuation of any class. A program's class answers the
   last two through the thunks its descriptor publishes (`KType.continuation_resume_with`,
   `KType.continuation_context`), so a continuation declared in one file is resumed from another,
   and from here, by one indirect call.

   A `Result<T>` is carried as its raw value, as Kotlin's own value class is: the value itself when
   it succeeded, and a `Result.Failure` holding the exception when it failed. */

#include "krusty_internal.h"

#include <stddef.h>

/* `COROUTINE_SUSPENDED`. Identity is all that is ever asked of it. */
static const KType kt_type_coroutine_singletons = {
    KT_NAMED("kotlin.coroutines.intrinsics.", "CoroutineSingletons"),
    .instance_size = sizeof(KObjectHeader),
    .super = &kt_type_any,
    .vtable = kt_any_vtable,
    .vtable_length = 3};

KRef kt_coroutine_suspended(void) {
    static KObjectHeader suspended = {&kt_type_coroutine_singletons};
    return (KRef)&suspended;
}

typedef struct KFailure {
    KObjectHeader header;
    KRef exception;
} KFailure;

static const uint32_t kt_failure_references[] = {offsetof(KFailure, exception)};

/* `Result.Failure` compares, hashes and prints by its exception, as the library's class does. */
static kt_boolean kt_failure_equals(KRef self, KRef other) {
    return other != NULL && kt_type_of(other) == &kt_type_result_failure &&
           kt_equals(((KFailure *)self)->exception, ((KFailure *)other)->exception);
}

static kt_int kt_failure_hash_code(KRef self) {
    return kt_hash_code(((KFailure *)self)->exception);
}

static KRef kt_failure_to_string(KRef self) {
    static KRef open, close;
    KRef text = kt_string_plus(kt_string_literal("Failure(", 8, &open),
                               kt_to_string(((KFailure *)self)->exception));
    return kt_string_plus(text, kt_string_literal(")", 1, &close));
}

static const kt_fn kt_failure_vtable[] = {
    (kt_fn)kt_failure_equals, (kt_fn)kt_failure_hash_code, (kt_fn)kt_failure_to_string};

const KType kt_type_result_failure = {KT_NAMED("kotlin.Result.", "Failure"),
                                             .instance_size = sizeof(KFailure),
                                             .reference_count = 1,
                                             .reference_offsets = kt_failure_references,
                                             .super = &kt_type_any,
                                             .vtable = kt_failure_vtable,
                                             .vtable_length = 3};

KRef kt_result_failure(KRef exception) {
    KFailure *failure = kt_gc_allocate(&kt_type_result_failure, sizeof(KFailure));
    failure->exception = exception;
    return (KRef)failure;
}

void kt_result_throw_on_failure(KRef value) {
    if (value != NULL && kt_type_of(value) == &kt_type_result_failure) {
        kt_throw(((KFailure *)value)->exception);
    }
}

static kt_boolean kt_result_is_failure_value(KRef value);

/* What the program's class publishes, or a loud failure: an object reaching here that implements
   `Continuation` without the thunks is a compiler defect, never a program's. */
static const KType *kt_continuation_type(KRef continuation) {
    if (continuation == NULL) {
        kt_throw(kt_throwable_new(&kt_type_null_pointer_exception, NULL));
        return NULL;
    }
    return kt_type_of(continuation);
}

void kt_continuation_resume_with(KRef continuation, KRef result) {
    const KType *type = kt_continuation_type(continuation);
    if (type == NULL) {
        return;
    }
    if (type->continuation_resume_with == NULL) {
        KT_FAIL("krusty: a continuation's class publishes no resumeWith\n");
    }
    type->continuation_resume_with(continuation, result);
}

KRef kt_continuation_context(KRef continuation) {
    const KType *type = kt_continuation_type(continuation);
    if (type == NULL) {
        return NULL;
    }
    if (type->continuation_context == NULL) {
        KT_FAIL("krusty: a continuation's class publishes no context\n");
    }
    return type->continuation_context(continuation);
}

/* ---- starting and resuming from the library's entry points ----------------------------------- */

/* Deliver what a coroutine's first run produced: nothing while it is suspended, since whatever
   resumes it delivers then, and otherwise its value or the exception it ended with. */
static void kt_coroutine_complete(KRef completion, KRef produced) {
    KRef thrown = kt_pending_exception();
    if (thrown != NULL) {
        kt_clear_pending();
        produced = kt_result_failure(thrown);
    } else if (produced == kt_coroutine_suspended()) {
        return;
    }
    kt_continuation_resume_with(completion, produced);
}

static kt_fn kt_function_invoke(KRef block) {
    if (block == NULL) {
        kt_throw(kt_throwable_new(&kt_type_null_pointer_exception, NULL));
        return NULL;
    }
    const KType *type = kt_type_of(block);
    if (type->vtable == NULL || type->vtable_length <= KT_SLOT_INVOKE) {
        KT_FAIL("krusty: a function value was expected here\n");
    }
    return type->vtable[KT_SLOT_INVOKE];
}

/* `(suspend () -> T).startCoroutine(completion)`. A suspend lambda's `invoke` takes the
   continuation after its declared parameters and runs the body until it first suspends. */
void kt_start_coroutine(KRef block, KRef completion) {
    kt_fn invoke = kt_function_invoke(block);
    if (invoke == NULL) {
        return;
    }
    KRef produced = ((KRef(*)(KRef, KRef))invoke)(block, completion);
    kt_coroutine_complete(completion, produced);
}

/* `(suspend R.() -> T).startCoroutine(receiver, completion)`. */
void kt_start_coroutine_with_receiver(KRef block, KRef receiver, KRef completion) {
    kt_fn invoke = kt_function_invoke(block);
    if (invoke == NULL) {
        return;
    }
    KRef produced = ((KRef(*)(KRef, KRef, KRef))invoke)(block, receiver, completion);
    kt_coroutine_complete(completion, produced);
}

/* `startCoroutineUninterceptedOrReturn`: the block's first answer is the caller's, a value or
   `COROUTINE_SUSPENDED`. */
KRef kt_start_coroutine_unintercepted_or_return(KRef block, KRef completion) {
    kt_fn invoke = kt_function_invoke(block);
    if (invoke == NULL) {
        return NULL;
    }
    return ((KRef(*)(KRef, KRef))invoke)(block, completion);
}

KRef kt_start_coroutine_unintercepted_or_return_with_receiver(KRef block, KRef receiver,
                                                              KRef completion) {
    kt_fn invoke = kt_function_invoke(block);
    if (invoke == NULL) {
        return NULL;
    }
    return ((KRef(*)(KRef, KRef, KRef))invoke)(block, receiver, completion);
}

/* `Continuation.resume(value)`: a successful `Result` is the value itself. */
void kt_continuation_resume(KRef continuation, KRef value) {
    kt_continuation_resume_with(continuation, value);
}

void kt_continuation_resume_with_exception(KRef continuation, KRef exception) {
    kt_continuation_resume_with(continuation, kt_result_failure(exception));
}

/* There is no dispatcher on this target: a continuation resumes on the thread that resumes it. */
KRef kt_continuation_intercepted(KRef continuation) {
    return continuation;
}

/* ---- continuations the library builds ------------------------------------------------------ */

/* `Continuation(context) { result -> … }`: the library's inline builder, whose class is laid out
   here. The block takes the `Result` boxed, as any `(Result<T>) -> Unit` value does. */
typedef struct KBlockContinuation {
    KObjectHeader header;
    KRef context;
    KRef block;
} KBlockContinuation;

static void kt_block_continuation_resume_with(KRef self, KRef result) {
    KBlockContinuation *continuation = (KBlockContinuation *)self;
    kt_fn invoke = kt_function_invoke(continuation->block);
    if (invoke == NULL) {
        return;
    }
    KRef boxed = kt_result_box(result);
    ((KRef(*)(KRef, KRef))invoke)(continuation->block, boxed);
}

static KRef kt_block_continuation_context(KRef self) {
    return ((KBlockContinuation *)self)->context;
}

static const uint32_t kt_block_continuation_references[] = {
    offsetof(KBlockContinuation, context), offsetof(KBlockContinuation, block)};

static const KType kt_type_block_continuation = {
    KT_NAMED("kotlin.coroutines.", "Continuation"),
    .instance_size = sizeof(KBlockContinuation),
    .reference_count = 2,
    .reference_offsets = kt_block_continuation_references,
    .super = &kt_type_any,
    .vtable = kt_any_vtable,
    .vtable_length = 3,
    .continuation_resume_with = kt_block_continuation_resume_with,
    .continuation_context = kt_block_continuation_context};

KRef kt_continuation_new(KRef context, KRef block) {
    KBlockContinuation *continuation =
        kt_gc_allocate(&kt_type_block_continuation, sizeof(KBlockContinuation));
    continuation->context = context;
    continuation->block = block;
    return (KRef)continuation;
}

/* `suspendCoroutine { c -> … }`: the block gets a continuation that may be resumed while the block
   still runs, or later, exactly once. Resumed before the block returns, the caller goes on with the
   value without suspending; otherwise it suspends and the first resume reaches its own
   continuation. */
typedef struct KSafeContinuation {
    KObjectHeader header;
    KRef delegate;
    KRef result;
    kt_int state;
} KSafeContinuation;

enum { KT_SAFE_UNDECIDED, KT_SAFE_RESUMED, KT_SAFE_SUSPENDED, KT_SAFE_DELIVERED };

static void kt_safe_continuation_resume_with(KRef self, KRef result) {
    KSafeContinuation *safe = (KSafeContinuation *)self;
    switch (safe->state) {
    case KT_SAFE_UNDECIDED:
        safe->result = result;
        safe->state = KT_SAFE_RESUMED;
        return;
    case KT_SAFE_SUSPENDED:
        safe->state = KT_SAFE_DELIVERED;
        kt_continuation_resume_with(safe->delegate, result);
        return;
    default:
        kt_throw(kt_throwable_new(&kt_type_illegal_state_exception, kt_string_utf8("Already resumed", 15)));
    }
}

static KRef kt_safe_continuation_context(KRef self) {
    return kt_continuation_context(((KSafeContinuation *)self)->delegate);
}

static const uint32_t kt_safe_continuation_references[] = {offsetof(KSafeContinuation, delegate),
                                                           offsetof(KSafeContinuation, result)};

static const KType kt_type_safe_continuation = {
    KT_NAMED("kotlin.coroutines.", "SafeContinuation"),
    .instance_size = sizeof(KSafeContinuation),
    .reference_count = 2,
    .reference_offsets = kt_safe_continuation_references,
    .super = &kt_type_any,
    .vtable = kt_any_vtable,
    .vtable_length = 3,
    .continuation_resume_with = kt_safe_continuation_resume_with,
    .continuation_context = kt_safe_continuation_context};

KRef kt_suspend_coroutine(KRef block, KRef continuation) {
    kt_fn invoke = kt_function_invoke(block);
    if (invoke == NULL) {
        return NULL;
    }
    KSafeContinuation *safe = kt_gc_allocate(&kt_type_safe_continuation, sizeof(KSafeContinuation));
    safe->delegate = continuation;
    ((KRef(*)(KRef, KRef))invoke)(block, (KRef)safe);
    if (kt_pending_exception() != NULL) {
        return NULL;
    }
    if (safe->state == KT_SAFE_UNDECIDED) {
        safe->state = KT_SAFE_SUSPENDED;
        return kt_coroutine_suspended();
    }
    safe->state = KT_SAFE_DELIVERED;
    return kt_result_get_or_throw(safe->result);
}

/* `suspendCoroutineUninterceptedOrReturn { c -> … }`: the block's answer is the caller's. */
KRef kt_suspend_coroutine_unintercepted_or_return(KRef block, KRef continuation) {
    kt_fn invoke = kt_function_invoke(block);
    if (invoke == NULL) {
        return NULL;
    }
    return ((KRef(*)(KRef, KRef))invoke)(block, continuation);
}

/* `(suspend () -> T).createCoroutine(completion)`: a continuation that starts the block when it is
   first resumed. */
typedef struct KStartContinuation {
    KObjectHeader header;
    KRef block;
    KRef receiver;
    KRef completion;
    kt_boolean has_receiver;
    kt_boolean started;
} KStartContinuation;

static void kt_start_continuation_resume_with(KRef self, KRef result) {
    KStartContinuation *start = (KStartContinuation *)self;
    if (start->started) {
        kt_throw(kt_throwable_new(&kt_type_illegal_state_exception, kt_string_utf8("Already resumed", 15)));
        return;
    }
    start->started = 1;
    if (kt_result_is_failure_value(result)) {
        kt_continuation_resume_with(start->completion, result);
    } else if (start->has_receiver) {
        kt_start_coroutine_with_receiver(start->block, start->receiver, start->completion);
    } else {
        kt_start_coroutine(start->block, start->completion);
    }
}

static KRef kt_start_continuation_context(KRef self) {
    return kt_continuation_context(((KStartContinuation *)self)->completion);
}

static const uint32_t kt_start_continuation_references[] = {
    offsetof(KStartContinuation, block), offsetof(KStartContinuation, receiver),
    offsetof(KStartContinuation, completion)};

static const KType kt_type_start_continuation = {
    KT_NAMED("kotlin.coroutines.", "CreatedCoroutine"),
    .instance_size = sizeof(KStartContinuation),
    .reference_count = 3,
    .reference_offsets = kt_start_continuation_references,
    .super = &kt_type_any,
    .vtable = kt_any_vtable,
    .vtable_length = 3,
    .continuation_resume_with = kt_start_continuation_resume_with,
    .continuation_context = kt_start_continuation_context};

static KRef kt_start_continuation_new(KRef block, KRef receiver, kt_boolean has_receiver,
                                      KRef completion) {
    KStartContinuation *start =
        kt_gc_allocate(&kt_type_start_continuation, sizeof(KStartContinuation));
    start->block = block;
    start->receiver = receiver;
    start->has_receiver = has_receiver;
    start->completion = completion;
    return (KRef)start;
}

KRef kt_create_coroutine(KRef block, KRef completion) {
    return kt_start_continuation_new(block, NULL, 0, completion);
}

KRef kt_create_coroutine_with_receiver(KRef block, KRef receiver, KRef completion) {
    return kt_start_continuation_new(block, receiver, 1, completion);
}

/* ---- a `Result`'s members, over its raw value ------------------------------------------------- */

static kt_boolean kt_result_is_failure_value(KRef value) {
    return value != NULL && kt_type_of(value) == &kt_type_result_failure;
}

kt_boolean kt_result_is_success(KRef value) {
    return !kt_result_is_failure_value(value);
}

kt_boolean kt_result_is_failure(KRef value) {
    return kt_result_is_failure_value(value);
}

KRef kt_result_success(KRef value) {
    return value;
}

KRef kt_result_get_or_throw(KRef value) {
    kt_result_throw_on_failure(value);
    return value;
}

KRef kt_result_get_or_null(KRef value) {
    return kt_result_is_failure_value(value) ? NULL : value;
}

KRef kt_result_exception_or_null(KRef value) {
    return kt_result_is_failure_value(value) ? ((KFailure *)value)->exception : NULL;
}

/* ---- `EmptyCoroutineContext` ------------------------------------------------------------------ */

/* The one context there is without a dispatcher library: a program reads it, passes it on and
   prints it, and nothing here asks it for an element. Kotlin's object renders as its name. */
static KRef kt_empty_coroutine_context_to_string(KRef self) {
    (void)self;
    static KRef text;
    static const char name[] = "EmptyCoroutineContext";
    return kt_string_literal(name, (kt_int)(sizeof(name) - 1), &text);
}

static const kt_fn kt_empty_coroutine_context_vtable[] = {
    (kt_fn)kt_any_equals, (kt_fn)kt_any_hash_code, (kt_fn)kt_empty_coroutine_context_to_string};

static const KType kt_type_empty_coroutine_context = {
    KT_NAMED("kotlin.coroutines.", "EmptyCoroutineContext"),
    .instance_size = sizeof(KObjectHeader),
    .super = &kt_type_any,
    .vtable = kt_empty_coroutine_context_vtable,
    .vtable_length = 3};

KRef kt_empty_coroutine_context(void) {
    static KObjectHeader context = {&kt_type_empty_coroutine_context};
    return (KRef)&context;
}

/* ---- a boxed `Result` ----------------------------------------------------------------------- */

/* Where a `Result` is held as `Any`, a type parameter or `Result?`, Kotlin boxes it like any value
   class. The class is the library's, so its box is laid out here. */
typedef struct KResultBox {
    KObjectHeader header;
    KRef value;
} KResultBox;

static const uint32_t kt_result_box_references[] = {offsetof(KResultBox, value)};

/* A boxed value class is equal to another box of the same class holding an equal value. */
static kt_boolean kt_result_box_equals(KRef self, KRef other) {
    return other != NULL && kt_type_of(other) == &kt_type_result &&
           kt_equals(((KResultBox *)self)->value, ((KResultBox *)other)->value);
}

static kt_int kt_result_box_hash_code(KRef self) {
    return kt_hash_code(((KResultBox *)self)->value);
}

/* `Success(value)`, or the failure's own text. */
static KRef kt_result_box_to_string(KRef self) {
    static KRef open, close;
    KRef value = ((KResultBox *)self)->value;
    if (value != NULL && kt_type_of(value) == &kt_type_result_failure) return kt_to_string(value);
    KRef text = kt_string_plus(kt_string_literal("Success(", 8, &open), kt_to_string(value));
    return kt_string_plus(text, kt_string_literal(")", 1, &close));
}

static const kt_fn kt_result_box_vtable[] = {
    (kt_fn)kt_result_box_equals, (kt_fn)kt_result_box_hash_code, (kt_fn)kt_result_box_to_string};

const KType kt_type_result = {KT_NAMED("kotlin.", "Result"),
                              .instance_size = sizeof(KResultBox),
                              .reference_count = 1,
                              .reference_offsets = kt_result_box_references,
                              .super = &kt_type_any,
                              .vtable = kt_result_box_vtable,
                              .vtable_length = 3};

KRef kt_result_box(KRef value) {
    KResultBox *box = kt_gc_allocate(&kt_type_result, sizeof(KResultBox));
    box->value = value;
    return (KRef)box;
}

KRef kt_result_unbox(KRef box) {
    return ((KResultBox *)box)->value;
}
