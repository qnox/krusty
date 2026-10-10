package krusty.buildtools;

import org.jetbrains.kotlin.buildtools.api.BuildOperation;
import org.jetbrains.kotlin.buildtools.api.KotlinLogger;

/** A build operation this implementation created, and so knows how to run. */
interface KrustyOperation<R> extends BuildOperation<R> {
    R execute(KotlinLogger logger);
}
