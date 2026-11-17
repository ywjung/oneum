package app.oneum.android;

import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.app.Service;
import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;
import android.content.IntentFilter;
import android.content.pm.ServiceInfo;
import android.graphics.Bitmap;
import android.graphics.BitmapFactory;
import android.graphics.drawable.Icon;
import android.media.AudioAttributes;
import android.media.AudioFocusRequest;
import android.media.AudioManager;
import android.media.MediaMetadata;
import android.media.session.MediaSession;
import android.media.session.PlaybackState;
import android.os.Build;
import android.os.Handler;
import android.os.IBinder;
import android.os.Looper;
import android.os.PowerManager;
import android.util.Log;
import android.view.KeyEvent;

/**
 * What keeps OnEum playing when it is not on screen, and what the system shows of it:
 * a foreground service with a media session and its notification (the buttons of the
 * notification, of the lock screen and of headsets come back to the app as commands).
 * It also asks for the sound when playing starts ("audio focus"), tells the app when a
 * call or another app takes it, and when headphones are pulled out.
 *
 * The service is started when music first plays and stays for as long as the app has
 * something to play; it is in the foreground while music plays and for a while after a
 * pause.
 */
public final class PlaybackService extends Service {
    private static final String TAG = "OnEum";
    private static final String CHANNEL = "playback";
    private static final int NOTIFICATION = 1;
    static final String ACTION_PREVIOUS = "app.oneum.android.PREVIOUS";
    static final String ACTION_TOGGLE = "app.oneum.android.TOGGLE";
    static final String ACTION_NEXT = "app.oneum.android.NEXT";
    /**
     * How long a paused service stays in the foreground. A service that leaves it at
     * once is stopped by Android about a minute later when the app is off screen ("app
     * idle"), and the notification and the lock screen lose their buttons: after a
     * short break the listener could only play on by opening the app. Ten minutes is
     * what media apps commonly keep. (Counted while the phone is awake: nothing holds
     * it awake for this.)
     */
    private static final long PAUSED_FOREGROUND_MS = 10 * 60 * 1000L;
    /** A verification run of a debuggable build may shorten it (see MainActivity). */
    static long pausedForegroundMs = PAUSED_FOREGROUND_MS;

    /** What plays, as the app last told it. */
    static final class State {
        final String title;
        final String artist;
        final String album;
        final long durationMs;
        final long positionMs;
        final boolean playing;
        /** How fast the song goes, as a multiple of its own speed. */
        final float speed;
        final String artPath;

        State(String title, String artist, String album, long durationMs, long positionMs,
                boolean playing, float speed, String artPath) {
            this.title = title;
            this.artist = artist;
            this.album = album;
            this.durationMs = durationMs;
            this.positionMs = positionMs;
            this.playing = playing;
            this.speed = speed;
            this.artPath = artPath;
        }
    }

    /** The service while it runs (main thread only, like everything here). */
    private static PlaybackService running;
    private static State latest;

    private static final Handler MAIN = new Handler(Looper.getMainLooper());
    /**
     * Who has the sound ("audio focus"). Kept apart from the running service: the app
     * asks for the sound before it makes any (see claim), which may be before the
     * service exists. Changed on the main thread only; hasFocus is also read elsewhere.
     */
    private static AudioManager audio;
    private static AudioFocusRequest focusRequest;
    private static volatile boolean hasFocus;
    /** The sound is with something else for a while (a call) and comes back by itself. */
    private static boolean lostForAWhile;

    private MediaSession session;
    private PowerManager.WakeLock awake;
    private boolean foreground;
    /** Whether leaveForeground is waiting for its time. */
    private boolean leaving;
    private String artShown;
    private Bitmap art;

    /**
     * A while after a pause (pausedForegroundMs): the service leaves the foreground.
     * The notification stays, and the system may end the app when it needs the room.
     * Not while the sound is only away for a while (a call): playing goes on by itself
     * afterwards, and a service that has left the foreground cannot come back to it
     * from behind the screen (it would play on unprotected). The time then starts
     * again when that is over and nothing plays (see settled).
     */
    private final Runnable leaveForeground = () -> {
        leaving = false;
        State state = latest;
        if (foreground && !lostForAWhile && (state == null || !state.playing)) {
            stopForeground(STOP_FOREGROUND_DETACH);
            foreground = false;
        }
    };

    private final BroadcastReceiver noisy = new BroadcastReceiver() {
        @Override
        public void onReceive(Context context, Intent intent) {
            // Headphones were pulled out: the music must not go on from the speaker.
            Native.onSound(4);
        }
    };

    /** The app says what plays. The service is started when something first plays. */
    static void update(Context context, State state) {
        latest = state;
        NowPlayingWidget.show(context, state);
        if (running != null) {
            running.show(state);
        } else if (state.playing) {
            try {
                context.startForegroundService(new Intent(context, PlaybackService.class));
            } catch (RuntimeException error) {
                // Not allowed from the background on newer Androids: the music still
                // plays for as long as the system leaves the app alone.
                Log.w(TAG, "cannot start the playback service", error);
            }
        }
    }

    /**
     * A key of the window in front. The system offers media keys (the button of wired
     * earphones, a keyboard's) to that window before the media session, and the app's
     * native side would take them and do nothing with them. They go to the session,
     * as they do with the app in the background; before anything has played there is
     * no session, and the app is told directly. False for any other key.
     */
    static boolean mediaKey(KeyEvent event) {
        int command;
        switch (event.getKeyCode()) {
            case KeyEvent.KEYCODE_MEDIA_PLAY:
                command = 0;
                break;
            case KeyEvent.KEYCODE_MEDIA_PAUSE:
                command = 1;
                break;
            case KeyEvent.KEYCODE_MEDIA_PLAY_PAUSE:
            case KeyEvent.KEYCODE_HEADSETHOOK:
                command = 2;
                break;
            case KeyEvent.KEYCODE_MEDIA_STOP:
                command = 3;
                break;
            case KeyEvent.KEYCODE_MEDIA_NEXT:
                command = 4;
                break;
            case KeyEvent.KEYCODE_MEDIA_PREVIOUS:
                command = 5;
                break;
            default:
                return false;
        }
        PlaybackService service = running;
        if (service != null
                && service.session.getController().dispatchMediaButtonEvent(event)) {
            return true;
        }
        if (event.getAction() == KeyEvent.ACTION_DOWN && event.getRepeatCount() == 0) {
            Native.onMedia(command, 0);
        }
        return true;
    }

    private static void prepareFocus(Context context) {
        if (focusRequest != null) {
            return;
        }
        audio = context.getApplicationContext().getSystemService(AudioManager.class);
        focusRequest = new AudioFocusRequest.Builder(AudioManager.AUDIOFOCUS_GAIN)
                .setAudioAttributes(new AudioAttributes.Builder()
                        .setUsage(AudioAttributes.USAGE_MEDIA)
                        .setContentType(AudioAttributes.CONTENT_TYPE_MUSIC)
                        .build())
                // During a call the sound is not refused for good but given afterwards.
                .setAcceptsDelayedFocusGain(true)
                .setOnAudioFocusChangeListener(PlaybackService::focusChanged, MAIN)
                .build();
    }

    /** Whether the sound is this app's right now (any thread). */
    static boolean hasSound() {
        return hasFocus;
    }

    /**
     * The app is about to make sound: true if the sound is this app's, or is now. False
     * while something else keeps it (a call): the app stays silent, and is told when
     * the sound comes (if it comes by itself). Main thread.
     */
    static boolean claim(Context context) {
        if (hasFocus) {
            return true;
        }
        prepareFocus(context);
        PlaybackService service = running;
        State state = latest;
        if (service != null && state != null) {
            // An app that is not on the screen may ask for the sound only from a
            // foreground service (Android 15): the service goes there first.
            service.goForeground(service.notification(state));
        }
        boolean got = ask();
        if (!got && service != null && state != null && !state.playing) {
            // Nothing plays after all: back to how a paused service is kept.
            service.show(state);
        }
        return got;
    }

    /** Asks the system for the sound. */
    private static boolean ask() {
        int answer = audio.requestAudioFocus(focusRequest);
        if (answer == AudioManager.AUDIOFOCUS_REQUEST_GRANTED) {
            hasFocus = true;
            lostForAWhile = false;
            return true;
        }
        if (answer == AudioManager.AUDIOFOCUS_REQUEST_DELAYED) {
            // A call is on: the sound is given when it is over.
            lostForAWhile = true;
        }
        // Refused: an interruption that was on before is still on, and still ends by
        // giving the sound back (lostForAWhile stays as it was).
        return false;
    }

    /**
     * The app has answered the end of an interruption: it plays on, or it does not
     * (the listener stopped it meanwhile). A service that kept the foreground through
     * the interruption and does not play leaves it like any paused one.
     */
    static void settled(boolean resumed) {
        PlaybackService service = running;
        State state = latest;
        if (!resumed && service != null && state != null && !state.playing) {
            service.show(state);
        }
    }

    /** Nothing is left to play (or the app is going). */
    static void end() {
        latest = null;
        if (running != null) {
            running.stopSelf();
        }
    }

    @Override
    public void onCreate() {
        super.onCreate();
        running = this;
        NotificationManager notifications = getSystemService(NotificationManager.class);
        NotificationChannel channel = new NotificationChannel(
                CHANNEL, getString(R.string.channel_playback), NotificationManager.IMPORTANCE_LOW);
        channel.setShowBadge(false);
        notifications.createNotificationChannel(channel);

        session = new MediaSession(this, "OnEum");
        session.setCallback(new MediaSession.Callback() {
            @Override
            public void onPlay() {
                Native.onMedia(0, 0);
            }

            @Override
            public void onPause() {
                Native.onMedia(1, 0);
            }

            @Override
            public void onStop() {
                Native.onMedia(3, 0);
            }

            @Override
            public void onSkipToNext() {
                Native.onMedia(4, 0);
            }

            @Override
            public void onSkipToPrevious() {
                Native.onMedia(5, 0);
            }

            @Override
            public void onSeekTo(long position) {
                Native.onMedia(6, position);
            }
        }, MAIN);
        session.setSessionActivity(openApp());

        prepareFocus(this);
        PowerManager power = getSystemService(PowerManager.class);
        awake = power.newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "OnEum:playback");
        awake.setReferenceCounted(false);
        registerReceiver(noisy, new IntentFilter(AudioManager.ACTION_AUDIO_BECOMING_NOISY));
    }

    @Override
    public int onStartCommand(Intent intent, int flags, int startId) {
        String action = intent == null ? null : intent.getAction();
        if (ACTION_PREVIOUS.equals(action)) {
            Native.onMedia(5, 0);
        } else if (ACTION_TOGGLE.equals(action)) {
            Native.onMedia(2, 0);
        } else if (ACTION_NEXT.equals(action)) {
            Native.onMedia(4, 0);
        }
        State state = latest;
        if (state == null) {
            // Started with nothing to show (the system brought the service back).
            goForeground(notification(new State("OnEum", "", "", 0, 0, false, 1f, "")));
            stopSelf();
        } else {
            // A service started with startForegroundService has to go to the foreground
            // whatever has happened since it was asked for: the music may have been
            // paused, or may have failed, before the service got here (show does not
            // go there for a paused state). Without this the system ends the whole app
            // a few seconds later, as one that has a bug.
            if (!foreground) {
                goForeground(notification(state));
            }
            show(state);
        }
        return START_NOT_STICKY;
    }

    @Override
    public IBinder onBind(Intent intent) {
        return null;
    }

    @Override
    public void onTaskRemoved(Intent rootIntent) {
        // Swiped away: the activity goes, and with it the app (see MainActivity).
        stopSelf();
        super.onTaskRemoved(rootIntent);
    }

    @Override
    public void onDestroy() {
        running = null;
        MAIN.removeCallbacks(leaveForeground);
        try {
            unregisterReceiver(noisy);
        } catch (RuntimeException error) {
            Log.w(TAG, "the headphone receiver was not registered", error);
        }
        // Given back whatever it stands at: also a request the system still holds while
        // something else has the sound for a while (it would answer a service that is
        // gone when that ends).
        if (focusRequest != null) {
            audio.abandonAudioFocusRequest(focusRequest);
        }
        hasFocus = false;
        lostForAWhile = false;
        if (awake.isHeld()) {
            awake.release();
        }
        session.setActive(false);
        session.release();
        stopForeground(STOP_FOREGROUND_REMOVE);
        super.onDestroy();
    }

    /** Shows `state` in the session and the notification, and follows playing/paused. */
    private void show(State state) {
        if (!state.artPath.equals(artShown)) {
            artShown = state.artPath;
            art = state.artPath.isEmpty() ? null : BitmapFactory.decodeFile(state.artPath);
        }
        MediaMetadata.Builder metadata = new MediaMetadata.Builder()
                .putString(MediaMetadata.METADATA_KEY_TITLE, state.title)
                .putString(MediaMetadata.METADATA_KEY_ARTIST, state.artist)
                .putString(MediaMetadata.METADATA_KEY_ALBUM, state.album)
                .putLong(MediaMetadata.METADATA_KEY_DURATION, state.durationMs);
        if (art != null) {
            metadata.putBitmap(MediaMetadata.METADATA_KEY_ALBUM_ART, art);
        }
        session.setMetadata(metadata.build());
        session.setPlaybackState(new PlaybackState.Builder()
                .setActions(PlaybackState.ACTION_PLAY | PlaybackState.ACTION_PAUSE
                        | PlaybackState.ACTION_PLAY_PAUSE | PlaybackState.ACTION_STOP
                        | PlaybackState.ACTION_SKIP_TO_NEXT
                        | PlaybackState.ACTION_SKIP_TO_PREVIOUS | PlaybackState.ACTION_SEEK_TO)
                .setState(state.playing ? PlaybackState.STATE_PLAYING : PlaybackState.STATE_PAUSED,
                        state.positionMs, state.playing ? state.speed : 0f)
                .build());
        session.setActive(true);

        Notification notification = notification(state);
        if (state.playing) {
            // In this order: an app that is not on the screen may ask for the sound only
            // from a foreground service (Android 15). Asked before, playing on from the
            // notification after another app had played was refused.
            goForeground(notification);
            MAIN.removeCallbacks(leaveForeground);
            leaving = false;
            // The app asks before it makes sound (claim); this is for a start that did
            // not get to ask. Without the sound the app is told as if it had been taken.
            if (!hasFocus && !ask()) {
                Native.onSound(1);
            }
            if (!awake.isHeld()) {
                awake.acquire();
            }
        } else {
            // Paused: the notification stays and nothing holds the phone awake. The
            // service keeps the foreground for a while, counted from the pause, so
            // that the notification's buttons stay (see PAUSED_FOREGROUND_MS).
            if (foreground && !leaving) {
                leaving = true;
                MAIN.postDelayed(leaveForeground, pausedForegroundMs);
            }
            getSystemService(NotificationManager.class).notify(NOTIFICATION, notification);
            if (awake.isHeld()) {
                awake.release();
            }
        }
    }

    private void goForeground(Notification notification) {
        try {
            if (Build.VERSION.SDK_INT >= 29) {
                startForeground(NOTIFICATION, notification,
                        ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PLAYBACK);
            } else {
                startForeground(NOTIFICATION, notification);
            }
            foreground = true;
        } catch (RuntimeException error) {
            // Not allowed from the background on newer Androids unless the user or a
            // media button asked: the notification is shown without it.
            Log.w(TAG, "cannot go to the foreground", error);
            getSystemService(NotificationManager.class).notify(NOTIFICATION, notification);
        }
    }

    private Notification notification(State state) {
        Notification.Builder builder = new Notification.Builder(this, CHANNEL)
                .setSmallIcon(R.drawable.ic_stat_note)
                .setContentTitle(state.title)
                .setContentText(state.artist)
                .setSubText(state.album)
                .setContentIntent(openApp())
                .setVisibility(Notification.VISIBILITY_PUBLIC)
                .setOngoing(state.playing)
                .setShowWhen(false)
                .setOnlyAlertOnce(true)
                .addAction(action(R.drawable.ic_prev, R.string.action_previous, ACTION_PREVIOUS))
                .addAction(state.playing
                        ? action(R.drawable.ic_pause, R.string.action_pause, ACTION_TOGGLE)
                        : action(R.drawable.ic_play, R.string.action_play, ACTION_TOGGLE))
                .addAction(action(R.drawable.ic_next, R.string.action_next, ACTION_NEXT))
                .setStyle(new Notification.MediaStyle()
                        .setMediaSession(session.getSessionToken())
                        .setShowActionsInCompactView(0, 1, 2));
        if (art != null) {
            builder.setLargeIcon(art);
        }
        return builder.build();
    }

    private Notification.Action action(int icon, int label, String action) {
        PendingIntent intent = PendingIntent.getService(this, 0,
                new Intent(this, PlaybackService.class).setAction(action),
                PendingIntent.FLAG_IMMUTABLE | PendingIntent.FLAG_UPDATE_CURRENT);
        return new Notification.Action.Builder(
                Icon.createWithResource(this, icon), getString(label), intent).build();
    }

    private PendingIntent openApp() {
        return PendingIntent.getActivity(this, 0,
                new Intent(this, MainActivity.class)
                        .setAction(Intent.ACTION_MAIN)
                        .addCategory(Intent.CATEGORY_LAUNCHER),
                PendingIntent.FLAG_IMMUTABLE | PendingIntent.FLAG_UPDATE_CURRENT);
    }

    private static void focusChanged(int change) {
        switch (change) {
            case AudioManager.AUDIOFOCUS_LOSS:
                // Another app plays now: this one stops and does not start by itself.
                hasFocus = false;
                lostForAWhile = false;
                Native.onSound(1);
                // The time a paused service keeps the foreground starts now (it does not
                // run out while the sound is only away for a while).
                settled(false);
                break;
            case AudioManager.AUDIOFOCUS_LOSS_TRANSIENT:
                // A call, an alarm: silent until it is over. The sound is not this
                // app's meanwhile: a "play" of the listener's asks for it again.
                hasFocus = false;
                lostForAWhile = true;
                Native.onSound(1);
                break;
            case AudioManager.AUDIOFOCUS_GAIN:
                hasFocus = true;
                if (lostForAWhile) {
                    lostForAWhile = false;
                    // The app answers with settled(): no guessing here how long it
                    // takes to play on.
                    Native.onSound(2);
                }
                break;
            default:
                // "May duck": the system turns the music down by itself.
                break;
        }
    }
}
