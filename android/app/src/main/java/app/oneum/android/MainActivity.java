package app.oneum.android;

import android.Manifest;
import android.app.AlertDialog;
import android.app.NativeActivity;
import android.content.ContentResolver;
import android.content.ContentUris;
import android.content.Intent;
import android.content.pm.ApplicationInfo;
import android.content.pm.PackageManager;
import android.content.res.Configuration;
import android.database.Cursor;
import android.graphics.Insets;
import android.net.Uri;
import android.os.Build;
import android.os.Bundle;
import android.os.Environment;
import android.provider.DocumentsContract;
import android.provider.MediaStore;
import android.provider.OpenableColumns;
import android.provider.Settings;
import android.system.ErrnoException;
import android.system.Os;
import android.util.Log;
import android.util.SparseIntArray;
import android.view.View;
import android.view.ViewGroup;
import android.view.Window;
import android.view.WindowInsets;
import android.view.WindowInsetsController;
import android.view.WindowManager;
import android.view.inputmethod.InputMethodManager;

import java.io.File;
import java.io.FileInputStream;
import java.io.FileOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.nio.ByteBuffer;
import java.nio.charset.CharacterCodingException;
import java.nio.charset.Charset;
import java.nio.charset.CodingErrorAction;
import java.util.ArrayList;
import java.util.List;
import java.util.Locale;
import java.util.concurrent.ExecutionException;
import java.util.concurrent.FutureTask;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.TimeoutException;

/**
 * The one screen of OnEum. The app itself is liboneum.so (Rust), which this native
 * activity loads and which draws the whole screen. This class is what the app cannot
 * do without Java: it tells the app its folders, the margins the system keeps, the
 * theme and what the keyboard types, and it does for the app what only Java offers
 * (pickers, the settings page for storage access, the playback notification).
 * The methods below "for the app" are called from Rust (src/platform/android/java.rs),
 * on a thread of the app's; what must happen on the main thread is posted there.
 */
public final class MainActivity extends NativeActivity {
    private static final String TAG = "OnEum";
    /** Request code of "where to keep this file"; pickers use the ids the app gives. */
    private static final int HAND_OUT = 1;
    private static final int ASK_NOTIFICATIONS = 2;
    /** The first id the app gives a picker (see files.rs: they count up from 1). */
    private static final int PICK_BASE = 1000;

    static {
        // The activity loads the library by itself; loading it here as well is what
        // lets Java find the `native` methods of `Native` in it.
        System.loadLibrary("oneum");
    }

    /**
     * The app is there: its screen was made and has not gone for good. What stays on
     * the home screen when it is not (the widget) asks this before it tells the app
     * anything.
     */
    static volatile boolean up;

    private InputView input;
    /** Which kind of picker was shown under which request code. */
    private final SparseIntArray picking = new SparseIntArray();
    private String handingOut;
    private boolean askedForNotifications;
    /** 0: the app follows the phone's theme, 1: it is light, 2: it is dark. */
    private int appearance;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        recordCrashes();
        // Off the main thread: the system's record of a crash can be large, and nothing
        // reads the result before the app is up.
        new Thread(this::recordLastExits, "exits").start();
        tellEnvironment();
        super.onCreate(savedInstanceState);
        up = true;
        Window window = getWindow();
        // The app draws under the system's bars and keeps clear of them itself.
        window.setDecorFitsSystemWindows(false);
        WindowManager.LayoutParams attributes = window.getAttributes();
        attributes.layoutInDisplayCutoutMode =
                WindowManager.LayoutParams.LAYOUT_IN_DISPLAY_CUTOUT_MODE_SHORT_EDGES;
        window.setAttributes(attributes);

        // The keyboard comes up when the app asks for it, not by itself (the native
        // activity leaves that to the system, which shows it for a view that takes text).
        window.setSoftInputMode(WindowManager.LayoutParams.SOFT_INPUT_STATE_ALWAYS_HIDDEN
                | WindowManager.LayoutParams.SOFT_INPUT_ADJUST_RESIZE);
        input = new InputView(this);
        addContentView(input, new ViewGroup.LayoutParams(1, 1));
        input.requestFocus();

        window.getDecorView().setOnApplyWindowInsetsListener((view, insets) -> {
            tellInsets(insets);
            return insets;
        });
        tellTheme(getResources().getConfiguration());
        open(getIntent());
    }

    /**
     * An exception nothing caught ends the app. What it was is written beside the app's
     * log first, so that "Save the Log" has it: on a phone nobody can look at the
     * system's own record, and the app's log has only what its native side said.
     */
    private void recordCrashes() {
        final File record = new File(new File(getFilesDir(), "logs"), "crash-java.txt");
        final Thread.UncaughtExceptionHandler next = Thread.getDefaultUncaughtExceptionHandler();
        Thread.setDefaultUncaughtExceptionHandler((thread, error) -> {
            try {
                record.getParentFile().mkdirs();
                // Kept short: the last few are what is asked about.
                boolean append = record.length() < 64 * 1024;
                try (java.io.FileWriter out = new java.io.FileWriter(record, append)) {
                    out.write(new java.util.Date() + " on thread " + thread.getName() + "\n"
                            + Log.getStackTraceString(error) + "\n");
                }
            } catch (Throwable ignored) {
                // Nothing more can be done for a crash that cannot be written down.
            }
            if (next != null) {
                next.uncaughtException(thread, error);
            }
        });
    }

    /**
     * How the app's earlier runs ended, as the system remembers it (a crash of the Java
     * side, one of the native side, not answering, ended by the user or for memory),
     * written beside the app's log for "Save the Log". A crash of the native side
     * leaves nothing else that can be read on the phone: for the latest of them the
     * signal and where the crashed thread was follow, on lines that begin with a tab
     * (see {@link Tombstone}).
     */
    private void recordLastExits() {
        try {
            android.app.ActivityManager manager =
                    getSystemService(android.app.ActivityManager.class);
            StringBuilder text = new StringBuilder();
            int detailed = 0;
            for (android.app.ApplicationExitInfo exit
                    : manager.getHistoricalProcessExitReasons(null, 0, 8)) {
                String reason;
                switch (exit.getReason()) {
                    case android.app.ApplicationExitInfo.REASON_CRASH:
                        reason = "crash (Java)";
                        break;
                    case android.app.ApplicationExitInfo.REASON_CRASH_NATIVE:
                        reason = "crash (native)";
                        break;
                    case android.app.ApplicationExitInfo.REASON_ANR:
                        reason = "not answering";
                        break;
                    case android.app.ApplicationExitInfo.REASON_LOW_MEMORY:
                        reason = "low memory";
                        break;
                    case android.app.ApplicationExitInfo.REASON_SIGNALED:
                        reason = "signal " + exit.getStatus();
                        break;
                    case android.app.ApplicationExitInfo.REASON_EXIT_SELF:
                        reason = "exit " + exit.getStatus();
                        break;
                    case android.app.ApplicationExitInfo.REASON_USER_REQUESTED:
                    case android.app.ApplicationExitInfo.REASON_USER_STOPPED:
                        reason = "ended by the user";
                        break;
                    default:
                        reason = "reason " + exit.getReason();
                        break;
                }
                String description = exit.getDescription();
                text.append(new java.util.Date(exit.getTimestamp())).append("  ").append(reason)
                        .append(description == null ? "" : "  " + description).append('\n');
                if (exit.getReason() == android.app.ApplicationExitInfo.REASON_CRASH_NATIVE
                        && detailed < 2) {
                    detailed += 1;
                    for (String line : crashDetails(exit)) {
                        text.append('\t').append(line).append('\n');
                    }
                }
            }
            File record = new File(new File(getFilesDir(), "logs"), "exits.txt");
            record.getParentFile().mkdirs();
            try (java.io.FileWriter out = new java.io.FileWriter(record, false)) {
                out.write(text.toString());
            }
        } catch (IOException | RuntimeException error) {
            Log.w(TAG, "cannot write down how the app ended before", error);
        }
    }

    /** What the system kept of a crash of the native side, in a few lines. */
    private static List<String> crashDetails(android.app.ApplicationExitInfo exit) {
        List<String> lines = new ArrayList<>();
        try (InputStream in = exit.getTraceInputStream()) {
            if (in == null) {
                lines.add("(the system kept no record of it)");
                return lines;
            }
            java.io.ByteArrayOutputStream record = new java.io.ByteArrayOutputStream();
            byte[] part = new byte[64 * 1024];
            int count;
            // A record is a few hundred kilobytes; one far larger is not read to its end.
            while ((count = in.read(part)) > 0 && record.size() < 32 * 1024 * 1024) {
                record.write(part, 0, count);
            }
            byte[] bytes = record.toByteArray();
            try {
                lines.addAll(Tombstone.summary(bytes, 32));
            } catch (IOException | RuntimeException error) {
                // Not the form this was written for: how it begins tells which it is.
                StringBuilder start = new StringBuilder();
                for (int i = 0; i < Math.min(bytes.length, 24); i++) {
                    start.append(String.format("%02x", bytes[i]));
                }
                lines.add("(the system's record of it cannot be read: " + error.getMessage()
                        + "; it begins " + start + ")");
            }
        } catch (IOException | RuntimeException error) {
            lines.add("(the system's record of it cannot be had: " + error + ")");
        }
        return lines;
    }

    /**
     * The app's folders, told through the environment before the library runs. A
     * debuggable build also takes what a verification run passes as extras of the
     * launching intent (ONEUM_SCRIPT and the like).
     */
    private void tellEnvironment() {
        try {
            Os.setenv("ONEUM_DATA_DIR", getFilesDir().getAbsolutePath(), false);
            Os.setenv("TMPDIR", getCacheDir().getAbsolutePath(), true);
            Os.setenv("ONEUM_MUSIC_DIR", Environment
                    .getExternalStoragePublicDirectory(Environment.DIRECTORY_MUSIC)
                    .getAbsolutePath(), false);
            Os.setenv("ONEUM_IMPORT_DIR", importedFolder().getAbsolutePath(), true);
            boolean debuggable =
                    (getApplicationInfo().flags & ApplicationInfo.FLAG_DEBUGGABLE) != 0;
            Bundle extras = getIntent() == null ? null : getIntent().getExtras();
            if (debuggable && extras != null) {
                for (String key : extras.keySet()) {
                    Object value = extras.get(key);
                    if (key.startsWith("ONEUM_") && value != null) {
                        Os.setenv(key, value.toString(), true);
                    }
                    if (key.equals("ONEUM_PAUSED_FOREGROUND_MS") && value != null) {
                        // A verification run does not wait ten minutes to see the
                        // paused service leave the foreground.
                        try {
                            PlaybackService.pausedForegroundMs = Math.max(
                                    1000L, Math.min(3_600_000L, Long.parseLong(value.toString())));
                        } catch (NumberFormatException error) {
                            Log.w(TAG, "not a time: " + value, error);
                        }
                    }
                }
            }
        } catch (ErrnoException error) {
            Log.w(TAG, "cannot tell the app its folders", error);
        }
    }

    private void tellInsets(WindowInsets insets) {
        float density = getResources().getDisplayMetrics().density;
        Insets bars = insets.getInsets(
                WindowInsets.Type.systemBars() | WindowInsets.Type.displayCutout());
        Insets keyboard = insets.getInsets(WindowInsets.Type.ime());
        Native.onInsets(bars.top / density, bars.left / density, bars.bottom / density,
                bars.right / density, keyboard.bottom / density);
    }

    private void tellTheme(Configuration configuration) {
        int night = configuration.uiMode & Configuration.UI_MODE_NIGHT_MASK;
        Native.onDark(night == Configuration.UI_MODE_NIGHT_YES ? 1 : 0);
    }

    @Override
    public void onConfigurationChanged(Configuration configuration) {
        super.onConfigurationChanged(configuration);
        tellTheme(configuration);
        showBars();
    }

    @Override
    protected void onStart() {
        super.onStart();
        Native.onForeground(1);
    }

    @Override
    protected void onStop() {
        Native.onForeground(0);
        super.onStop();
    }

    @Override
    protected void onDestroy() {
        // The app cannot start a second time in one process: when its screen goes for
        // good, it saves what it keeps and the process ends (this call does not return).
        up = false;
        NowPlayingWidget.appGone(this);
        PlaybackService.end();
        Native.onDestroy();
        super.onDestroy();
    }

    @Override
    protected void onNewIntent(Intent intent) {
        super.onNewIntent(intent);
        setIntent(intent);
        open(intent);
    }

    // ----- for the app ------------------------------------------------------------------

    /** The language the phone is set to ("ko", "en"). */
    public String language() {
        return Locale.getDefault().getLanguage();
    }

    /** Bytes in the Korean code page 949 as text; null if they are not that. */
    public String decodeKorean(byte[] bytes) {
        try {
            Charset korean = Charset.isSupported("x-windows-949")
                    ? Charset.forName("x-windows-949") : Charset.forName("EUC-KR");
            return korean.newDecoder()
                    .onMalformedInput(CodingErrorAction.REPORT)
                    .onUnmappableCharacter(CodingErrorAction.REPORT)
                    .decode(ByteBuffer.wrap(bytes))
                    .toString();
        } catch (CharacterCodingException | IllegalArgumentException error) {
            return null;
        }
    }

    public boolean openWebPage(String address) {
        try {
            startActivity(new Intent(Intent.ACTION_VIEW, Uri.parse(address))
                    .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK));
            return true;
        } catch (RuntimeException error) {
            Log.w(TAG, "no app opens the web page", error);
            return false;
        }
    }

    public void alert(String title, String body) {
        runOnUiThread(() -> {
            if (!isFinishing()) {
                // What went wrong can be copied, to be passed on as it is written.
                new AlertDialog.Builder(this)
                        .setTitle(title)
                        .setMessage(body)
                        .setPositiveButton(android.R.string.ok, null)
                        .setNeutralButton(android.R.string.copy,
                                (dialog, which) -> putOnClipboard(title + "\n" + body))
                        .show();
            }
        });
    }

    /** For the app: keeps the password of a network folder (see {@link Secrets}). */
    public boolean secretPut(String id, String secret) {
        return Secrets.put(getApplicationContext(), id, secret);
    }

    /** For the app: the password kept under this name, or null. */
    public String secretGet(String id) {
        return Secrets.get(getApplicationContext(), id);
    }

    /** For the app: forgets the password kept under this name. */
    public void secretRemove(String id) {
        Secrets.remove(getApplicationContext(), id);
    }

    /** For the app: puts a text on the system's clipboard (the app has none of its own). */
    public void copyText(String text) {
        runOnUiThread(() -> putOnClipboard(text));
    }

    private void putOnClipboard(String text) {
        try {
            android.content.ClipboardManager clipboard =
                    getSystemService(android.content.ClipboardManager.class);
            clipboard.setPrimaryClip(android.content.ClipData.newPlainText("OnEum", text));
        } catch (RuntimeException error) {
            // (A text too large for the system to pass between apps, for one.)
            Log.w(TAG, "cannot copy the text", error);
        }
    }

    /**
     * How the app looks: 0 as the phone is set, 1 light, 2 dark. The icons of the
     * system's bars are drawn to be read on it (light ones on a dark app).
     */
    public void setAppearance(int mode) {
        runOnUiThread(() -> {
            appearance = mode;
            showBars();
        });
    }

    private void showBars() {
        int night = getResources().getConfiguration().uiMode & Configuration.UI_MODE_NIGHT_MASK;
        boolean dark = appearance == 2
                || (appearance == 0 && night == Configuration.UI_MODE_NIGHT_YES);
        WindowInsetsController controller = getWindow().getInsetsController();
        if (controller != null) {
            int light = WindowInsetsController.APPEARANCE_LIGHT_STATUS_BARS
                    | WindowInsetsController.APPEARANCE_LIGHT_NAVIGATION_BARS;
            controller.setSystemBarsAppearance(dark ? 0 : light, light);
        }
    }

    public void showKeyboard(boolean show) {
        runOnUiThread(() -> {
            InputMethodManager manager = getSystemService(InputMethodManager.class);
            if (manager == null) {
                return;
            }
            input.setTyping(show);
            if (show) {
                input.requestFocus();
                input.clear();
                manager.showSoftInput(input, 0);
            } else {
                manager.hideSoftInputFromWindow(input.getWindowToken(), 0);
            }
        });
    }

    /** The input method starts from nothing (the field was changed by a touch). */
    public void resetKeyboard() {
        runOnUiThread(() -> input.clear());
    }

    /**
     * For the app: it cannot go on in this process (its graphics device is gone) and is
     * about to end itself. {@link RelaunchActivity}, in a process of its own, opens the
     * app again once this process is gone. Not posted to the main thread: the app ends
     * right after this call.
     */
    public void relaunch() {
        PlaybackService.end();
        startActivity(new Intent(this, RelaunchActivity.class)
                .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
                .putExtra(RelaunchActivity.PID, android.os.Process.myPid()));
    }

    /** For the app: a line the system shows for a moment, also once the app is gone. */
    public void notice(String text) {
        runOnUiThread(() -> android.widget.Toast.makeText(
                getApplicationContext(), text, android.widget.Toast.LENGTH_LONG).show());
    }

    /** Leaves the app without ending it: it goes behind what was there before. */
    public void moveToBack() {
        runOnUiThread(() -> moveTaskToBack(true));
    }

    public boolean hasAllFilesAccess() {
        return Environment.isExternalStorageManager();
    }

    public void askAllFilesAccess() {
        runOnUiThread(() -> {
            try {
                startActivity(new Intent(Settings.ACTION_MANAGE_APP_ALL_FILES_ACCESS_PERMISSION,
                        Uri.parse("package:" + getPackageName())));
            } catch (RuntimeException error) {
                // Some phones have only the list of all apps.
                try {
                    startActivity(new Intent(Settings.ACTION_MANAGE_ALL_FILES_ACCESS_PERMISSION));
                } catch (RuntimeException second) {
                    Log.w(TAG, "no settings page for all files access", second);
                }
            }
        });
    }

    /** Shows the system's picker: 0 a folder, 1 music files, 2 one file of any kind. */
    public void pick(int kind, int id) {
        runOnUiThread(() -> {
            Intent intent;
            if (kind == 0) {
                intent = new Intent(Intent.ACTION_OPEN_DOCUMENT_TREE);
            } else {
                intent = new Intent(Intent.ACTION_OPEN_DOCUMENT)
                        .addCategory(Intent.CATEGORY_OPENABLE)
                        .setType(kind == 1 ? "audio/*" : "*/*")
                        .putExtra(Intent.EXTRA_ALLOW_MULTIPLE, kind == 1);
            }
            try {
                picking.put(PICK_BASE + id, kind);
                startActivityForResult(intent, PICK_BASE + id);
            } catch (RuntimeException error) {
                Log.w(TAG, "no picker", error);
                picking.delete(PICK_BASE + id);
                Native.onPicked(id, "");
            }
        });
    }

    /** Asks where to keep the file at {@code path} and copies it there. */
    public void handOut(String path, String name) {
        runOnUiThread(() -> {
            handingOut = path;
            Intent intent = new Intent(Intent.ACTION_CREATE_DOCUMENT)
                    .addCategory(Intent.CATEGORY_OPENABLE)
                    .setType(name.endsWith(".m3u8") || name.endsWith(".m3u")
                            ? "audio/x-mpegurl" : "application/octet-stream")
                    .putExtra(Intent.EXTRA_TITLE, name);
            try {
                startActivityForResult(intent, HAND_OUT);
            } catch (RuntimeException error) {
                Log.w(TAG, "no place to keep a file", error);
            }
        });
    }

    /** What plays, for the notification and the lock screen. Times in milliseconds. */
    /**
     * The app is about to make sound (it calls this from its own thread, not the main
     * one): true if the sound is this app's or is now, false while something else
     * keeps it. See PlaybackService.claim.
     */
    public boolean claimSound() {
        if (PlaybackService.hasSound()) {
            return true;
        }
        FutureTask<Boolean> asked =
                new FutureTask<>(() -> PlaybackService.claim(getApplicationContext()));
        runOnUiThread(asked);
        try {
            return asked.get(1500, TimeUnit.MILLISECONDS);
        } catch (InterruptedException | ExecutionException | TimeoutException error) {
            // The main thread is busy: the music is not held back for it. The service
            // asks when it hears of the playing (PlaybackService.show).
            Log.w(TAG, "could not ask for the sound in time", error);
            return true;
        }
    }

    /** The app's answer to the end of an interruption: it plays on, or it does not. */
    public void soundSettled(boolean resumed) {
        runOnUiThread(() -> PlaybackService.settled(resumed));
    }

    public void updatePlayback(String title, String artist, String album, long durationMs,
            long positionMs, boolean playing, float speed, String artPath) {
        PlaybackService.State state = new PlaybackService.State(
                title, artist, album, durationMs, positionMs, playing, speed, artPath);
        runOnUiThread(() -> {
            if (playing) {
                askForNotifications();
            }
            PlaybackService.update(this, state);
        });
    }

    public void stopPlayback() {
        runOnUiThread(() -> {
            PlaybackService.end();
            // (The notification goes with the service; the widgets are told.)
            NowPlayingWidget.nothing(this);
        });
    }

    // ----- pickers, files from other apps -----------------------------------------------

    /** Once, when music first plays: without the right the notification is not shown. */
    private void askForNotifications() {
        if (askedForNotifications || Build.VERSION.SDK_INT < 33) {
            return;
        }
        askedForNotifications = true;
        if (checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS)
                != PackageManager.PERMISSION_GRANTED) {
            requestPermissions(
                    new String[] {Manifest.permission.POST_NOTIFICATIONS}, ASK_NOTIFICATIONS);
        }
    }

    @Override
    protected void onActivityResult(int request, int result, Intent data) {
        super.onActivityResult(request, result, data);
        if (request == HAND_OUT) {
            String from = handingOut;
            handingOut = null;
            if (result == RESULT_OK && data != null && data.getData() != null && from != null) {
                Uri to = data.getData();
                new Thread(() -> copyOut(new File(from), to), "oneum-export").start();
            }
            return;
        }
        int index = picking.indexOfKey(request);
        if (index < 0) {
            return;
        }
        int kind = picking.valueAt(index);
        picking.removeAt(index);
        int id = request - PICK_BASE;
        List<Uri> chosen = new ArrayList<>();
        if (result == RESULT_OK && data != null) {
            if (data.getClipData() != null) {
                for (int i = 0; i < data.getClipData().getItemCount(); i++) {
                    chosen.add(data.getClipData().getItemAt(i).getUri());
                }
            } else if (data.getData() != null) {
                chosen.add(data.getData());
            }
        }
        if (chosen.isEmpty()) {
            Native.onPicked(id, "");
        } else if (kind == 0) {
            String folder = folderPath(chosen.get(0));
            boolean told = folder != null && folder.indexOf('\n') < 0 && folder.indexOf('\r') < 0;
            Native.onPicked(id, told ? folder : "");
        } else {
            // Music that was chosen is kept; a file that is read once (a playlist, a
            // correction file) is not.
            boolean keep = kind == 1;
            new Thread(() -> Native.onPicked(id, pathsOf(chosen, keep)), "oneum-import")
                    .start();
        }
    }

    /**
     * The path of a folder chosen in the system's picker, for the folders of shared
     * storage (the phone's own, a card, a drive that is plugged in): the picker hands
     * out an address, and with "all files access" the folder is read by its path.
     */
    private String folderPath(Uri tree) {
        if (!"com.android.externalstorage.documents".equals(tree.getAuthority())) {
            return null;
        }
        String id = DocumentsContract.getTreeDocumentId(tree);
        int colon = id.indexOf(':');
        if (colon < 0) {
            return null;
        }
        String volume = id.substring(0, colon);
        String below = id.substring(colon + 1);
        File root = "primary".equalsIgnoreCase(volume)
                ? Environment.getExternalStorageDirectory()
                : new File("/storage", volume);
        return (below.isEmpty() ? root : new File(root, below)).getAbsolutePath();
    }

    /** Copies what the addresses name into the app's cache; the paths, one a line. */
    /**
     * Where the app keeps its copies of music it cannot read where it is (a file of a
     * cloud drive, of another app). Not in the cache: the system empties that when
     * the phone runs short of room, and the songs would be gone from the library.
     * The app deletes a copy when its song is removed from the library.
     */
    private File importedFolder() {
        return new File(getFilesDir(), "Imported");
    }

    /**
     * Paths the app can read for these files, one a line: the files themselves where
     * they lie in shared storage and can be read there (then they are used where they
     * are, like the music folders), copies of the others. With `keep`, the copies of
     * music are kept (see importedFolder); otherwise they are in the cache.
     */
    private String pathsOf(List<Uri> from, boolean keep) {
        StringBuilder paths = new StringBuilder();
        List<Uri> elsewhere = new ArrayList<>();
        for (Uri uri : from) {
            String path = filePath(uri);
            // (A file whose own name has a line break in it cannot be told in a list
            // of lines: it is copied, under a name without one.)
            if (path != null && path.indexOf('\n') < 0 && path.indexOf('\r') < 0) {
                paths.append(path).append('\n');
            } else {
                elsewhere.add(uri);
            }
        }
        if (!elsewhere.isEmpty()) {
            paths.append(copyIn(elsewhere, keep));
        }
        return paths.toString();
    }

    /**
     * The path of the file behind an address of the system's, if it is a file of
     * shared storage that the app can read (with "all files access"); null otherwise.
     */
    private String filePath(Uri uri) {
        try {
            String path = null;
            String authority = uri.getAuthority();
            if (DocumentsContract.isDocumentUri(this, uri)) {
                String id = DocumentsContract.getDocumentId(uri);
                int colon = id.indexOf(':');
                String kind = colon < 0 ? "" : id.substring(0, colon);
                String rest = colon < 0 ? id : id.substring(colon + 1);
                if ("com.android.externalstorage.documents".equals(authority) && colon >= 0) {
                    File root = "primary".equalsIgnoreCase(kind)
                            ? Environment.getExternalStorageDirectory()
                            : new File("/storage", kind);
                    path = new File(root, rest).getAbsolutePath();
                } else if ("com.android.providers.downloads.documents".equals(authority)
                        && "raw".equals(kind)) {
                    path = rest;
                } else if ("com.android.providers.media.documents".equals(authority)
                        || ("com.android.providers.downloads.documents".equals(authority)
                                && "msf".equals(kind))) {
                    // The number the media store knows the file by.
                    path = dataOf(ContentUris.withAppendedId(
                            MediaStore.Files.getContentUri(MediaStore.VOLUME_EXTERNAL),
                            Long.parseLong(rest)));
                }
            } else if (MediaStore.AUTHORITY.equals(authority)) {
                path = dataOf(uri);
            }
            if (path != null && new File(path).canRead()) {
                return path;
            }
        } catch (RuntimeException error) {
            Log.w(TAG, "no path for a chosen file", error);
        }
        return null;
    }

    /** Where the media store says a file is. */
    private String dataOf(Uri uri) {
        try (Cursor cursor = getContentResolver().query(
                uri, new String[] {MediaStore.MediaColumns.DATA}, null, null, null)) {
            if (cursor != null && cursor.moveToFirst()) {
                return cursor.getString(0);
            }
        }
        return null;
    }

    /**
     * Shows a folder of shared storage in the system's file manager. False if the
     * folder is not one it can show, or there is no file manager that takes it.
     */
    public boolean showFolder(String path) {
        String primary = Environment.getExternalStorageDirectory().getAbsolutePath();
        String document;
        if (path.equals(primary) || path.startsWith(primary + "/")) {
            document = "primary:" + path.substring(Math.min(path.length(), primary.length() + 1));
        } else if (path.startsWith("/storage/")) {
            String below = path.substring("/storage/".length());
            int slash = below.indexOf('/');
            document = slash < 0
                    ? below + ":"
                    : below.substring(0, slash) + ":" + below.substring(slash + 1);
        } else {
            return false;
        }
        Uri folder = DocumentsContract.buildDocumentUri(
                "com.android.externalstorage.documents", document);
        try {
            startActivity(new Intent(Intent.ACTION_VIEW)
                    .setDataAndType(folder, DocumentsContract.Document.MIME_TYPE_DIR)
                    .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION));
            return true;
        } catch (RuntimeException error) {
            Log.w(TAG, "no file manager shows " + document, error);
            return false;
        }
    }

    /**
     * Copies files into the app's own storage and returns the paths of the copies, one
     * a line. Every file gets a name of its own (two files called the same do not
     * become one), and a copy that is there already, name and content, is used again.
     */
    private String copyIn(List<Uri> from, boolean keep) {
        StringBuilder paths = new StringBuilder();
        File once = new File(getCacheDir(), "picked/" + System.nanoTime());
        ContentResolver resolver = getContentResolver();
        for (Uri uri : from) {
            String name = nameOf(uri);
            // A playlist is read and done with, also among music.
            String lower = name.toLowerCase(Locale.ROOT);
            boolean music = !lower.endsWith(".m3u8") && !lower.endsWith(".m3u");
            File into = keep && music ? importedFolder() : once;
            if (!into.isDirectory() && !into.mkdirs()) {
                Log.w(TAG, "cannot make " + into);
            }
            File part = null;
            try {
                part = File.createTempFile("copy", ".part", into);
                try (InputStream in = resolver.openInputStream(uri);
                        OutputStream out = new FileOutputStream(part)) {
                    if (in == null) {
                        continue;
                    }
                    copy(in, out);
                }
                File kept = settle(part, into, name);
                part = null;
                paths.append(kept.getAbsolutePath()).append('\n');
            } catch (IOException | RuntimeException error) {
                Log.w(TAG, "cannot copy a chosen file", error);
            } finally {
                if (part != null && part.exists() && !part.delete()) {
                    Log.w(TAG, "cannot remove " + part);
                }
            }
        }
        return paths.toString();
    }

    /**
     * Gives the copy `part` its place in `folder`: the name the file had, or that name
     * with a number where another file has it. A file of that name with the same
     * content is used instead, and the copy dropped. No file is ever written over.
     */
    private static synchronized File settle(File part, File folder, String name)
            throws IOException {
        int dot = name.lastIndexOf('.');
        String stem = dot > 0 ? name.substring(0, dot) : name;
        String ending = dot > 0 ? name.substring(dot) : "";
        for (int number = 1; number < 100000; number++) {
            File at = new File(folder, number == 1 ? name : stem + " (" + number + ")" + ending);
            // Made only if there is none of that name: the name is this copy's then.
            if (at.createNewFile()) {
                if (part.renameTo(at)) {
                    return at;
                }
                if (!at.delete()) {
                    Log.w(TAG, "cannot remove " + at);
                }
                throw new IOException("cannot move the copy to " + at);
            }
            if (sameContent(at, part)) {
                if (!part.delete()) {
                    Log.w(TAG, "cannot remove " + part);
                }
                return at;
            }
        }
        throw new IOException("no free name for " + name);
    }

    private static boolean sameContent(File one, File other) throws IOException {
        if (one.length() != other.length()) {
            return false;
        }
        try (InputStream a = new FileInputStream(one); InputStream b = new FileInputStream(other)) {
            byte[] left = new byte[1 << 16];
            byte[] right = new byte[1 << 16];
            while (true) {
                int read = fill(a, left);
                if (read != fill(b, right)) {
                    return false;
                }
                if (read == 0) {
                    return true;
                }
                for (int i = 0; i < read; i++) {
                    if (left[i] != right[i]) {
                        return false;
                    }
                }
            }
        }
    }

    /** Reads until `buffer` is full or the stream is at its end; how much was read. */
    private static int fill(InputStream in, byte[] buffer) throws IOException {
        int have = 0;
        while (have < buffer.length) {
            int read = in.read(buffer, have, buffer.length - have);
            if (read < 0) {
                break;
            }
            have += read;
        }
        return have;
    }

    private void copyOut(File from, Uri to) {
        try (InputStream in = new FileInputStream(from);
                OutputStream out = getContentResolver().openOutputStream(to)) {
            if (out != null) {
                copy(in, out);
            }
        } catch (IOException | RuntimeException error) {
            Log.w(TAG, "cannot write the file where it was to be kept", error);
        }
    }

    private static void copy(InputStream in, OutputStream out) throws IOException {
        byte[] buffer = new byte[1 << 16];
        int read;
        while ((read = in.read(buffer)) > 0) {
            out.write(buffer, 0, read);
        }
    }

    /** The name a file is shown under, without anything a path would make of it. */
    private String nameOf(Uri uri) {
        String name = null;
        if ("content".equals(uri.getScheme())) {
            try (Cursor cursor = getContentResolver().query(
                    uri, new String[] {OpenableColumns.DISPLAY_NAME}, null, null, null)) {
                if (cursor != null && cursor.moveToFirst()) {
                    name = cursor.getString(0);
                }
            } catch (RuntimeException error) {
                Log.w(TAG, "no name for a chosen file", error);
            }
        }
        if (name == null) {
            name = uri.getLastPathSegment();
        }
        if (name == null || name.isEmpty()) {
            name = "file";
        }
        // What a path would make of it, and what the list of paths would: the paths go
        // to the app one a line, so a line break in a name would make two of one (and
        // the second could name any place).
        name = name.replaceAll("[\\p{Cntrl}/\\\\]", "_").trim();
        // Short enough for a file name with a number added (255 bytes; a letter can be
        // three or four): cut, with its ending kept.
        if (name.length() > 70) {
            int dot = name.lastIndexOf('.');
            String ending = dot > 0 && name.length() - dot <= 10 ? name.substring(dot) : "";
            name = name.substring(0, 70 - ending.length()) + ending;
        }
        return name.isEmpty() || name.equals(".") || name.equals("..") ? "file" : name;
    }

    /** A music file another app asked to have opened ("Open with"). */
    private void open(Intent intent) {
        if (intent == null || !Intent.ACTION_VIEW.equals(intent.getAction())
                || intent.getData() == null) {
            return;
        }
        Uri uri = intent.getData();
        if ("file".equals(uri.getScheme()) && uri.getPath() != null) {
            Native.onOpen(uri.getPath());
            return;
        }
        List<Uri> one = new ArrayList<>();
        one.add(uri);
        new Thread(() -> Native.onOpen(pathsOf(one, true)), "oneum-open").start();
    }
}
