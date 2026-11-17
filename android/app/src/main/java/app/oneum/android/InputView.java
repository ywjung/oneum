package app.oneum.android;

import android.content.Context;
import android.text.Editable;
import android.text.InputType;
import android.text.Selection;
import android.view.KeyEvent;
import android.view.View;
import android.view.inputmethod.BaseInputConnection;
import android.view.inputmethod.EditorInfo;
import android.view.inputmethod.InputConnection;
import android.view.inputmethod.InputMethodManager;

/**
 * The view the on-screen keyboard types into. The app draws its own text fields; this
 * view only keeps what the input method holds (it composes Korean syllables and changes
 * what it has written) and tells the app the whole of it after every change. The app
 * works out what was typed from the difference (src/platform/android/text_input.rs).
 */
final class InputView extends View {
    private final Editable held = Editable.Factory.getInstance().newEditable("");
    /** A text field of the app is being typed into (it asked for the keyboard). */
    private boolean typing;
    /** What is typed is a password (see {@link #setSecret}). */
    private boolean secret;

    InputView(Context context) {
        super(context);
        setFocusable(true);
        setFocusableInTouchMode(true);
    }

    void setTyping(boolean typing) {
        this.typing = typing;
    }

    /**
     * What is typed now is a password (or no longer is): the input method is told,
     * so that it neither suggests from it nor learns it. It asks anew what kind of
     * field this is.
     */
    void setSecret(boolean secret) {
        if (this.secret == secret) {
            return;
        }
        this.secret = secret;
        InputMethodManager manager = getContext().getSystemService(InputMethodManager.class);
        if (manager != null) {
            manager.restartInput(this);
        }
    }

    /** Starts from nothing: another field is typed into, or this one is done with. */
    void clear() {
        held.clear();
        Selection.setSelection(held, 0);
        InputMethodManager manager = getContext().getSystemService(InputMethodManager.class);
        if (manager != null) {
            manager.restartInput(this);
        }
    }

    @Override
    public boolean onCheckIsTextEditor() {
        return true;
    }

    /**
     * Keys pass here (this view has the window's focus) before the input method and
     * the app's native side get them. The media keys are taken out and sent where they
     * belong. Of the keys of a keyboard, the ones that are the app's shortcuts go to
     * the app; and while no field is typed into, none goes to the input method, which
     * would come up for any of them (also for Ctrl alone) and write into nothing.
     */
    @Override
    public boolean onKeyPreIme(int keyCode, KeyEvent event) {
        if (PlaybackService.mediaKey(event)) {
            return true;
        }
        if (event.isSystem()) {
            // Back, the volume keys and their like go the way they always went.
            return super.onKeyPreIme(keyCode, event);
        }
        int action = event.getAction();
        boolean pressedOrLetGo = action == KeyEvent.ACTION_DOWN || action == KeyEvent.ACTION_UP;
        if (pressedOrLetGo && isShortcutKey(keyCode)
                && (!typing || event.isCtrlPressed() || keyCode == KeyEvent.KEYCODE_ESCAPE)) {
            Native.onHardKey(keyCode, event.getMetaState(),
                    action == KeyEvent.ACTION_DOWN ? 1 : 0,
                    event.getRepeatCount() > 0 ? 1 : 0);
            return true;
        }
        return !typing || super.onKeyPreIme(keyCode, event);
    }

    /** The keys the app's shortcuts are made of (see src/ui/hardkeys.rs). */
    private static boolean isShortcutKey(int keyCode) {
        return (keyCode >= KeyEvent.KEYCODE_A && keyCode <= KeyEvent.KEYCODE_Z)
                || (keyCode >= KeyEvent.KEYCODE_0 && keyCode <= KeyEvent.KEYCODE_9)
                || (keyCode >= KeyEvent.KEYCODE_DPAD_UP && keyCode <= KeyEvent.KEYCODE_DPAD_RIGHT)
                || keyCode == KeyEvent.KEYCODE_SPACE
                || keyCode == KeyEvent.KEYCODE_ENTER
                || keyCode == KeyEvent.KEYCODE_NUMPAD_ENTER
                || keyCode == KeyEvent.KEYCODE_ESCAPE
                || keyCode == KeyEvent.KEYCODE_COMMA
                || keyCode == KeyEvent.KEYCODE_PERIOD
                || keyCode == KeyEvent.KEYCODE_DEL
                || keyCode == KeyEvent.KEYCODE_FORWARD_DEL;
    }

    @Override
    public InputConnection onCreateInputConnection(EditorInfo out) {
        out.inputType = InputType.TYPE_CLASS_TEXT | (secret
                ? InputType.TYPE_TEXT_VARIATION_PASSWORD
                : InputType.TYPE_TEXT_FLAG_NO_SUGGESTIONS);
        out.imeOptions = EditorInfo.IME_ACTION_DONE
                | EditorInfo.IME_FLAG_NO_EXTRACT_UI
                | EditorInfo.IME_FLAG_NO_FULLSCREEN
                | (secret ? EditorInfo.IME_FLAG_NO_PERSONALIZED_LEARNING : 0);
        out.initialSelStart = held.length();
        out.initialSelEnd = held.length();
        return new Connection();
    }

    private final class Connection extends BaseInputConnection {
        Connection() {
            super(InputView.this, true);
        }

        @Override
        public Editable getEditable() {
            return held;
        }

        private void report() {
            Native.onText(held.toString());
        }

        @Override
        public boolean commitText(CharSequence text, int newCursorPosition) {
            boolean done = super.commitText(text, newCursorPosition);
            report();
            return done;
        }

        @Override
        public boolean setComposingText(CharSequence text, int newCursorPosition) {
            boolean done = super.setComposingText(text, newCursorPosition);
            report();
            return done;
        }

        @Override
        public boolean deleteSurroundingText(int before, int after) {
            // What lies before the caret here; the app's field may hold more (text
            // that was there before the keyboard came up): the rest is deleted there.
            int have = Math.max(Selection.getSelectionStart(held), 0);
            boolean done = super.deleteSurroundingText(before, after);
            report();
            for (int i = have; i < before; i++) {
                Native.onKey(1);
            }
            return done;
        }

        @Override
        public boolean deleteSurroundingTextInCodePoints(int before, int after) {
            int have = Math.max(Selection.getSelectionStart(held), 0);
            boolean done = super.deleteSurroundingTextInCodePoints(before, after);
            report();
            for (int i = have; i < before; i++) {
                Native.onKey(1);
            }
            return done;
        }

        @Override
        public boolean sendKeyEvent(KeyEvent event) {
            int code = event.getKeyCode();
            if (code == KeyEvent.KEYCODE_DEL || code == KeyEvent.KEYCODE_ENTER) {
                if (event.getAction() == KeyEvent.ACTION_DOWN) {
                    if (code == KeyEvent.KEYCODE_ENTER) {
                        enter();
                    } else if (held.length() > 0) {
                        finishComposingText();
                        held.delete(held.length() - 1, held.length());
                        report();
                    } else {
                        Native.onKey(1);
                    }
                }
                return true;
            }
            return super.sendKeyEvent(event);
        }

        @Override
        public boolean performEditorAction(int action) {
            enter();
            return true;
        }

        private void enter() {
            finishComposingText();
            report();
            Native.onKey(2);
            clear();
        }
    }
}
