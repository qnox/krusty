/* The function protocol's stubs, which a driver must write because it links no program.

   In a program the final link writes `kt_itf__1_function_<n>` from the functions that lambdas,
   callable references and classes implementing a function type record (`native::interface_calls`).
   A driver's function values are built by hand (`FUNCTION_VALUE` in `driver_exceptions.h`): their
   descriptors hold the one body in the three vtable slots after `kotlin.Any`'s, one per operand
   count the runtime calls back with, so each stub reads the slot of its own operand count. */
#include "krusty_rt.h"

#define DRIVER_FUNCTION_SLOT(operands) (3u + (operands))

static kt_fn driver_function_body(KRef self, unsigned operands) {
    return ((const KObjectHeader *)self)->type->vtable[DRIVER_FUNCTION_SLOT(operands)];
}

KRef kt_itf__1_function_0(KRef self) {
    return ((KRef(*)(KRef))driver_function_body(self, 0))(self);
}

KRef kt_itf__1_function_1(KRef self, KRef first) {
    return ((KRef(*)(KRef, KRef))driver_function_body(self, 1))(self, first);
}

KRef kt_itf__1_function_2(KRef self, KRef first, KRef second) {
    return ((KRef(*)(KRef, KRef, KRef))driver_function_body(self, 2))(
        self, first, second);
}
