#!/bin/sh
# Makes the key store OnEum's Android builds are signed with for others: the "upload
# key" of Google Play, and the key of the APK that is installed by hand.
#
#   scripts/android-keystore.sh            makes it (never over one that is there)
#   scripts/android-keystore.sh --show     says where it is and prints its certificate
#
#   ONEUM_ANDROID_KEYSTORE                 the key store (default:
#                                          ~/.config/oneum-signing/android-upload.jks)
#   ONEUM_ANDROID_KEYSTORE_PASSWORD_FILE   the file its password is kept in (default: the
#                                          key store's name with .pass for .jks)
#   ONEUM_ANDROID_KEY_ALIAS                the key's name in it (default: upload)
#
# Both files are outside the repository and readable by their owner only. The password
# is made here at random and written to its file; it is never printed. The certificate
# names no person ("CN=OnEum"): Google Play shows the developer's name from the account,
# not from the key.
#
# KEEP A COPY OF BOTH FILES somewhere else (a password manager takes both). Without
# them: Google can set a new upload key for an app on Play (Play Console > Play app
# signing > Request upload key reset), but the APK that people installed by hand can
# then never be updated in place: a next version signed with another key is refused
# until the app is removed, and its library with it.
set -eu
STORE=${ONEUM_ANDROID_KEYSTORE:-$HOME/.config/oneum-signing/android-upload.jks}
SECRET=${ONEUM_ANDROID_KEYSTORE_PASSWORD_FILE:-${STORE%.jks}.pass}
ALIAS=${ONEUM_ANDROID_KEY_ALIAS:-upload}
if [ -z "${JAVA_HOME:-}" ]; then
  JAVA_HOME=$(/usr/libexec/java_home -v 21 2>/dev/null || /usr/libexec/java_home -v 17 2>/dev/null || true)
fi
KEYTOOL=${JAVA_HOME:+$JAVA_HOME/bin/}keytool
command -v "$KEYTOOL" >/dev/null 2>&1 || { echo "no keytool (a JDK is needed)" >&2; exit 1; }

show() {
  [ -f "$STORE" ] && [ -f "$SECRET" ] || { echo "no key store at $STORE (or no password file beside it)" >&2; exit 1; }
  echo "key store: $STORE"
  echo "password:  in $SECRET (not shown)"
  # (The certificate is public: it is in every file signed with the key.)
  # (In English whatever the language of this computer: the lines are picked by name.)
  "$KEYTOOL" -J-Duser.language=en -list -v -keystore "$STORE" -alias "$ALIAS" -storepass:file "$SECRET" \
    | grep -E '^(Alias name|Owner|Valid from|Signature algorithm name|Subject Public Key Algorithm)|SHA256:'
}

case "${1:-}" in
  --show) show; exit 0 ;;
  "") ;;
  *) echo "usage: android-keystore.sh [--show]" >&2; exit 2 ;;
esac

if [ -e "$STORE" ] || [ -e "$SECRET" ]; then
  echo "there is a key store (or a password file) already: $STORE" >&2
  echo "it is not made anew: a new key cannot update what the old one signed" >&2
  exit 1
fi
umask 077
mkdir -p "$(dirname "$STORE")"
chmod 700 "$(dirname "$STORE")"
# 32 random bytes as letters and digits, on one line.
openssl rand -base64 32 | tr -d '\n=+/' > "$SECRET"
[ "$(wc -c < "$SECRET" | tr -d ' ')" -ge 32 ] || { echo "no password was made" >&2; rm -f "$SECRET"; exit 1; }
# Valid for 30 years: Google Play asks for a key that outlives 2033, and the key of an
# APK installed by hand has to last as long as the app is updated.
if ! "$KEYTOOL" -J-Duser.language=en -genkeypair -keystore "$STORE" -storetype PKCS12 -alias "$ALIAS" \
  -keyalg RSA -keysize 4096 -sigalg SHA256withRSA -validity 10950 \
  -dname "CN=OnEum" -storepass:file "$SECRET" >/dev/null 2>&1; then
  rm -f "$STORE" "$SECRET"
  echo "keytool could not make the key store" >&2
  exit 1
fi
chmod 600 "$STORE" "$SECRET"
echo "made a new key store"
show
echo
echo "Keep a copy of both files somewhere else. To build with this key:"
echo "  ONEUM_ANDROID_KEYSTORE=\"$STORE\" scripts/android-release.sh"
