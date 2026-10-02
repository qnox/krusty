use super::common;

#[test]
fn an_inlined_local_delegate_specializes_only_its_reified_parameter() {
    common::expect_box_same_as_kotlinc(
        "interface Item\n\
         class Token : Item\n\
         class Root : Item\n\
         class Delegate<T, R : Item>(val value: Any, val candidate: Item)\n\
         inline operator fun <T, reified R : Item> Delegate<T, R>.getValue(\n\
             owner: Any?, property: Any?\n\
         ): Any? = if (candidate is R) value as? T else null\n\
         inline fun <T, reified R : Item> read(value: Any, candidate: Item): Any? {\n\
             val delegate = Delegate<T, R>(value, candidate)\n\
             val local by delegate\n\
             return local\n\
         }\n\
         fun box(): String {\n\
             val kept = read<Root, Token>(Token(), Token())\n\
             val rejected = read<Root, Token>(Token(), Root())\n\
             return if (kept is Token && rejected == null) \"OK\" else \"Fail\"\n\
         }\n",
        "ReifiedLocalDelegate",
    );
}

#[test]
fn an_escaping_lambda_specializes_the_local_delegate_accessor_it_captures() {
    common::expect_box_same_as_kotlinc(
        "interface Item\n\
         class Token : Item\n\
         class Root : Item\n\
         class Delegate<T, R : Item>(val value: Any, val candidate: Item)\n\
         inline operator fun <T, reified R : Item> Delegate<T, R>.getValue(\n\
             owner: Any?, property: Any?\n\
         ): Any? = if (candidate is R) value as? T else null\n\
         var read: () -> Any? = { null }\n\
         class Installer {\n\
             inline fun <T, reified R : Item> install(value: Any, candidate: Item) {\n\
                 read = {\n\
                     val delegate = Delegate<T, R>(value, candidate)\n\
                     val local by delegate\n\
                     local\n\
                 }\n\
             }\n\
         }\n\
         class Caller {\n\
             fun installKept() = Installer().install<Root, Token>(Token(), Token())\n\
             fun installRejected() = Installer().install<Root, Token>(Token(), Root())\n\
         }\n\
         fun box(): String {\n\
             val caller = Caller()\n\
             caller.installKept()\n\
             val kept = read()\n\
             caller.installRejected()\n\
             return if (kept is Token && read() == null) \"OK\" else \"Fail\"\n\
         }\n",
        "EscapingReifiedLocalDelegate",
    );
}

#[test]
fn a_local_delegate_accessor_specializes_the_lambda_it_returns() {
    common::expect_box_same_as_kotlinc(
        "interface Item\n\
         class Token : Item\n\
         class Root : Item\n\
         class Delegate<T, R : Item>(val value: Any, val candidate: Item)\n\
         inline operator fun <T, reified R : Item> Delegate<T, R>.getValue(\n\
             owner: Any?, property: Any?\n\
         ): () -> Any? = { if (candidate is R) value as? T else null }\n\
         inline fun <T, reified R : Item> make(\n\
             value: Any, candidate: Item\n\
         ): () -> Any? {\n\
             val delegate = Delegate<T, R>(value, candidate)\n\
             val local by delegate\n\
             return local\n\
         }\n\
         fun box(): String {\n\
             val kept = make<Root, Token>(Token(), Token())()\n\
             val rejected = make<Root, Token>(Token(), Root())()\n\
             return if (kept is Token && rejected == null) \"OK\" else \"Fail\"\n\
         }\n",
        "ReifiedLocalDelegateLambdaResult",
    );
}

#[test]
fn an_omitted_noinline_default_specializes_its_local_delegate_plan() {
    common::expect_box_same_as_kotlinc(
        "interface Item\n\
         class Token : Item\n\
         class Root : Item\n\
         class Delegate<T, R : Item>(val value: Any, val candidate: Item)\n\
         inline operator fun <T, reified R : Item> Delegate<T, R>.getValue(\n\
             owner: Any?, property: Any?\n\
         ): Any? = if (candidate is R) value as? T else null\n\
         inline fun <T, reified R : Item> read(\n\
             value: Any,\n\
             candidate: Item,\n\
             noinline action: () -> Any? = {\n\
                 val delegate = Delegate<T, R>(value, candidate)\n\
                 val local by delegate\n\
                 local\n\
             }\n\
         ): Any? = action()\n\
         fun box(): String {\n\
             val kept = read<Root, Token>(Token(), Token())\n\
             val rejected = read<Root, Token>(Token(), Root())\n\
             return if (kept is Token && rejected == null) \"OK\" else \"Fail\"\n\
         }\n",
        "ReifiedLocalDelegateDefaultLambda",
    );
}
