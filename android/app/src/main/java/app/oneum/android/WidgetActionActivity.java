package app.oneum.android;

import android.app.Activity;
import android.content.Intent;
import android.os.Bundle;

/**
 * What a button of the home screen widget starts: an activity that shows nothing. If
 * the app is there, it is told (previous, play or pause, next) as its notification's
 * buttons tell it; if it is not, there is nothing that could play, and the app is
 * opened. (An activity, not a broadcast: only an activity may open another from behind
 * the screen.)
 */
public final class WidgetActionActivity extends Activity {
    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        String action = getIntent() == null ? null : getIntent().getAction();
        if (MainActivity.up) {
            if (PlaybackService.ACTION_PREVIOUS.equals(action)) {
                Native.onMedia(5, 0);
            } else if (PlaybackService.ACTION_TOGGLE.equals(action)) {
                Native.onMedia(2, 0);
            } else if (PlaybackService.ACTION_NEXT.equals(action)) {
                Native.onMedia(4, 0);
            }
        } else {
            startActivity(new Intent(this, MainActivity.class)
                    .setAction(Intent.ACTION_MAIN)
                    .addCategory(Intent.CATEGORY_LAUNCHER)
                    .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK));
        }
        finish();
    }
}
