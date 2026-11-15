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
 * something to play; it is in the foreground only while music plays.
 */
public final class PlaybackService extends Service {
    private static final String TAG = "OnEum";
    private static final String CHANNEL = "playback";
    private static final int NOTIFICATION = 1;
    private static final String ACTION_PREVIOUS = "app.oneum.android.PREVIOUS";
    private static final String ACTION_TOGGLE = "app.oneum.android.TOGGLE";
    private static final String ACTION_NEXT = "app.oneum.android.NEXT";

    /** What plays, as the app last told it. */
    static final class State {
        final String title;
        final String artist;
        final String album;
        final long durationMs;
        final long positionMs;
        final boolean playing;
        final String artPath;

        State(String title, String artist, String album, long durationMs, long positionMs,
                boolean playing, String artPath) {
            this.title = title;
            this.artist = artist;
            this.album = album;
            this.durationMs = durationMs;
            this.positionMs = positionMs;
            this.playing = playing;
            this.artPath = artPath;
        }
    }

    /** The service while it runs (main thread only, like everything here). */
    private static PlaybackService running;
    private static State latest;

    private final Handler main = new Handler(Looper.getMainLooper());
    private MediaSession session;
    private AudioManager audio;
    private AudioFocusRequest focusRequest;
    private boolean hasFocus;
    private boolean lostForAWhile;
    private PowerManager.WakeLock awake;
    private boolean foreground;
    private String artShown;
    private Bitmap art;

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
        }, main);
        session.setSessionActivity(openApp());

        audio = getSystemService(AudioManager.class);
        focusRequest = new AudioFocusRequest.Builder(AudioManager.AUDIOFOCUS_GAIN)
                .setAudioAttributes(new AudioAttributes.Builder()
                        .setUsage(AudioAttributes.USAGE_MEDIA)
                        .setContentType(AudioAttributes.CONTENT_TYPE_MUSIC)
                        .build())
                .setOnAudioFocusChangeListener(this::focusChanged, main)
                .build();
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
            goForeground(notification(new State("OnEum", "", "", 0, 0, false, "")));
            stopSelf();
        } else {
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
        try {
            unregisterReceiver(noisy);
        } catch (RuntimeException error) {
            Log.w(TAG, "the headphone receiver was not registered", error);
        }
        if (hasFocus) {
            audio.abandonAudioFocusRequest(focusRequest);
            hasFocus = false;
        }
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
        if (state.playing && !hasFocus) {
            int answer = audio.requestAudioFocus(focusRequest);
            hasFocus = answer == AudioManager.AUDIOFOCUS_REQUEST_GRANTED;
            lostForAWhile = false;
            if (!hasFocus) {
                // A call is on, say: the app is told as if the sound had been taken.
                Native.onSound(1);
            }
        }
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
                        state.positionMs, state.playing ? 1f : 0f)
                .build());
        session.setActive(true);

        Notification notification = notification(state);
        if (state.playing) {
            goForeground(notification);
            if (!awake.isHeld()) {
                awake.acquire();
            }
        } else {
            // Paused: the notification stays, but nothing holds the phone awake and the
            // system may end the app when it needs the room.
            if (foreground) {
                stopForeground(STOP_FOREGROUND_DETACH);
                foreground = false;
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

    private void focusChanged(int change) {
        switch (change) {
            case AudioManager.AUDIOFOCUS_LOSS:
                // Another app plays now: this one stops and does not start by itself.
                hasFocus = false;
                lostForAWhile = false;
                Native.onSound(1);
                break;
            case AudioManager.AUDIOFOCUS_LOSS_TRANSIENT:
                // A call, an alarm: silent until it is over.
                lostForAWhile = true;
                Native.onSound(1);
                break;
            case AudioManager.AUDIOFOCUS_GAIN:
                if (lostForAWhile) {
                    lostForAWhile = false;
                    Native.onSound(2);
                }
                break;
            default:
                // "May duck": the system turns the music down by itself.
                break;
        }
    }
}
