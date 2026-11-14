// Stands in for the app's program while Xcode builds the bundle: the build phase
// "Build the Rust program" (build-rust.sh) puts the real one, built by cargo, in its
// place before the app is signed. Xcode needs something to compile and link; this is it.
int main(void) {
    return 0;
}
