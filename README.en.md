# OnEum

[한국어](README.md) · **English** · [Homepage](https://ywjung.github.io/oneum/en/)

[![CI](https://github.com/ywjung/oneum/actions/workflows/ci.yml/badge.svg)](https://github.com/ywjung/oneum/actions/workflows/ci.yml)

An offline player for the music files you own: add a folder and listen. Written in Rust, made for macOS first, and built for Windows, iPhone and Android from the same code.
The name is the Korean word **온음** ("on-eum"): a whole, unaltered sound, and the whole tone of music.

- Add a music folder and play at once, while it is still being scanned.
- Searching, sorting and scrolling answer immediately in a library of 50,000 songs (partial words, Korean initial consonants, and fields such as `artist:`, `genre:`, `year:1990-1999`, `plays:>=5`, `added:30d`).
- FLAC, WAV, AIFF, ALAC, WavPack, MP3, AAC, Opus and DSD play gapless. Monkey's Audio (APE) plays too. Mono and 5.1/7.1 files are mixed to stereo.
- DSD (.dsf, .dff, DSD64 to DSD512) is converted to PCM (88.2/96 kHz); Matroska/WebM audio (.mka, .webm) and ADPCM WAV play too.
- An album that is one file is split into its songs by the CUE sheet beside it, and those songs follow each other without a gap.
- Queue, playlists (M3U8 import and export), smart playlists (saved searches, with an editor that sets the conditions with buttons), favourites, recently played, shuffle and repeat, stop after the current song, sleep timer.
- Browse by album, artist, genre and folder; pin things to the sidebar; choose the columns of the song list (year, genre, plays, added, last played, format).
- File check: damage (a FLAC is compared with the MD5 it carries), dynamic range (DR), true peak. Statistics (time listened per period, most played artists, albums, songs and genres). Play history and playlists can be brought over from an Apple Music library.
- Volume leveling (ReplayGain): the file's tags are used, and songs without tags are measured by the app (EBU R 128). Music files are never changed.
- Meter deck: analogue VU meters for left and right (needles that move to the standard, PEAK lamps), a 36-band spectrum between them, and the source and output path.
- A WebDAV share of a NAS added as a network folder (the password is kept only in the system's store for passwords; a certificate the NAS made itself is trusted by its fingerprint).
- Media servers of the same network (DLNA/UPnP): finding them or adding one by its address, looking through their folders, playing from them into memory as the music arrives. The network is used only when that view is opened
- A ten-band equalizer (12 presets and your own), headphone correction (profiles for 36 widely used headphones, and correction files such as AutoEq's `ParametricEQ.txt`), headphone crossfeed, and a signal path that lists, in order, what the app does between the file and the device.
- Listen Again (recently added, most played, not heard for long), lyrics (tags and `.lrc`, followed line by line when timed), credits and work information from the tags, a genre view, other versions of the same song, and radio (when the queue ends, similar songs from the library follow).
- The user guide is inside the app (Help → OnEum User Guide), in Korean and English.
- Playback speed (0.5× to 2× at the song's own pitch), crossfade between songs, a section repeated (A–B), ratings.
- Room correction (optional): an impulse response made by measuring the room is imported and applied by a convolution that adds no delay.
- Upsampling (optional): the device runs at the highest whole multiple of the song's sample rate it offers, with a choice of filter (sharp, slow, minimum phase).
- Lyrics from the internet (optional, off unless switched on): for songs whose file has no synced lyrics, looked up on LRCLIB by the song's title, artist, album and length, and nothing else.
- Optional output quality settings: set the device to the song's sample rate and play without resampling, exclusive output, memory playback. Dither is added only where an integer device needs it.
- Where resampling is needed it is flat to 20 kHz (±0.001 dB; the measurements are in docs/ARCHITECTURE.md), and peaks above full scale are held down for that moment instead of being clipped (peak protection).
- Drag songs and albums to playlists and the queue; select, play and tidy with the keyboard alone; type to find a song.
- Media keys and the system's Now Playing, opening files from Finder, Korean and English, light and dark.
- No account, no server, no advertising, no usage data. Music files are read and never changed.

## Requirements

| | |
|---|---|
| OS | Declared for macOS 14 and later. **Verified on macOS 27.2.1 only.** |
| CPU | Apple Silicon (arm64). Intel Macs are not verified and not supported. |
| Build | Rust 1.99.1 (`rust-toolchain.toml`), the Xcode command line tools |

## Running

```bash
scripts/package.sh
```

```bash
open dist/OnEum.app
```

`scripts/package.sh --dmg` makes a disk image for installing (`dist/OnEum-<version>-arm64.dmg`).
A build made this way is not signed with a Developer ID and not notarised, so Gatekeeper warns on other Macs (the disk image on the releases page is signed and notarised).
Details: [docs/RELEASE.md](docs/RELEASE.md) (Korean).

## Windows

```bash
scripts/package-windows.sh
```

Cross-compiles on a Mac and makes `dist/OnEum-<version>-windows-x86_64.zip` (unzip and run) and the setup program `dist/OnEum-<version>-windows-x86_64-setup.exe` (needs `rustup target add x86_64-pc-windows-gnu` and `brew install mingw-w64`; the setup program is made when `brew install makensis` is there).
The setup program installs for the user who runs it, without administrator rights, and adds a Start menu shortcut and an entry in "Installed apps" for removing it. `OnEum.exe` carries its icon and version details.
**The Windows build has only been compiled and linked. It has not been run on a real Windows machine, and the setup program has only been built, not run.** (The CI workflow builds, tests and starts it on a Windows runner; that has not run yet.)
The files for Windows on the releases page are not signed yet; the plan and the procedure are in the [Code signing policy](https://ywjung.github.io/oneum/en/code-signing.html). A workflow (`.github/workflows/windows-package.yml`) builds the packages with the same script on a GitHub runner. A package for the Microsoft Store (MSIX) is being prepared: [docs/MICROSOFT_STORE.md](docs/MICROSOFT_STORE.md) (in Korean).
What differs there (a menu bar inside the window, Ctrl shortcuts, features that are missing) is in the [Windows user guide](docs/USER_GUIDE_WINDOWS.en.md), and in "플랫폼" of [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) and "Windows" of [docs/TESTING.md](docs/TESTING.md) (Korean).

## iPhone

The iPhone app is on the App Store (free, iOS 16 or later): **[OnEum on the App Store](https://apps.apple.com/app/id6838073525)**. It is not on the App Store of France, mainland China and the member states of the EU.

To build and run it from source:

```bash
scripts/ios-sim.sh
```

Installs and starts the app in the iOS Simulator (needs `rustup target add aarch64-apple-ios aarch64-apple-ios-sim` and Xcode). For a real iPhone:

```bash
scripts/ios-device.sh
```

Connect the iPhone, unlock it and let it trust this Mac first. Xcode needs an Apple ID, and the iPhone needs Developer Mode switched on.
Opening `ios/OnEum.xcodeproj` in Xcode, choosing a team and pressing Run does the same.
The team to sign with is taken from the development certificate in the keychain. To sign with another one (a free personal team, say), give its ID in `ONEUM_IOS_TEAM` and an identifier that team can use in `ONEUM_IOS_BUNDLE_ID`.

The iPhone has screens of its own: tabs for the library, search, playlists and settings; a player that shows the cover, the lyrics, the VU meters with the spectrum, or the queue; pins, sorting, choosing several songs at once, reordering playlists and the queue by dragging, smart playlists, song info with credits and the signal path, the file check, the equalizer and headphone correction; playback in the background; searching inside a list; linking a folder in iCloud Drive or on a plugged-in drive without copying it; output at the song's sample rate (asked of the iPhone); importing an Apple Music library; landscape.
Not there: choosing an output device and exclusive output (the iPhone decides), the menu bar and keyboard shortcuts, the choice of list columns, and screens made for the iPad.

**The iPhone version is verified in the iOS 27.0 Simulator. On a real iPhone (iOS 27.2.1) the screens of the build that is on the App Store (0.1.0) have been seen: the library, playing, the player, the equalizer, search and Control Center. Sound, the Lock Screen, earphone buttons and linked folders have not been checked by us on a real iPhone.**
Usage: [docs/USER_GUIDE_IOS.en.md](docs/USER_GUIDE_IOS.en.md). What differs and what was verified: [docs/adr/0002-iphone.md](docs/adr/0002-iphone.md) and the "iPhone" section of [docs/TESTING.md](docs/TESTING.md) (both Korean).

## Android

```bash
scripts/package-android.sh
```

Packs the app (a shared library built from the Rust code) and a thin Java shell (`android/`) into an APK, `dist/OnEum-<version>-android-arm64.apk`. Needs the Android SDK with an NDK, `rustup target add aarch64-linux-android`, and a JDK 17 or 21.
The APK is what installs the app: bring it to the phone (by cable, a cloud drive …) and open it in the Files app. The first time, that app has to be allowed to "install unknown apps", and where Google Play Protect recommends a scan of an app it has not seen, "More details → Install without scanning" goes on. It is for 64-bit ARM phones with Android 11 or later and is signed with this computer's debug key (for your own phone, not for publishing).
To install and start it straight away on a connected device or emulator (a device needs USB debugging switched on):

```bash
scripts/android-device.sh
```

The folder `android/` can also be opened in Android Studio (the script above builds the app's library first).

Android uses the same phone screens as the iPhone. Allow access to "music and audio" when the system asks on the first start and the phone's Music folder is read where it is; other folders (also on a card) can be added. WavPack, Monkey's Audio and DSD files, and cover pictures, lyrics and CUE files, are read once "All files access" is allowed as well, in the settings. TalkBack works in part: swiping from item to item and double-tapping to use an item (checked in the emulator, not on a real phone). Explore by touch does not work, and no frame is drawn around the item TalkBack is on.
Music plays on with the screen off (controls in the notification and on the lock screen) and pauses for a call. The system's Back works throughout.
The shortcuts of a keyboard that is plugged in or paired (Space, Ctrl+→ …), opening single music files without adding their folder, showing a song's folder in the Files app, and saving the log are there.
Not there: TalkBack's explore by touch and focus frame, choosing an output device, matching a USB DAC's sample rate, exclusive output, and screens made for tablets.

**The Android version is verified in an Android 16 emulator. It has not been installed on a real Android device yet.**
Usage: [docs/USER_GUIDE_ANDROID.en.md](docs/USER_GUIDE_ANDROID.en.md). The plan and how it went: [docs/ANDROID_PLAN.md](docs/ANDROID_PLAN.md); what was verified: "Android" in [docs/TESTING.md](docs/TESTING.md) (both Korean).

## Development

```bash
scripts/check.sh
```

runs what CI runs: formatting, lints (for macOS, and for Windows, iPhone and Android when their targets are installed) and the tests. The single steps:

```bash
cargo fmt --all --check
```

```bash
cargo clippy --workspace --all-targets --locked -- -D warnings
```

```bash
cargo test --workspace --locked
```

```bash
cargo build --release --locked
```

- The tests need no audio device (sound is rendered offline and checked sample by sample).
- The real app can be started with a throw-away profile and driven by a script: `scripts/dev/smoke.sh` (macOS) and `scripts/dev/smoke-ios.sh` (iOS Simulator); see [docs/TESTING.md](docs/TESTING.md).

## Documents

The user guides are in Korean and English; the other documents are in Korean.

| Document | What it holds |
|---|---|
| [docs/USER_GUIDE.en.md](docs/USER_GUIDE.en.md) · [docs/USER_GUIDE.md](docs/USER_GUIDE.md) | User guide for macOS (the text of the app's Help menu) |
| [docs/USER_GUIDE_WINDOWS.en.md](docs/USER_GUIDE_WINDOWS.en.md) · [docs/USER_GUIDE_WINDOWS.md](docs/USER_GUIDE_WINDOWS.md) | User guide for Windows |
| [docs/USER_GUIDE_IOS.en.md](docs/USER_GUIDE_IOS.en.md) · [docs/USER_GUIDE_IOS.md](docs/USER_GUIDE_IOS.md) | User guide for iPhone |
| [docs/USER_GUIDE_ANDROID.en.md](docs/USER_GUIDE_ANDROID.en.md) · [docs/USER_GUIDE_ANDROID.md](docs/USER_GUIDE_ANDROID.md) | User guide for Android |
| [CHANGELOG.md](CHANGELOG.md) | What changed |
| [docs/PRD.md](docs/PRD.md) | Users, scope, what "done" means |
| [docs/UX.md](docs/UX.md) | Screens, interaction, how playback behaves |
| [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) | Modules, threads, the audio path, storage |
| [docs/TESTING.md](docs/TESTING.md) | How it is tested, format and gapless tables, measurements, what is still unverified |
| [docs/RELEASE.md](docs/RELEASE.md) | Packaging, signing, notarisation, distribution |
| [docs/PLAN.md](docs/PLAN.md) · [docs/STATUS.md](docs/STATUS.md) | Phases and the current state |
| [docs/adr/0001-gui-stack.md](docs/adr/0001-gui-stack.md) · [docs/adr/0002-iphone.md](docs/adr/0002-iphone.md) | Decisions: the GUI stack, the iPhone version |
| [docs/IOS_PLAN.md](docs/IOS_PLAN.md) | The plan for the iPhone version and how it went |
| [docs/APP_STORE.md](docs/APP_STORE.md) | Putting the iPhone app on the App Store: signing, the archive, what a review asks (Korean) |
| [docs/MAC_APP_STORE.md](docs/MAC_APP_STORE.md) | Preparing the Mac app for the Mac App Store: the sandbox, the package, what was checked (Korean) |
| [docs/ANDROID_PLAN.md](docs/ANDROID_PLAN.md) | The plan for the Android version and how it goes |
| [site/](site/) | The homepage (published with GitHub Pages; see "홈페이지" in docs/RELEASE.md) |

## License

[0BSD](LICENSE) (BSD Zero Clause): anyone may use, change, redistribute and sell it for any purpose, with no conditions and no need to give credit (and with no warranty). The open-source software and data OnEum uses stay under their own licenses, and their notices have to go along when a package that contains them is passed on. The open-source software and data used (the built-in headphone correction profiles are AutoEq's values): [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
