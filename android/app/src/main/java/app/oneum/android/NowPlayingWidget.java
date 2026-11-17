package app.oneum.android;

import android.app.PendingIntent;
import android.appwidget.AppWidgetManager;
import android.appwidget.AppWidgetProvider;
import android.content.ComponentName;
import android.content.Context;
import android.content.Intent;
import android.content.SharedPreferences;
import android.graphics.Bitmap;
import android.graphics.BitmapFactory;
import android.util.Log;
import android.widget.RemoteViews;

import java.util.Objects;

/**
 * The home screen widget: the song that plays (cover, title, artist) with previous,
 * play or pause, and next.
 *
 * The widget is drawn by the launcher from what is handed to it here, so it stays on
 * the screen when the app is gone. The app, though, is one process that ends with its
 * screen (see MainActivity.onDestroy): what the widget shows then is the song that was
 * on last, not playing, and its buttons open the app (WidgetActionActivity), since
 * there is nothing left that could play. The song is kept in the preferences for a
 * launcher that asks again (after a restart of the phone, or when a widget is added).
 */
public final class NowPlayingWidget extends AppWidgetProvider {
    private static final String TAG = "OnEum";
    private static final String KEPT = "widget";
    /** The longest edge of the cover as the launcher gets it. */
    private static final int ART_EDGE = 256;

    /** What the widgets show now (main thread only): nothing is sent twice. */
    private static String shownTitle;
    private static String shownArtist;
    private static String shownArt;
    private static Boolean shownPlaying;

    /** The app says what plays (as it tells the playback service, with every change). */
    static void show(Context context, PlaybackService.State state) {
        boolean same = Objects.equals(state.title, shownTitle)
                && Objects.equals(state.artist, shownArtist)
                && Objects.equals(state.artPath, shownArt)
                && shownPlaying != null && shownPlaying == state.playing;
        if (same) {
            return;
        }
        shownTitle = state.title;
        shownArtist = state.artist;
        shownArt = state.artPath;
        shownPlaying = state.playing;
        context.getSharedPreferences(KEPT, Context.MODE_PRIVATE).edit()
                .putString("title", state.title)
                .putString("artist", state.artist)
                .putString("art", state.artPath)
                .apply();
        push(context, state.title, state.artist, state.artPath, state.playing);
    }

    /** The app is ending: the song stays on the widgets, not playing. */
    static void appGone(Context context) {
        if (shownPlaying == null || shownPlaying) {
            SharedPreferences kept = context.getSharedPreferences(KEPT, Context.MODE_PRIVATE);
            push(context, kept.getString("title", ""), kept.getString("artist", ""),
                    kept.getString("art", ""), false);
        }
        shownPlaying = null;
    }

    /** The launcher asks what to show: a widget was added, or the phone started. */
    @Override
    public void onUpdate(Context context, AppWidgetManager manager, int[] ids) {
        SharedPreferences kept = context.getSharedPreferences(KEPT, Context.MODE_PRIVATE);
        boolean playing = shownPlaying != null && shownPlaying;
        RemoteViews views = views(context, kept.getString("title", ""),
                kept.getString("artist", ""), kept.getString("art", ""), playing);
        for (int id : ids) {
            manager.updateAppWidget(id, views);
        }
    }

    private static void push(Context context, String title, String artist, String art,
            boolean playing) {
        try {
            AppWidgetManager manager = AppWidgetManager.getInstance(context);
            ComponentName widget = new ComponentName(context, NowPlayingWidget.class);
            // (Nothing is drawn where no widget was put on a screen.)
            if (manager == null || manager.getAppWidgetIds(widget).length == 0) {
                return;
            }
            manager.updateAppWidget(widget, views(context, title, artist, art, playing));
        } catch (RuntimeException error) {
            // A launcher that refuses what it is handed must not take the music along.
            Log.w(TAG, "the widget cannot be updated", error);
        }
    }

    private static RemoteViews views(Context context, String title, String artist, String art,
            boolean playing) {
        RemoteViews views = new RemoteViews(context.getPackageName(), R.layout.widget_now_playing);
        boolean known = title != null && !title.isEmpty();
        views.setTextViewText(R.id.widget_title,
                known ? title : context.getString(R.string.app_name));
        views.setTextViewText(R.id.widget_artist,
                known ? (artist == null ? "" : artist) : context.getString(R.string.widget_nothing));
        Bitmap cover = cover(art);
        if (cover != null) {
            views.setImageViewBitmap(R.id.widget_art, cover);
        } else {
            views.setImageViewResource(R.id.widget_art, R.drawable.ic_stat_note);
        }
        views.setImageViewResource(R.id.widget_toggle,
                playing ? R.drawable.ic_pause : R.drawable.ic_play);
        views.setContentDescription(R.id.widget_toggle,
                context.getString(playing ? R.string.action_pause : R.string.action_play));
        int flags = PendingIntent.FLAG_IMMUTABLE | PendingIntent.FLAG_UPDATE_CURRENT;
        // The cover and the words open the app; the buttons go through an activity
        // that shows nothing, which tells the app if it is there and opens it if not.
        PendingIntent open = PendingIntent.getActivity(context, 0,
                new Intent(context, MainActivity.class)
                        .setAction(Intent.ACTION_MAIN)
                        .addCategory(Intent.CATEGORY_LAUNCHER)
                        .setFlags(Intent.FLAG_ACTIVITY_NEW_TASK),
                flags);
        views.setOnClickPendingIntent(R.id.widget_art, open);
        views.setOnClickPendingIntent(R.id.widget_words, open);
        views.setOnClickPendingIntent(R.id.widget_previous,
                button(context, PlaybackService.ACTION_PREVIOUS, flags));
        views.setOnClickPendingIntent(R.id.widget_toggle,
                button(context, PlaybackService.ACTION_TOGGLE, flags));
        views.setOnClickPendingIntent(R.id.widget_next,
                button(context, PlaybackService.ACTION_NEXT, flags));
        return views;
    }

    private static PendingIntent button(Context context, String action, int flags) {
        return PendingIntent.getActivity(context, 0,
                new Intent(context, WidgetActionActivity.class).setAction(action), flags);
    }

    /** The cover, small enough to be handed to another process; null without one. */
    private static Bitmap cover(String path) {
        if (path == null || path.isEmpty()) {
            return null;
        }
        try {
            BitmapFactory.Options size = new BitmapFactory.Options();
            size.inJustDecodeBounds = true;
            BitmapFactory.decodeFile(path, size);
            int edge = Math.max(size.outWidth, size.outHeight);
            if (edge <= 0) {
                return null;
            }
            BitmapFactory.Options read = new BitmapFactory.Options();
            read.inSampleSize = 1;
            while (edge / read.inSampleSize > ART_EDGE * 2) {
                read.inSampleSize *= 2;
            }
            return BitmapFactory.decodeFile(path, read);
        } catch (RuntimeException | OutOfMemoryError error) {
            return null;
        }
    }
}
