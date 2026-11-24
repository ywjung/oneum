package app.oneum.android;

import android.content.Context;
import android.graphics.Rect;
import android.os.Bundle;
import android.util.Log;
import android.util.SparseArray;
import android.view.View;
import android.view.ViewParent;
import android.view.accessibility.AccessibilityEvent;
import android.view.accessibility.AccessibilityManager;
import android.view.accessibility.AccessibilityNodeInfo;
import android.view.accessibility.AccessibilityNodeProvider;

import org.json.JSONArray;
import org.json.JSONException;
import org.json.JSONObject;

import java.util.ArrayList;
import java.util.List;

/**
 * The app's screen as TalkBack sees it. The app draws everything itself on one surface,
 * so the system knows no buttons or texts of it: this view, as large as the screen and
 * drawing nothing, stands for them. It is told the app's accessibility tree, flattened
 * (src/ui/accessible.rs, as JSON), and shows each node of it to the system as a view of
 * its own (a "virtual" view). What TalkBack asks of one (a double tap on a button is a
 * click) goes back to the app by the node's number.
 *
 * Touches never reach this view (the native activity takes them all), so TalkBack's
 * exploring by touch finds nothing here; moving from item to item with swipes, and
 * double taps, work.
 */
final class A11yHost extends View {
    private static final String TAG = "OnEum";

    // A node's bits, as src/ui/accessible.rs writes them.
    private static final int CLICKABLE = 1;
    private static final int FOCUSABLE = 1 << 1;
    private static final int FOCUSED = 1 << 2;
    private static final int CHECKABLE = 1 << 3;
    private static final int CHECKED = 1 << 4;
    private static final int DISABLED = 1 << 5;
    private static final int EDITABLE = 1 << 6;
    private static final int SELECTED = 1 << 7;
    private static final int HEADING = 1 << 8;
    private static final int PASSWORD = 1 << 9;

    // What is asked of the app (src/ui/accessible.rs, actions_for).
    private static final int CLICK = 0;
    private static final int FOCUS = 1;

    /** No node has TalkBack's focus. */
    private static final int NONE = Integer.MIN_VALUE;

    private static final class Node {
        int parent;
        int flags;
        final Rect bounds = new Rect();
        String className;
        String text;
        String hint;
        double[] range;
        final List<Integer> children = new ArrayList<>();
    }

    private final SparseArray<Node> nodes = new SparseArray<>();
    /** The nodes right under the view, in reading order. */
    private final List<Integer> top = new ArrayList<>();
    /** The node with the keyboard's focus (a field that is typed into), or -1. */
    private int focus = -1;
    /** The node TalkBack is on, or NONE. */
    private int accessibilityFocus = NONE;
    private boolean changeWaiting;
    private final int[] origin = new int[2];

    A11yHost(Context context) {
        super(context);
        setImportantForAccessibility(IMPORTANT_FOR_ACCESSIBILITY_YES);
        setFocusable(false);
    }

    /** A new tree from the app (main thread). */
    void update(String json) {
        int focusBefore = focus;
        nodes.clear();
        top.clear();
        try {
            JSONObject tree = new JSONObject(json);
            focus = tree.optInt("focus", -1);
            JSONArray list = tree.getJSONArray("nodes");
            for (int i = 0; i < list.length(); i++) {
                JSONObject item = list.getJSONObject(i);
                Node node = new Node();
                int id = item.getInt("id");
                node.parent = item.getInt("parent");
                node.flags = item.getInt("flags");
                JSONArray b = item.getJSONArray("bounds");
                node.bounds.set(b.getInt(0), b.getInt(1), b.getInt(2), b.getInt(3));
                node.className = item.getString("class");
                node.text = item.optString("text", "");
                node.hint = item.optString("hint", "");
                JSONArray range = item.optJSONArray("range");
                if (range != null && range.length() == 3) {
                    node.range = new double[] {range.getDouble(0), range.getDouble(1),
                            range.getDouble(2)};
                }
                nodes.put(id, node);
                // (The list is in reading order: a parent comes before its children.)
                Node parent = nodes.get(node.parent);
                if (parent != null) {
                    parent.children.add(id);
                } else {
                    top.add(id);
                }
            }
        } catch (JSONException error) {
            Log.w(TAG, "the accessibility tree cannot be read", error);
            return;
        }
        if (accessibilityFocus != NONE && nodes.get(accessibilityFocus) == null) {
            accessibilityFocus = NONE;
        }
        // The system is told that something changed at most every tenth of a second:
        // the app's screen changes with every frame while a song plays (its time).
        if (!changeWaiting) {
            changeWaiting = true;
            postDelayed(() -> {
                changeWaiting = false;
                AccessibilityEvent event =
                        new AccessibilityEvent(AccessibilityEvent.TYPE_WINDOW_CONTENT_CHANGED);
                event.setContentChangeTypes(AccessibilityEvent.CONTENT_CHANGE_TYPE_SUBTREE);
                sendForHost(event);
            }, 100);
        }
        if (focus != focusBefore && focus >= 0) {
            send(focus, AccessibilityEvent.TYPE_VIEW_FOCUSED);
        }
    }

    @Override
    public AccessibilityNodeProvider getAccessibilityNodeProvider() {
        return provider;
    }

    private void sendForHost(AccessibilityEvent event) {
        ViewParent parent = getParent();
        if (parent == null || !isAttachedToWindow()) {
            return;
        }
        event.setPackageName(getContext().getPackageName());
        event.setSource(this);
        deliver(parent, event);
    }

    /**
     * Hands an event to the system, if a screen reader (or another accessibility
     * service) is still there to hear it. Events are sent a moment after the screen
     * changed, and the service may have been switched off by then: the system answers
     * an event it has no one for with an exception ("Accessibility off"), which ended
     * the app. The switch is thrown on another thread, so it can also fall between
     * the question and the sending: that one exception is caught.
     */
    private void deliver(ViewParent parent, AccessibilityEvent event) {
        AccessibilityManager manager =
                (AccessibilityManager) getContext().getSystemService(Context.ACCESSIBILITY_SERVICE);
        if (manager == null || !manager.isEnabled()) {
            return;
        }
        try {
            parent.requestSendAccessibilityEvent(this, event);
        } catch (IllegalStateException off) {
            Log.i(TAG, "accessibility was switched off as an event was sent");
        }
    }

    private void send(int id, int type) {
        ViewParent parent = getParent();
        Node node = nodes.get(id);
        if (parent == null || node == null || !isAttachedToWindow()) {
            return;
        }
        AccessibilityEvent event = new AccessibilityEvent(type);
        event.setPackageName(getContext().getPackageName());
        event.setClassName(node.className);
        event.setSource(this, id);
        if (!node.text.isEmpty()) {
            event.getText().add(node.text);
        }
        deliver(parent, event);
    }

    /**
     * For verification (src/ui/automation.rs, "a11y"): asks of the node that says
     * {@code said} what TalkBack would, moving onto it (1) and a double tap (2).
     * Says whether there is such a node and it took what was asked.
     */
    boolean ask(String said, int what) {
        for (int i = 0; i < nodes.size(); i++) {
            Node node = nodes.valueAt(i);
            if (said.equals(node.text) || said.equals(node.hint)) {
                int id = nodes.keyAt(i);
                boolean on = accessibilityFocus == id || provider.performAction(id,
                        AccessibilityNodeInfo.ACTION_ACCESSIBILITY_FOCUS, null);
                return what == 1 ? on
                        : on && provider.performAction(id, AccessibilityNodeInfo.ACTION_CLICK, null);
            }
        }
        return false;
    }

    private final AccessibilityNodeProvider provider = new AccessibilityNodeProvider() {
        @Override
        public AccessibilityNodeInfo createAccessibilityNodeInfo(int id) {
            A11yHost host = A11yHost.this;
            getLocationOnScreen(origin);
            if (id == HOST_VIEW_ID) {
                AccessibilityNodeInfo info = new AccessibilityNodeInfo(host);
                onInitializeAccessibilityNodeInfo(info);
                for (int child : top) {
                    info.addChild(host, child);
                }
                // (Not scrollable: TalkBack would take each child of a list for one item,
                // the whole screen among them. A list is moved with two fingers.)
                return info;
            }
            Node node = nodes.get(id);
            if (node == null) {
                return null;
            }
            AccessibilityNodeInfo info = new AccessibilityNodeInfo(host, id);
            // (A node made here is not taken for important by itself: TalkBack would
            // pass over it.)
            info.setImportantForAccessibility(true);
            info.setPackageName(getContext().getPackageName());
            info.setClassName(node.className);
            if (nodes.get(node.parent) != null) {
                info.setParent(host, node.parent);
            } else {
                info.setParent(host);
            }
            for (int child : node.children) {
                info.addChild(host, child);
            }
            Rect screen = new Rect(node.bounds);
            screen.offset(origin[0], origin[1]);
            info.setBoundsInScreen(screen);
            info.setVisibleToUser(!node.bounds.isEmpty()
                    && Rect.intersects(node.bounds, new Rect(0, 0, getWidth(), getHeight())));
            if (!node.text.isEmpty()) {
                info.setText(node.text);
            }
            if (!node.hint.isEmpty()) {
                info.setHintText(node.hint);
            }
            int flags = node.flags;
            boolean enabled = (flags & DISABLED) == 0;
            info.setEnabled(enabled);
            boolean clickable = (flags & CLICKABLE) != 0;
            info.setClickable(clickable);
            if (clickable && enabled) {
                info.addAction(AccessibilityNodeInfo.AccessibilityAction.ACTION_CLICK);
            }
            boolean focusable = (flags & FOCUSABLE) != 0;
            boolean focused = (flags & FOCUSED) != 0;
            info.setFocusable(focusable);
            info.setFocused(focused);
            if (focusable && !focused) {
                info.addAction(AccessibilityNodeInfo.AccessibilityAction.ACTION_FOCUS);
            }
            info.setCheckable((flags & CHECKABLE) != 0);
            info.setChecked((flags & CHECKED) != 0);
            info.setEditable((flags & EDITABLE) != 0);
            info.setSelected((flags & SELECTED) != 0);
            info.setHeading((flags & HEADING) != 0);
            info.setPassword((flags & PASSWORD) != 0);
            if (node.range != null) {
                info.setRangeInfo(new AccessibilityNodeInfo.RangeInfo(
                        AccessibilityNodeInfo.RangeInfo.RANGE_TYPE_FLOAT,
                        (float) node.range[1], (float) node.range[2], (float) node.range[0]));
            }
            if (id == accessibilityFocus) {
                info.setAccessibilityFocused(true);
                info.addAction(AccessibilityNodeInfo.AccessibilityAction
                        .ACTION_CLEAR_ACCESSIBILITY_FOCUS);
            } else {
                info.addAction(AccessibilityNodeInfo.AccessibilityAction.ACTION_ACCESSIBILITY_FOCUS);
            }
            return info;
        }

        @Override
        public boolean performAction(int id, int action, Bundle arguments) {
            if (id == HOST_VIEW_ID) {
                return performAccessibilityAction(action, arguments);
            }
            Node node = nodes.get(id);
            if (node == null) {
                return false;
            }
            switch (action) {
                case AccessibilityNodeInfo.ACTION_ACCESSIBILITY_FOCUS:
                    if (accessibilityFocus == id) {
                        return false;
                    }
                    if (accessibilityFocus != NONE) {
                        send(accessibilityFocus,
                                AccessibilityEvent.TYPE_VIEW_ACCESSIBILITY_FOCUS_CLEARED);
                    }
                    accessibilityFocus = id;
                    send(id, AccessibilityEvent.TYPE_VIEW_ACCESSIBILITY_FOCUSED);
                    Native.onAccessibilityFocus(node.text.isEmpty() ? node.hint : node.text);
                    return true;
                case AccessibilityNodeInfo.ACTION_CLEAR_ACCESSIBILITY_FOCUS:
                    if (accessibilityFocus != id) {
                        return false;
                    }
                    accessibilityFocus = NONE;
                    send(id, AccessibilityEvent.TYPE_VIEW_ACCESSIBILITY_FOCUS_CLEARED);
                    Native.onAccessibilityFocus(null);
                    return true;
                case AccessibilityNodeInfo.ACTION_CLICK:
                    if ((node.flags & CLICKABLE) == 0 || (node.flags & DISABLED) != 0) {
                        return false;
                    }
                    // (A field takes the keyboard's focus by a tap, as a field of the
                    // system's does; the UI library wants to be asked for it.)
                    if ((node.flags & EDITABLE) != 0) {
                        Native.onAccessibilityAction(id, FOCUS);
                    }
                    Native.onAccessibilityAction(id, CLICK);
                    send(id, AccessibilityEvent.TYPE_VIEW_CLICKED);
                    return true;
                case AccessibilityNodeInfo.ACTION_FOCUS:
                    if ((node.flags & FOCUSABLE) == 0) {
                        return false;
                    }
                    Native.onAccessibilityAction(id, FOCUS);
                    return true;
                default:
                    return false;
            }
        }

        @Override
        public AccessibilityNodeInfo findFocus(int focusType) {
            int id = focusType == AccessibilityNodeInfo.FOCUS_ACCESSIBILITY
                    ? accessibilityFocus : focus;
            if (id == NONE || id < 0 || nodes.get(id) == null) {
                return null;
            }
            return createAccessibilityNodeInfo(id);
        }
    };
}
