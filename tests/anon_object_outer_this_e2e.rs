use super::common;

fn run_ok(stem: &str, body: &str) {
    common::expect_box_ok_with_stdlib(body, stem);
}

// Regression test for intellij-community's BannerStartPagePromoter.kt: member
// functions of the enclosing class must be callable from inside an anonymous
// object expression, both unqualified and via a labeled `this@Outer`.
// Previously the checker reported `unresolved function 'onBannerShown'`
// because the enclosing class was never added to the anonymous class's
// implicit-receiver chain.

#[test]
fn calls_outer_member_function_unqualified() {
    run_ok(
        "AnonOuterCall",
        "interface Activatable { fun showNotify(): String }\n\
         abstract class Promoter {\n\
         var shown = 0\n\
         fun install(): Activatable {\n\
         return object : Activatable {\n\
         override fun showNotify(): String {\n\
         onBannerShown()\n\
         return \"shown\"\n\
         }\n\
         }\n\
         }\n\
         protected open fun onBannerShown() { shown += 1 }\n\
         }\n\
         class MyPromoter : Promoter()\n\
         fun box(): String {\n\
         val a = MyPromoter().install()\n\
         return if (a.showNotify() == \"shown\") \"OK\" else \"F\" }\n",
    );
}

#[test]
fn calls_outer_member_function_labeled_this() {
    run_ok(
        "AnonOuterLabeled",
        "interface Activatable { fun showNotify(): String }\n\
         abstract class Promoter {\n\
         var shown = 0\n\
         fun install(): Activatable {\n\
         return object : Activatable {\n\
         override fun showNotify(): String {\n\
         this@Promoter.onBannerShown()\n\
         return if (this@Promoter.shown == 1) \"OK\" else \"F\"\n\
         }\n\
         }\n\
         }\n\
         protected open fun onBannerShown() { shown += 1 }\n\
         }\n\
         class MyPromoter : Promoter()\n\
         fun box(): String {\n\
         return MyPromoter().install().showNotify() }\n",
    );
}

#[test]
fn override_dispatches_through_outer_open_function() {
    run_ok(
        "AnonOuterOverride",
        "interface Activatable { fun showNotify(): String }\n\
         abstract class Promoter {\n\
         fun install(): Activatable {\n\
         return object : Activatable {\n\
         override fun showNotify(): String = onBannerShown()\n\
         }\n\
         }\n\
         protected abstract fun onBannerShown(): String\n\
         }\n\
         class MyPromoter : Promoter() {\n\
         override fun onBannerShown(): String = \"OK\"\n\
         }\n\
         fun box(): String = MyPromoter().install().showNotify()\n",
    );
}
