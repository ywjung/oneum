package app.oneum.android;

/**
 * What the Java side tells the app (liboneum.so, see src/platform/android/natives.rs).
 * Called on the main thread; each call only leaves a note for the app's own thread.
 */
final class Native {
    private Native() {}

    /** What the system's bars, the camera cut-out and the keyboard cover, in dp. */
    static native void onInsets(float top, float left, float bottom, float right, float keyboard);

    /** The phone's theme: dark (1) or light (0). */
    static native void onDark(int dark);

    /** The activity came to the screen (1) or left it (0). */
    static native void onForeground(int foreground);

    /** Whether an accessibility service (TalkBack) is on: 1, or 0. */
    static native void onAccessibility(int on);

    /**
     * Something asked of a node of the accessibility tree (by its number): 0 click,
     * 1 focus, 2 scroll forward, 3 scroll back.
     */
    static native void onAccessibilityAction(int id, int action);

    /** What the node TalkBack is on says (null: it is on none of the app's). */
    static native void onAccessibilityFocus(String said);

    /** The text the input method holds for the field that is typed into. */
    static native void onText(String held);

    /** A key that is not text: 1 delete backwards, 2 enter. */
    static native void onKey(int key);

    /**
     * A key of a keyboard (not the on-screen one): its code and meta state, pressed
     * (1) or let go of (0), repeating (1) or not.
     */
    static native void onHardKey(int code, int meta, int pressed, int repeat);

    /** 0 play, 1 pause, 2 play or pause, 3 stop, 4 next, 5 previous, 6 seek (ms). */
    static native void onMedia(int command, long argument);

    /** 1 the sound was taken, 2 given back (play on), 3 given back, 4 output gone. */
    static native void onSound(int event);

    /** What was chosen in the picker asked for under {@code id}: paths, one a line. */
    static native void onPicked(int id, String paths);

    /**
     * Something was chosen in the picker asked for under {@code id} that the app
     * cannot use (a folder it cannot read by a path, files it could not take in).
     */
    static native void onPickFailed(int id);

    /**
     * What became of the file offered under {@code id}: kept where the user wanted it
     * (0), the question put away (1), not written (2).
     */
    static native void onHandedOut(int id, int outcome);

    /** Files another app asked to have opened: paths, one a line. */
    static native void onOpen(String paths);

    /**
     * The activity is gone: for good (1), or to be made anew by the system at once
     * (0). Does not return: the process ends.
     */
    static native void onDestroy(int finishing);
}
