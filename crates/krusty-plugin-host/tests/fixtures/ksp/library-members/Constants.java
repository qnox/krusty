package dependency;

import java.lang.annotation.Retention;
import java.lang.annotation.RetentionPolicy;

@Retention(RetentionPolicy.RUNTIME)
public @interface Constants {
    Mode[] DEFAULT_MODES = {Mode.FIELD};
    String PREFIX = "with";
    int LIMIT = 3;

    Mode[] modes() default {Mode.FIELD};

    String prefix() default PREFIX;

    Entry[] entries() default {};

    enum Mode {
        FIELD,
        METHOD
    }

    @interface Entry {
        String name();
    }
}
