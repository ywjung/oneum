# Sourced by the scripts that build what is given to others (from the project's
# folder). Rust keeps the names of source files in the program, for the messages of
# its errors, and they begin with this computer's home folder (the account's name)
# and the project's place in it. A build that is given out names them from "home",
# "cargo", "rustup" and "oneum" instead. So does what the C compiler keeps.
#   (Where two of these fit a path, the one that is named later holds.)
ONEUM_SEP=$(printf '\037')
CARGO_ENCODED_RUSTFLAGS="--remap-path-prefix=$HOME=home${ONEUM_SEP}--remap-path-prefix=$HOME/.rustup=rustup${ONEUM_SEP}--remap-path-prefix=$HOME/.cargo=cargo${ONEUM_SEP}--remap-path-prefix=$(pwd)=oneum"
CFLAGS="${CFLAGS:+$CFLAGS }-ffile-prefix-map=$HOME=home"
export CARGO_ENCODED_RUSTFLAGS CFLAGS
