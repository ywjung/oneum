package app.oneum.android;

import android.app.Activity;
import android.content.Intent;
import android.os.Bundle;
import android.os.SystemClock;

import java.io.File;

/**
 * Starts OnEum anew after its process ended itself because it could not go on: its
 * graphics device was lost, and a process gets no second one (see `graphics_lost` in
 * src/ui/mod.rs). This activity runs in a process of its own (see the manifest), so
 * that it outlives the app's. It waits for that process to be gone (it saves what it
 * keeps first), then opens the app as the launcher does.
 */
public final class RelaunchActivity extends Activity {
    static final String PID = "app.oneum.android.pid";
    /** How long the app is given to save and end before it is ended from here. */
    private static final long PATIENCE_MS = 10_000;

    @Override
    protected void onCreate(Bundle state) {
        super.onCreate(state);
        final int pid = getIntent().getIntExtra(PID, -1);
        new Thread(() -> {
            long until = SystemClock.uptimeMillis() + PATIENCE_MS;
            while (alive(pid) && SystemClock.uptimeMillis() < until) {
                SystemClock.sleep(50);
            }
            if (alive(pid)) {
                android.os.Process.killProcess(pid);
                SystemClock.sleep(200);
            }
            runOnUiThread(() -> {
                startActivity(new Intent(this, MainActivity.class).addFlags(
                        Intent.FLAG_ACTIVITY_NEW_TASK | Intent.FLAG_ACTIVITY_CLEAR_TASK));
                finish();
                // Nothing else lives in this process.
                System.exit(0);
            });
        }, "relaunch").start();
    }

    private static boolean alive(int pid) {
        return pid > 0 && new File("/proc/" + pid).exists();
    }
}
