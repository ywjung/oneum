#!/bin/sh
# Signs the Windows files (Authenticode, SHA-256, with an RFC 3161 time stamp) and checks
# the signatures. Used by scripts/package-windows.sh and by the setup program's script
# (for the uninstaller). Nothing is signed unless ONEUM_WIN_SIGN is set, and no
# certificate, key, PIN or account is kept here: all of it comes from the environment.
#
#   scripts/windows-sign.sh wanted            succeeds when signing was asked for
#   scripts/windows-sign.sh ready             fails unless all that signing needs is there
#   scripts/windows-sign.sh sign <file>...    signs each file in place and checks it
#   scripts/windows-sign.sh verify <file>...  fails unless each file carries a signature
#                                             that fits the file, whose chain ends in a
#                                             trusted root, and that has a time stamp
#
# Needs osslsigncode (brew install osslsigncode).
#
# ONEUM_WIN_SIGN says with what:
#   pkcs12   a certificate and its key in one file: ONEUM_WIN_SIGN_PKCS12. Public
#            certificate authorities no longer hand keys out as files, so this is for a
#            test certificate or a private one.
#   pkcs11   a key that stays in a USB token or a cloud HSM, reached through the PKCS #11
#            library of its maker:
#              ONEUM_WIN_SIGN_PKCS11_MODULE  that library
#              ONEUM_WIN_SIGN_KEY            the key (a "pkcs11:" URI)
#              ONEUM_WIN_SIGN_CERT           the certificate: a PEM file with its chain,
#                                            or a "pkcs11:" URI
#              ONEUM_WIN_SIGN_PROVIDER       (if OpenSSL has to be told) its PKCS #11
#                                            provider
#   command  a signing program of your own, for a service that has its own tool:
#            ONEUM_WIN_SIGN_COMMAND is run by sh with the file as "$1" and has to sign
#            that file in place, with a time stamp. What it leaves is checked like the
#            rest.
#
# The password or PIN (pkcs12, pkcs11): ONEUM_WIN_SIGN_PASS_FILE (a file whose first line
# is the password) or ONEUM_WIN_SIGN_PASS; with neither, osslsigncode asks for it at the
# terminal, once for every file.
#
# ONEUM_WIN_SIGN_TS          the time stamp server (RFC 3161); the default is below
# ONEUM_WIN_SIGN_CAFILE      the roots the check trusts for the signature (a PEM file);
#                            by default the list osslsigncode uses
# ONEUM_WIN_SIGN_TSA_CAFILE  the same for the time stamp
# ONEUM_WIN_SIGN_SUBJECT     if set, the check also wants this text in the subject of the
#                            signer's certificate (so that the wrong certificate fails)
set -eu

MODE=${ONEUM_WIN_SIGN:-}
TS=${ONEUM_WIN_SIGN_TS:-http://timestamp.digicert.com}
DESCRIPTION="OnEum"
URL="https://github.com/ywjung/oneum"

die() { echo "windows-sign: $*" >&2; exit 1; }

need_tool() {
  command -v osslsigncode >/dev/null || die "osslsigncode is not installed (brew install osslsigncode)"
}

# Fails unless the named variable, which the chosen way of signing needs, is set.
need() {
  eval "value=\${$1:-}"
  [ -n "$value" ] || die "ONEUM_WIN_SIGN=$MODE needs $1"
}

# Runs osslsigncode with the password on its standard input when one was given, so that
# it is in no command line (which other programs of this computer can read).
with_password() {
  if [ -n "${ONEUM_WIN_SIGN_PASS_FILE:-}" ]; then
    head -n 1 "$ONEUM_WIN_SIGN_PASS_FILE" | tr -d '\r\n' | "$@" -readpass -
  elif [ -n "${ONEUM_WIN_SIGN_PASS:-}" ]; then
    printf '%s' "$ONEUM_WIN_SIGN_PASS" | "$@" -readpass -
  else
    "$@" -askpass
  fi
}

# Fails, saying what is missing, unless everything the chosen way of signing needs was
# given (before a long build rather than after it).
check_setup() {
  case "$MODE" in
    pkcs12)
      need_tool
      need ONEUM_WIN_SIGN_PKCS12
      [ -r "$ONEUM_WIN_SIGN_PKCS12" ] || die "cannot read ONEUM_WIN_SIGN_PKCS12"
      ;;
    pkcs11)
      need_tool
      need ONEUM_WIN_SIGN_PKCS11_MODULE
      need ONEUM_WIN_SIGN_KEY
      need ONEUM_WIN_SIGN_CERT
      [ -r "$ONEUM_WIN_SIGN_PKCS11_MODULE" ] || die "cannot read ONEUM_WIN_SIGN_PKCS11_MODULE"
      case "$ONEUM_WIN_SIGN_CERT" in
        pkcs11:*) ;;
        *) [ -r "$ONEUM_WIN_SIGN_CERT" ] || die "cannot read ONEUM_WIN_SIGN_CERT" ;;
      esac
      ;;
    command)
      # (The check afterwards is osslsigncode's, whatever signs.)
      need_tool
      need ONEUM_WIN_SIGN_COMMAND
      ;;
    "") die "signing was not asked for (ONEUM_WIN_SIGN is not set)" ;;
    *) die "ONEUM_WIN_SIGN is \"$MODE\": it can be pkcs12, pkcs11 or command" ;;
  esac
  if [ -n "${ONEUM_WIN_SIGN_PASS_FILE:-}" ] && [ ! -r "$ONEUM_WIN_SIGN_PASS_FILE" ]; then
    die "cannot read ONEUM_WIN_SIGN_PASS_FILE"
  fi
}

sign_one() {
  file=$1
  [ -f "$file" ] || die "no such file: $file"
  check_setup
  case "$MODE" in
    pkcs12)
      rm -f "$file.signing"
      with_password osslsigncode sign -pkcs12 "$ONEUM_WIN_SIGN_PKCS12" \
        -h sha256 -n "$DESCRIPTION" -i "$URL" -ts "$TS" -in "$file" -out "$file.signing" >/dev/null \
        || { rm -f "$file.signing"; die "signing failed: $file"; }
      mv -f "$file.signing" "$file"
      ;;
    pkcs11)
      case "$ONEUM_WIN_SIGN_CERT" in
        pkcs11:*) set -- -pkcs11cert "$ONEUM_WIN_SIGN_CERT" ;;
        *) set -- -certs "$ONEUM_WIN_SIGN_CERT" ;;
      esac
      [ -z "${ONEUM_WIN_SIGN_PROVIDER:-}" ] || set -- -provider "$ONEUM_WIN_SIGN_PROVIDER" "$@"
      rm -f "$file.signing"
      with_password osslsigncode sign "$@" \
        -pkcs11module "$ONEUM_WIN_SIGN_PKCS11_MODULE" -key "$ONEUM_WIN_SIGN_KEY" \
        -h sha256 -n "$DESCRIPTION" -i "$URL" -ts "$TS" -in "$file" -out "$file.signing" >/dev/null \
        || { rm -f "$file.signing"; die "signing failed: $file"; }
      mv -f "$file.signing" "$file"
      ;;
    command)
      sh -c "$ONEUM_WIN_SIGN_COMMAND" windows-sign "$file" || die "the signing command failed: $file"
      ;;
  esac
  verify_one "$file"
  echo "signed $file"
}

verify_one() {
  file=$1
  need_tool
  [ -f "$file" ] || die "no such file: $file"
  set --
  if [ -n "${ONEUM_WIN_SIGN_CAFILE:-}" ]; then
    [ -r "$ONEUM_WIN_SIGN_CAFILE" ] || die "cannot read ONEUM_WIN_SIGN_CAFILE"
    set -- "$@" -CAfile "$ONEUM_WIN_SIGN_CAFILE"
  fi
  if [ -n "${ONEUM_WIN_SIGN_TSA_CAFILE:-}" ]; then
    [ -r "$ONEUM_WIN_SIGN_TSA_CAFILE" ] || die "cannot read ONEUM_WIN_SIGN_TSA_CAFILE"
    set -- "$@" -TSA-CAfile "$ONEUM_WIN_SIGN_TSA_CAFILE"
  fi
  report=$(osslsigncode verify -in "$file" "$@" 2>&1) || {
    if printf '%s\n' "$report" | grep -q "No signature found"; then
      die "not signed: $file"
    fi
    printf '%s\n' "$report" | grep -E "Error|error|failed|digest" >&2 || true
    die "the signature does not verify: $file"
  }
  # osslsigncode is content with a signature that has no time stamp; this check is not:
  # without one the signature stops counting on the day the certificate runs out.
  printf '%s\n' "$report" | grep -q "^Signature verification: ok" \
    || die "the signature does not verify: $file"
  printf '%s\n' "$report" | grep -q "^Timestamp Server Signature verification: ok" \
    || die "the signature has no time stamp that verifies: $file"
  if [ -n "${ONEUM_WIN_SIGN_SUBJECT:-}" ]; then
    # The first certificate of the report is the signer's.
    subject=$(printf '%s\n' "$report" | sed -n 's/^[[:space:]]*Subject: //p' | head -n 1)
    case "$subject" in
      *"$ONEUM_WIN_SIGN_SUBJECT"*) ;;
      *) die "signed by \"$subject\", not by \"$ONEUM_WIN_SIGN_SUBJECT\": $file" ;;
    esac
  fi
}

action=${1:-}
[ $# -eq 0 ] || shift
case "$action" in
  wanted)
    [ -n "$MODE" ]
    ;;
  ready)
    check_setup
    ;;
  sign)
    [ $# -gt 0 ] || die "sign: no file was named"
    for f in "$@"; do sign_one "$f"; done
    ;;
  verify)
    [ $# -gt 0 ] || die "verify: no file was named"
    for f in "$@"; do verify_one "$f"; echo "signature ok: $f"; done
    ;;
  *)
    die "usage: windows-sign.sh wanted | ready | sign <file>... | verify <file>..."
    ;;
esac
