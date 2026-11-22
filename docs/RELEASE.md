# 릴리스 절차

기본 배포 형태는 직접 내려받는 `.dmg`(안에 `OnEum.app`)입니다. Mac App Store·자동 업데이트는 범위 밖입니다. iPhone 앱을 App Store에 내는 절차는 [APP_STORE.md](APP_STORE.md)에 있습니다.

## 현재 상태 (0.1.0)
| 단계 | 상태 |
|---|---|
| 재현 가능한 빌드·번들·DMG | 완료 (`scripts/package.sh --dmg`) |
| ad-hoc 서명(하드닝 런타임 포함), `codesign --verify` | 완료 |
| 별도 위치에 설치해 실행 | 완료 (docs/TESTING.md "패키지 검증") |
| Developer ID 서명 + Hardened Runtime | **검증 대기** — 인증서 없음 |
| 공증(notarytool), stapling, `spctl` 평가 | **검증 대기** — Apple 계정 자격 증명 없음 |
| 라이선스 전문 묶음 | 완료 — `THIRD_PARTY_LICENSES.txt`(`scripts/gen-licenses.py`, 패키징 때 자동 생성) |
| 검증 채널 제외 | 완료 — 배포 빌드에는 `automation` 기능이 없음 |
| Windows 실제 실행 확인 | **검증 대기** — 실제 Windows 필요 |
| iPhone: 시뮬레이터 실행, 기기용 빌드 | 완료 — `scripts/dev/smoke-ios.sh`(69항목), Xcode 프로젝트로 기기용 릴리스 빌드 |
| 소스 공개 준비 | 준비됨(2026-11-14) — 원격 `github`(https://github.com/ywjung/oneum, 비어 있는 공개 저장소). 추적 파일과 지난 커밋의 내용에 비밀 값·개인 주소·홈 경로·서명 팀 ID·기기 식별자가 없음을 확인, 저장소 크기 약 5MB(가장 큰 파일 1MB), 오픈 소스 고지가 iPhone 빌드까지 포함, CI에 iPhone 린트 추가. 커밋 기록의 작성자는 모두 한 이름과 한 e-mail 주소로 통일했다(이 저장소의 git 설정도 같음). 이 Mac의 로컬 계정 이름은 기록에 남지 않고, 그 e-mail 주소는 push하면 공개된다. 새 클론에서 빌드와 테스트 562개가 통과함을 확인. push와 CI 첫 실행은 **아직 하지 않음** |
| Android: 에뮬레이터 실행, 릴리스 APK | 완료(2026-11-15) — `scripts/dev/smoke-android.sh`(44항목), 설치 파일 `scripts/package-android.sh` → `dist/OnEum-<버전>-android-arm64.apk`(에뮬레이터의 파일 앱에서 열어 설치·실행 확인). 디버그 키 서명이며 배포용 키 서명과 Play 스토어는 범위 밖. 실제 기기 설치는 **검증 대기** |
| iPhone: 실제 기기 설치·실행 | 설치·실행 확인(2026-11-14, 개인 팀 서명, `scripts/ios-device.sh`). 기기에서의 화면·재생 확인은 **검증 대기** |
| iPhone: App Store용 아카이브 | 서명 없는 아카이브와 그 검사까지 완료(2026-11-22, `scripts/ios-archive.sh --unsigned`). 서명 · `.ipa` 내보내기 · 업로드는 **검증 대기** — 유료 Apple Developer Program 계정 필요([APP_STORE.md](APP_STORE.md)) |

서명·공증되지 않은 빌드를 정식 배포물로 취급하지 마세요. 다른 Mac에서는 Gatekeeper가 실행을 막습니다.

## 1. 준비
- `Cargo.toml`의 `version`을 올리고 `CHANGELOG.md`를 갱신합니다. 빌드 번호는 git 커밋 수에서 자동으로 정해집니다.
- 의존성이 바뀌었으면 `python3 scripts/gen-notices.py`로 `THIRD_PARTY_NOTICES.md`를 다시 만듭니다. 라이선스 전문(`THIRD_PARTY_LICENSES.txt`)은 패키징 스크립트가 만듭니다.
- `vendor/lofty`는 lofty 0.25.5에서 한 곳을 고친 사본입니다(`vendor/lofty/README-ONEUM.md`). lofty를 올릴 때, 새 버전이 `id3v2_compression_support` 없이 빌드되면 폴더와 `Cargo.toml`의 `[patch.crates-io]`를 지웁니다.
- 아이콘을 바꿨으면 `scripts/gen-icon.sh`.

## 2. 검사
아래 세 가지(그리고 mingw-w64가 있으면 Windows 대상 clippy)를 한 번에 돌리는 스크립트가 `scripts/check.sh`입니다. 저장소를 GitHub에 올리면 `.github/workflows/ci.yml`이 macOS·Windows에서 같은 검사와 Windows 실행 확인, 의존성 권고 점검을 합니다.
```bash
scripts/check.sh
```
```bash
cargo fmt --all --check
```
```bash
cargo clippy --workspace --all-targets --locked -- -D warnings
```
```bash
cargo test --workspace --locked
```

## 3. 패키징
```bash
scripts/package.sh --dmg
```
결과물:
- `dist/OnEum.app` — 번들(`Info.plist`, 아이콘, 고지와 라이선스 전문 포함. 사용 안내는 실행 파일에 내장)

배포용 빌드(Mac · Windows · Android · iPhone)에는 빌드한 컴퓨터의 경로가 들어가지 않습니다: Rust가 오류 메시지용으로 넣는 소스 경로의 홈 폴더와 프로젝트 폴더 부분을 `scripts/release-paths.sh`가 `home` · `cargo` · `rustup` · `oneum`으로 바꿉니다. 공개 전에 `grep -a -c /Users/ <프로그램>`이 0인지 확인합니다(macOS의 `strings`는 `-` 없이 쓰면 프로그램의 섹션만 읽어서, 벗겨 내지 않은 프로그램의 기호 표에 남은 경로를 놓칩니다).

`scripts/package.sh`는 검증 채널(`ONEUM_SCRIPT`)이 없는 빌드를 만듭니다. 그 채널이 든 빌드는 `scripts/dev/run.sh`(`ONEUM_AUTOMATION=1`)가 `dist/dev`에만 만듭니다. **`dist/dev`의 앱을 배포하지 마세요.**
- `dist/OnEum-<버전>-arm64.dmg` — 앱과 `/Applications` 바로가기
- `dist/SHA256SUMS.txt`

아키텍처: Apple Silicon(arm64) 전용입니다. Intel/유니버설 빌드는 검증 장비가 없어 만들지 않습니다
(`rustup target add x86_64-apple-darwin` 후 `lipo`로 합칠 수는 있으나, 검증 전에는 지원한다고 표기하지 않습니다).

### Windows판
```bash
rustup target add x86_64-pc-windows-gnu
```
```bash
brew install mingw-w64
```
```bash
brew install makensis
```
```bash
scripts/package-windows.sh
```
세 가지가 만들어집니다.
- `dist/OnEum-<버전>-windows-x86_64.zip`: OnEum.exe, LICENSE.txt, Windows 빌드 기준 THIRD_PARTY_NOTICES.md, README.txt. 압축을 푼 폴더에서 바로 실행합니다.
- `dist/OnEum-<버전>-windows-x86_64-setup.exe`: 설치 프로그램(NSIS 3, `assets/windows/installer.nsi`). `makensis`가 없으면 만들지 않고 그렇게 알립니다.
- `dist/SHA256SUMS-windows.txt`: 두 파일의 체크섬.

설치 프로그램이 하는 일:
- 실행한 사용자의 계정에 설치합니다(관리자 권한을 묻지 않음). 기본 위치는 `%LOCALAPPDATA%\Programs\OnEum`이고 바꿀 수 있습니다.
- 시작 메뉴에 바로 가기를 만들고, 마지막 화면에서 고르면 바탕 화면에도 만듭니다.
- "설치된 앱"(프로그램 추가/제거)에 항목을 만들고, 거기서 제거합니다. 제거할 때 라이브러리와 설정(`%APPDATA%\OnEum`, `%LOCALAPPDATA%\OnEum`)도 지울지 묻고, 기본은 남기는 것입니다. 음악 파일은 건드리지 않습니다.
- OnEum이 실행 중이면 닫으라고 알리고 기다립니다. 새 버전은 같은 자리에 덮어 설치합니다.
- 64비트 Windows 10 이상이 아니면 설치하지 않습니다. 언어는 Windows의 언어에 따라 한국어 또는 영어입니다.

`OnEum.exe`에는 아이콘(16~256px 아홉 가지)과 버전 정보가 들어갑니다. `build.rs`가 `assets/windows/oneum.rc`를 mingw-w64의 `windres`로 컴파일해 링크합니다.
아이콘 파일은 `python3 scripts/gen-windows-icon.py`로 `assets/icon-1024.png`에서 만듭니다(Pillow 필요, 결과는 저장소에 들어 있음).
패키징 스크립트는 실행 파일에 아이콘이 들어 있는지 검사하고, 없으면 실패합니다.
(mac에서 `makensis`는 로케일이 설정되지 않은 셸에서 메모리 오류로 끝나므로 스크립트가 UTF-8 로케일로 실행합니다.)
- 실행 파일도 설치 프로그램도 서명하지 않으므로 Windows SmartScreen 경고가 나옵니다(코드 서명 인증서가 필요합니다).
- 스크립트는 실행 파일이 컴파일러 런타임 DLL을 필요로 하지 않는지만 검사합니다. **Windows에서 실행해 보는 단계는 없습니다** — 배포 전에 실제 Windows에서
  docs/TESTING.md "Windows"의 항목을 확인해야 합니다.
- **설치 프로그램은 만들어지는 것까지만 확인했습니다.** Windows에서 설치·실행·제거해 본 적이 없습니다(docs/TESTING.md "Windows").

### Android판
```bash
rustup target add aarch64-linux-android
```
```bash
scripts/package-android.sh
```
준비물: Android SDK(플랫폼 35, 빌드 도구)와 NDK(`scripts/android-env.sh`가 `ANDROID_NDK_HOME` 또는 SDK 아래에서 찾는다), JDK 17 또는 21. Gradle과 Android Gradle 플러그인은 처음 빌드할 때 받는다(`android/gradlew`).

결과물: `dist/OnEum-<버전>-android-arm64.apk`와 `dist/SHA256SUMS-android.txt`(arm64 전용, Android 11 이상). 앱 ID는 `app.oneum.android`, 버전 이름은 `Cargo.toml`의 버전, 버전 코드는 커밋 수다. APK가 Android의 설치 파일이다. 스크립트는 릴리스 빌드(`scripts/android-build.sh --release`, 검증 채널 없음)를 만들고, OnEum의 라이선스와 오픈 소스 라이선스 전문(`scripts/gen-licenses.py --android`)을 APK의 `assets`에 넣으며, APK에 앱의 라이브러리와 그 두 파일이 들어 있는지, 서명이 맞는지, 디버그 가능한 빌드가 아닌지 확인한다.

휴대폰에 설치: APK를 휴대폰으로 옮겨 파일 앱에서 연다. 처음에는 그 앱에 "출처를 알 수 없는 앱 설치"를 허용하라는 안내가 나오고(설정 → 이 출처 허용 → 뒤로), 다시 열면 설치를 묻는다. Google Play Protect가 "처음 보는 앱"이라며 검사를 권하면 **자세히 → 검사 없이 설치**로 진행할 수 있다("앱 검사"는 APK를 Google로 보낸다).

서명: 환경 변수 `ONEUM_ANDROID_KEYSTORE`(키 저장소 파일), `ONEUM_ANDROID_KEYSTORE_PASSWORD`, `ONEUM_ANDROID_KEY_ALIAS`, `ONEUM_ANDROID_KEY_PASSWORD`를 주면 그 키로 서명하고, 없으면 이 컴퓨터의 디버그 키로 서명한다. **디버그 키로 서명한 APK는 내 기기에 넣어 쓰는 용도이며 배포물이 아니다**(다른 키로 서명한 다음 버전은 덮어 설치할 수 없다). 키 저장소와 비밀번호는 저장소에 넣지 않는다.

연결된 기기나 에뮬레이터에 설치: `scripts/android-device.sh`(USB 디버깅을 켠 기기). `--automation`으로 만든 빌드는 검증 채널이 들어 있으므로 배포하지 않는다.

음악은 "음악 및 오디오" 권한(`READ_MEDIA_AUDIO`)으로 읽는다. "모든 파일에 대한 접근"(`MANAGE_EXTERNAL_STORAGE`)은 설정에서 고르는 선택으로 남아 있고 매니페스트에 선언되어 있으므로, Play 스토어에 올리려면 그 권한의 사용 목적을 신고하거나 선언을 빼야 한다(빼면 WavPack · APE · DSD 파일과 옆의 표지 · 가사 · CUE를 못 읽는다, [ANDROID_PLAN.md](ANDROID_PLAN.md) 4.1).

## 4. Developer ID 서명 (자격 증명 필요)
필요한 것: Apple Developer Program의 **Developer ID Application** 인증서(키체인에 설치).
```bash
security find-identity -v -p codesigning
```
```bash
ONEUM_SIGN_IDENTITY="Developer ID Application: <이름> (<팀 ID>)" scripts/package.sh --dmg
```
스크립트가 `--options runtime`(Hardened Runtime)과 `--timestamp`로 서명합니다.
OnEum은 JIT·외부 라이브러리 로딩·마이크·카메라를 쓰지 않으므로 **entitlements 파일이 필요 없습니다.**
```bash
codesign --verify --deep --strict --verbose=2 dist/OnEum.app
```
```bash
codesign -d --entitlements - --verbose=2 dist/OnEum.app
```

## 5. 공증 (자격 증명 필요)
필요한 것: Apple ID, 앱 암호, 팀 ID. 비밀 값은 키체인 프로필에만 저장하고 저장소·로그에 남기지 않습니다.
```bash
xcrun notarytool store-credentials oneum-notary --apple-id <Apple ID> --team-id <팀 ID>
```
```bash
xcrun notarytool submit dist/OnEum-<버전>-arm64.dmg --keychain-profile oneum-notary --wait
```
```bash
xcrun stapler staple dist/OnEum-<버전>-arm64.dmg
```
```bash
spctl --assess --type open --context context:primary-signature --verbose=4 dist/OnEum-<버전>-arm64.dmg
```
```bash
spctl --assess --type execute --verbose=4 /Volumes/OnEum*/OnEum.app
```
stapling 후에는 체크섬이 바뀌므로 `SHA256SUMS.txt`를 다시 만듭니다: `(cd dist && shasum -a 256 *.dmg > SHA256SUMS.txt)`.

## 6. 설치 확인
1. DMG를 다른 사용자 계정이나 다른 Mac에서 엽니다(격리 속성이 붙은 상태로 확인하려면 브라우저로 내려받습니다).
2. `OnEum.app`을 `/Applications`로 끌어다 놓고 실행합니다.
3. 음악 폴더 추가 → 재생 → 종료 → 재실행(상태 복원)을 확인합니다.
4. docs/TESTING.md의 "검증 대기" 항목 중 가능한 것을 확인하고 결과를 기록합니다.

## 7. 공개
- 공개 업로드는 권한이 있는 사람이 수행합니다. 공개 전에 결과물(`dist/*.dmg`, `SHA256SUMS.txt`)과 `CHANGELOG.md`를 검토합니다.
- 라이선스 전문 묶음(`THIRD_PARTY_LICENSES.txt`)이 번들 `Resources`와 Windows zip에 들어 있는지 확인합니다. 각 crate 패키지에 든 LICENSE·COPYING·NOTICE 파일을 모은 것이고,
  파일이 없는 crate는 목록과 함께 표준 본문이 묶음 안에 있는지 스크립트가 검사합니다(없으면 실패).
- 판매·공개 전 남은 일: Developer ID 서명과 공증(4·5절), Windows 실제 실행 확인(docs/TESTING.md "Windows"), 그리고 가능하면 Windows 코드 서명(없으면 SmartScreen 경고).

## iPhone: App Store 제출 준비 (2026-11-18 점검)
**가입부터 심사까지의 순서, 아카이브 스크립트(`scripts/ios-archive.sh`), 스토어 정보 점검표는 [APP_STORE.md](APP_STORE.md)에 있습니다.** 아래는 서명 전까지 확인하고 고친 것과, 제출하는 사람이 할 일입니다. 서명 없는 아카이브(`xcodebuild … archive CODE_SIGNING_ALLOWED=NO`)는 앱과 위젯 확장이 같은 버전 · 빌드 번호로 들어간 정상 구조이고, 안에 아래 파일들이 있습니다.

| 항목 | 상태 |
|---|---|
| 개인정보 매니페스트 | `ios/PrivacyInfo.xcprivacy`. 추적 없음, 수집하는 데이터 없음. 앱 바이너리가 쓰는 사유 필요 API(`nm -u`로 확인): 파일 시각 C617.1 · 3B52.1, 시스템 가동 시간 35F9.1, 디스크 공간 E174.1(번들한 SQLite가 데이터베이스를 쓰기 전에 볼륨 종류를 묻는 것 — 딱 맞는 사유가 없어 가장 가까운 것). 위젯 확장은 이런 API를 쓰지 않음 |
| 권한 안내문의 번역 | `ios/ko.lproj` · `ios/en.lproj`의 `InfoPlist.strings`(로컬 네트워크) |
| 심사자가 쓸 음악 | 빈 보관함 화면의 **샘플 음악 넣기**: 앱에 든 지어낸 곡 9개(3개 앨범, `assets/samples`, 약 3MB, `scripts/dev/gen-demo-library.py --samples`로 만듦)를 OnEum 폴더에 복사 |
| iPad에서 | iPhone 전용 앱이 iPad의 창으로 열릴 때 창 단추(왼쪽 위의 점 세 개)를 피해 화면이 그 아래에서 시작(`platform::ios::scene::safe_area_insets`) |
| 암호화 수출 | `ITSAppUsesNonExemptEncryption = false`로 되어 있음. 앱은 시스템의 것이 아닌 TLS 구현(rustls · ring)을 담고 있어, 프랑스에 배포하려면 신고가 필요한 분류로 보임 — **결정 필요** |
| 서버 자동 찾기 | iPhone에서 SSDP 멀티캐스트는 Apple의 `com.apple.developer.networking.multicast` 승인이 필요하고 프로젝트에 없음. 승인 전에는 자동 찾기가 막히고 주소로 추가는 됨 — **결정 필요** |
| 인터넷 가사(LRCLIB) | 꺼져 있는 선택 기능. 저작권 있는 가사를 제3자에게서 가져오므로 심사에서 문제 삼을 수 있음 — **결정 필요** |
| 계정 · 주소 | 유료 Apple Developer Program, 지원 URL, 앱 이름 확인 — **제출하는 사람의 일**. 개인정보 처리방침은 초안이 `site/privacy.html` · `site/en/privacy.html`에 있음(2026-11-22) |

심사 메모(App Review Information → Notes)에 쓸 글의 초안:

> OnEum plays music files that are on the device; it has no account and no server of its own. To try it without music of your own: open the app, and on the empty Library screen tap **Add Sample Music**. Nine short made-up songs (three albums, synthesized sound, no one's recordings) are copied into the app's folder; play any of them from Library → Albums. Media servers (DLNA) and WebDAV folders on a local network are optional. Online lyrics are off by default (Settings → General → Lyrics).

## 샌드박스와 권한
- 직접 배포이므로 **App Sandbox는 쓰지 않습니다.** 사용자가 고른 폴더를 일반 파일 권한으로 읽습니다.
  (샌드박스를 도입하려면 security-scoped bookmark의 저장·복원·해제를 함께 구현해야 합니다.)
- Hardened Runtime은 Developer ID 서명 시 켭니다(샌드박스와 별개).
- 마이크, 카메라, 전체 디스크 접근, 네트워크 권한을 요구하지 않습니다. 앱은 네트워크에 접속하지 않습니다.

## 홈페이지 (GitHub Pages)
프로그램을 소개하는 페이지가 `site/`에 있습니다. 한국어(`site/index.html`)와 영어(`site/en/index.html`) 두 장이고, 같은 스타일시트(`site/style.css`)와 스크립트(`site/meters.js`, 움직이는 VU 미터)를 씁니다. 개인정보 처리방침(`site/privacy.html`, `site/en/privacy.html`)도 여기 있습니다: 앱이 기기 밖으로 보내는 것이나 묻는 권한이 바뀌면 두 언어의 페이지와 날짜를 함께 고칩니다.
외부 글꼴·스크립트·추적 코드를 불러오지 않으며, 쿠키도 쓰지 않습니다.

게시하는 방법:
1. 저장소를 GitHub에 올립니다(`git push -u github main`).
2. GitHub 저장소의 **Settings → Pages → Build and deployment → Source**를 **GitHub Actions**로 바꿉니다(한 번만).
3. `.github/workflows/pages.yml`이 `main`의 `site/`가 바뀔 때마다 게시합니다. Actions 탭에서 "Pages"를 직접 실행할 수도 있습니다.
4. 주소는 `https://ywjung.github.io/oneum/`(영어는 `/oneum/en/`)입니다.

미리 보기:
```bash
python3 -m http.server 8799 --bind 127.0.0.1 --directory site
```
브라우저에서 `http://localhost:8799/`를 엽니다. (파일을 직접 열면 스타일과 그림이 붙지 않습니다.)

화면 그림을 다시 만들 때:
- 그림 속 음악가와 앨범은 지어낸 것입니다. `python3 scripts/dev/gen-demo-library.py <폴더>`가 합성음과 그린 표지로 라이브러리를 만듭니다(ffmpeg, Pillow 필요).
- macOS 화면은 `scripts/dev/run.sh`로 띄운 앱에서 `shot` 명령으로(앱 볼륨 0), iPhone 화면은 시뮬레이터에서 같은 방법으로 찍어 `site/img/`에 넣습니다. 언어는 설정의 `language`로 바꿉니다.
- 페이지의 "확인한 범위" 표는 docs/TESTING.md와 같아야 합니다. 플랫폼의 상태가 바뀌면 두 언어의 페이지를 함께 고칩니다.

확인한 것(2026-11-15): 로컬 서버로 열어 두 언어의 페이지가 넓은 화면(1280px)과 좁은 화면(390px)에서 넘침 없이 그려지고, 밝은 화면과 어두운 화면 모두 읽히며, VU 미터가 움직이고 버튼으로 멈추는 것, 페이지가 가리키는 사이트 안의 파일이 모두 있는 것, 다른 서버로 가는 요청이 없는 것. **GitHub에 게시해 본 적은 없습니다**(워크플로는 실행된 적이 없습니다).
