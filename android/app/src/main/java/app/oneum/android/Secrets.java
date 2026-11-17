package app.oneum.android;

import android.content.Context;
import android.content.SharedPreferences;
import android.security.keystore.KeyGenParameterSpec;
import android.security.keystore.KeyProperties;
import android.util.Base64;
import android.util.Log;

import java.nio.charset.StandardCharsets;
import java.security.KeyStore;

import javax.crypto.Cipher;
import javax.crypto.KeyGenerator;
import javax.crypto.SecretKey;
import javax.crypto.spec.GCMParameterSpec;

/**
 * The passwords the app keeps (those of network folders).
 *
 * Each is encrypted (AES-256 in GCM mode) with a key that is made in the Android
 * Keystore and never leaves it: the app can ask the system to encrypt and decrypt with
 * it, and cannot read it. What is written to the app's own storage is the encrypted
 * password, which is of no use without the key (the app's storage is not backed up,
 * and the key would not be).
 */
final class Secrets {
    private static final String TAG = "OnEum";
    private static final String KEY = "oneum.network.folders";
    private static final String FILE = "secrets";
    private static final String STORE = "AndroidKeyStore";

    private Secrets() {}

    private static SecretKey key() throws Exception {
        KeyStore store = KeyStore.getInstance(STORE);
        store.load(null);
        if (!store.containsAlias(KEY)) {
            KeyGenerator maker = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, STORE);
            maker.init(new KeyGenParameterSpec.Builder(
                            KEY, KeyProperties.PURPOSE_ENCRYPT | KeyProperties.PURPOSE_DECRYPT)
                    .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                    .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                    .setKeySize(256)
                    .build());
            maker.generateKey();
        }
        return ((KeyStore.SecretKeyEntry) store.getEntry(KEY, null)).getSecretKey();
    }

    private static SharedPreferences file(Context context) {
        return context.getSharedPreferences(FILE, Context.MODE_PRIVATE);
    }

    /** Keeps {@code secret} under {@code id}; false when the key store refuses. */
    static synchronized boolean put(Context context, String id, String secret) {
        try {
            Cipher cipher = Cipher.getInstance("AES/GCM/NoPadding");
            cipher.init(Cipher.ENCRYPT_MODE, key());
            byte[] sealed = cipher.doFinal(secret.getBytes(StandardCharsets.UTF_8));
            String written = Base64.encodeToString(cipher.getIV(), Base64.NO_WRAP)
                    + ":" + Base64.encodeToString(sealed, Base64.NO_WRAP);
            return file(context).edit().putString(id, written).commit();
        } catch (Exception error) {
            // (The message of the error, never the password.)
            Log.w(TAG, "a password could not be kept: " + error);
            return false;
        }
    }

    /** The password kept under {@code id}, or null. */
    static synchronized String get(Context context, String id) {
        String written = file(context).getString(id, null);
        if (written == null) {
            return null;
        }
        try {
            int colon = written.indexOf(':');
            byte[] iv = Base64.decode(written.substring(0, colon), Base64.NO_WRAP);
            byte[] sealed = Base64.decode(written.substring(colon + 1), Base64.NO_WRAP);
            Cipher cipher = Cipher.getInstance("AES/GCM/NoPadding");
            cipher.init(Cipher.DECRYPT_MODE, key(), new GCMParameterSpec(128, iv));
            return new String(cipher.doFinal(sealed), StandardCharsets.UTF_8);
        } catch (Exception error) {
            // The key is gone (the app's data came from another phone) or the entry
            // is damaged: there is no password, and it is asked for again.
            Log.w(TAG, "a kept password could not be read: " + error);
            return null;
        }
    }

    static synchronized void remove(Context context, String id) {
        file(context).edit().remove(id).commit();
    }
}
