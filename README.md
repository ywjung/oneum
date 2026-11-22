# OnEum

**한국어** · [English](README.en.md) · [홈페이지](https://ywjung.github.io/oneum/)

[![CI](https://github.com/ywjung/oneum/actions/workflows/ci.yml/badge.svg)](https://github.com/ywjung/oneum/actions/workflows/ci.yml)

폴더만 추가하면 바로 듣는 오프라인 로컬 음악 플레이어. Rust로 작성했고 macOS가 주 대상이며 Windows, iPhone, Android용으로도 빌드됩니다.
이름은 우리말 **온음**(온전한 소리, 그리고 음악의 온음)에서 왔습니다.

- 음악 폴더를 추가하면 스캔하면서 곧바로 재생할 수 있습니다.
- 5만 곡 라이브러리에서도 검색·정렬·스크롤이 즉시 반응합니다(한글 부분 검색, 초성 검색, `artist:`·`genre:`·`year:1990-1999`·`plays:>=5`·`added:30d` 같은 필드 지정).
- FLAC·WAV·AIFF·ALAC·WavPack·MP3·AAC·Opus·DSD는 곡 사이 끊김 없이(gapless) 이어집니다. Monkey's Audio(APE)도 재생합니다. 모노와 5.1·7.1 파일은 스테레오로 섞어 재생합니다.
- DSD(.dsf, .dff, DSD64~512)는 PCM(88.2/96kHz)으로 바꿔 재생하고, Matroska·WebM 오디오(.mka, .webm)와 ADPCM WAV도 재생합니다.
- 앨범 전체가 파일 하나인 경우 옆의 CUE 시트대로 곡을 나눠 보여 주고, 곡 사이를 끊김 없이 잇습니다.
- 대기열, 플레이리스트(M3U8 가져오기/내보내기), 스마트 플레이리스트(저장한 검색), 즐겨찾기, 최근 재생, 셔플·반복, 현재 곡이 끝나면 정지, 잠자기 타이머.
- 앨범·아티스트·장르·폴더로 둘러보기, 사이드바 고정, 곡 목록에 연도·장르·재생 횟수·추가·마지막 재생·포맷 열을 골라서 표시.
- 파일 검사: 손상(FLAC은 파일의 MD5와 대조), 다이내믹 레인지(DR), 트루 피크. 통계(기간별 들은 시간, 많이 들은 아티스트·앨범·곡·장르). Apple Music 보관함에서 재생 기록·플레이리스트 옮겨 오기.
- 음량 보정(ReplayGain): 파일의 태그를 쓰고, 태그가 없는 곡은 앱이 음량을 재서(EBU R 128) 보정합니다. 음악 파일은 바꾸지 않습니다.
- 미터 덱: 좌·우 아날로그 VU 미터(규격대로 움직이는 바늘과 PEAK 램프)와 그 사이의 36대역 스펙트럼 표시, 소스·출력 경로 표시.
- NAS의 WebDAV 공유를 네트워크 폴더로 추가해 듣기(비밀번호는 운영체제의 보관소에만 저장, 스스로 만든 인증서는 지문을 확인해 신뢰).
- 같은 네트워크의 미디어 서버(DLNA/UPnP) 둘러보기와 재생: 서버 찾기·주소로 추가, 받는 대로 메모리에 올려 재생. 네트워크는 그 화면을 열 때만 씁니다
- 10대역 이퀄라이저(기본 프리셋 12개 + 내 프리셋 저장), 헤드폰 보정(널리 쓰이는 헤드폰 36종의 기본 프로필 + AutoEq의 `ParametricEQ.txt` 같은 보정값 파일 불러오기), 헤드폰 크로스피드, 그리고 파일에서 장치까지 앱이 하는 일을 순서대로 보여 주는 신호 경로.
- 다시 듣기(최근 추가 · 많이 들은 곡 · 오래 안 들은 곡), 가사 표시(태그·`.lrc`, 시간 표시가 있으면 싱크), 태그에 든 크레딧·작품 정보, 장르 보기, 같은 곡의 다른 버전, 라디오(대기열이 끝나면 라이브러리에서 어울리는 곡을 이어 재생).
- 사용 안내를 앱 안에서 봅니다(도움말 → OnEum 사용 안내). 언어를 English로 두면 안내도 영어로 나옵니다.
- 재생 속도(0.5×~2×, 음높이 그대로), 곡 사이 크로스페이드, 구간 반복(A–B), 별점.
- 룸 보정(선택): 방을 측정해 만든 임펄스 응답 파일을 불러와 지연 없는 컨볼루션으로 적용.
- 업샘플링(선택): 장치가 내는 가장 높은 정수배 샘플레이트로 올려 출력하고, 필터를 선명 · 완만 · 최소 위상 중에 고름.
- 인터넷에서 가사 찾기(선택, 켜야만 동작): 파일에 싱크 가사가 없는 곡의 가사를 LRCLIB에서 찾음. 보내는 것은 곡의 제목 · 아티스트 · 앨범 · 길이뿐.
- 출력 품질 옵션(선택): 곡의 샘플레이트에 장치를 맞춰 리샘플링 없이 출력, 독점 출력, 메모리 재생. 정수 포맷 장치에는 필요한 때에만 디더를 넣습니다.
- 리샘플링이 필요할 때는 20kHz까지 평탄하게(±0.001dB, 측정값은 docs/ARCHITECTURE.md) 변환하고, 최대 음량을 넘는 피크는 잘라 내지 않고 그 순간만 눌러 줍니다(피크 보호).
- 곡·앨범을 플레이리스트와 대기열로 끌어다 놓기, 키보드만으로 하는 선택·재생·정리, 글자를 쳐서 곡 찾기.
- 미디어 키와 시스템 "지금 재생 중", Finder에서 열기, 한국어/영어, 밝은/어두운 화면.
- 계정·서버·광고·사용량 수집이 없습니다. 음악 파일은 읽기만 하고 바꾸지 않습니다.

## 요구 환경

| | |
|---|---|
| OS | macOS 14 이상으로 선언. **실제 검증은 macOS 27.2.1에서만** 했습니다. |
| CPU | Apple Silicon(arm64). Intel Mac은 검증하지 않아 지원하지 않습니다. |
| 빌드 | Rust 1.99.1 (`rust-toolchain.toml`), Xcode 명령줄 도구 |

## 실행

```bash
scripts/package.sh
```

```bash
open dist/OnEum.app
```

설치용 디스크 이미지는 `scripts/package.sh --dmg`로 만듭니다(`dist/OnEum-<버전>-arm64.dmg`).
Developer ID로 서명·공증하지 않은 빌드이므로 다른 Mac에서는 Gatekeeper 경고가 나옵니다.
자세한 내용은 [docs/RELEASE.md](docs/RELEASE.md).

## Windows
```bash
scripts/package-windows.sh
```
Mac에서 크로스 컴파일해 `dist/OnEum-<버전>-windows-x86_64.zip`(압축을 풀어 바로 실행)과 설치 프로그램 `dist/OnEum-<버전>-windows-x86_64-setup.exe`를 만듭니다
(`rustup target add x86_64-pc-windows-gnu`, `brew install mingw-w64` 필요. 설치 프로그램은 `brew install makensis`가 있을 때 만들어집니다).
설치 프로그램은 관리자 권한 없이 사용자 계정에 설치하고, 시작 메뉴 바로 가기와 "설치된 앱"의 제거 항목을 만듭니다. `OnEum.exe`에는 아이콘과 버전 정보가 들어 있습니다.
**Windows판은 컴파일과 링크까지만 확인했고 실제 Windows에서 실행해 보지 못했습니다. 설치 프로그램도 만들어지는 것까지만 확인했습니다.** (CI가 Windows 러너에서 빌드·테스트하고 띄워 보게 되어 있지만 아직 돌아간 적이 없습니다.)
Windows에서 다른 점(창 안 메뉴 막대, Ctrl 단축키, 지원하지 않는 기능)은 [Windows 사용 안내](docs/USER_GUIDE_WINDOWS.md),
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)의 "플랫폼", [docs/TESTING.md](docs/TESTING.md)의 "Windows"에 있습니다.

## iPhone
```bash
scripts/ios-sim.sh
```
iOS 시뮬레이터에 설치해 실행합니다(`rustup target add aarch64-apple-ios aarch64-apple-ios-sim`, Xcode 필요). 실제 iPhone에는 아래로 설치합니다.
```bash
scripts/ios-device.sh
```
iPhone을 연결해 잠금을 풀고 이 Mac을 신뢰하게 한 뒤 실행합니다. Xcode에 Apple ID가 등록되어 있어야 하고, iPhone의 개발자 모드가 켜져 있어야 합니다.
Xcode에서 `ios/OnEum.xcodeproj`를 열어 팀을 고르고 실행해도 됩니다.
서명할 팀은 키체인의 개발 인증서에서 찾습니다. 다른 팀(예: 무료 개인 팀)으로 서명하려면 `ONEUM_IOS_TEAM`에 팀 ID를, 그 팀에서 쓸 수 있는 번들 ID를 `ONEUM_IOS_BUNDLE_ID`에 줍니다.

iPhone에는 폰 전용 화면이 있습니다: 보관함 · 검색 · 플레이리스트 · 설정 탭, 표지 · 가사 · VU 미터와 스펙트럼 · 대기열을 보여 주는 플레이어, 고정, 정렬, 여러 곡 한꺼번에 고르기,
플레이리스트와 대기열의 순서를 끌어서 바꾸기, 스마트 플레이리스트, 크레딧과 신호 경로까지 나오는 곡 정보, 파일 검사, 이퀄라이저와 헤드폰 보정, 백그라운드 재생,
목록 안에서 찾기, iCloud Drive나 꽂은 저장 장치의 폴더를 복사 없이 연결하기, 곡의 샘플레이트로 출력(iPhone에 요청), Apple Music 보관함 가져오기, 가로 화면.
없는 것: 출력 장치 선택과 독점 출력(iPhone이 정함), 메뉴 막대와 키보드 단축키, 목록의 열 고르기, iPad용 화면.

**iPhone판은 iOS 27.0 시뮬레이터에서 확인했습니다. 실제 iPhone(iOS 27.2.1)에는 설치해서 시작되고, 그려지고, 업데이트 뒤에도 보관함이 그대로인 것까지 확인했습니다. 기기에서 화면이 어떻게 보이고 움직이는지는 저희가 확인하지 못했습니다.**
화면과 사용법은 [docs/USER_GUIDE_IOS.md](docs/USER_GUIDE_IOS.md), 다른 점과 확인 범위는 [docs/adr/0002-iphone.md](docs/adr/0002-iphone.md)와 [docs/TESTING.md](docs/TESTING.md)의 "iPhone"에 있습니다.

## Android
```bash
scripts/package-android.sh
```
앱(Rust로 만든 공유 라이브러리)과 얇은 Java 껍데기(`android/`)를 APK로 묶어 `dist/OnEum-<버전>-android-arm64.apk`를 만듭니다. Android SDK와 NDK, `rustup target add aarch64-linux-android`, JDK 17 또는 21이 필요합니다.
APK가 Android의 설치 파일입니다. 휴대폰으로 옮겨(케이블, 클라우드 등) 파일 앱에서 열면 설치됩니다. 처음에는 그 앱에 "출처를 알 수 없는 앱 설치"를 허용해야 하고, Google Play Protect가 처음 보는 앱이라며 검사를 권하면 "자세히 → 검사 없이 설치"를 고를 수 있습니다. 64비트 ARM, Android 11 이상용이며 이 컴퓨터의 디버그 키로 서명됩니다(내 휴대폰용, 배포용 아님).
케이블로 연결된 기기나 에뮬레이터에는 아래로 바로 설치해 실행합니다(USB 디버깅을 켠 기기).
```bash
scripts/android-device.sh
```
Android Studio에서 `android/` 폴더를 열어도 됩니다(앱의 라이브러리는 위 스크립트가 먼저 만들어 둡니다).

iPhone과 같은 폰 화면을 씁니다. 처음 열 때 시스템이 묻는 "음악 및 오디오" 접근을 허용하면 휴대폰의 Music 폴더를 그 자리에서 읽고, 다른 폴더(SD 카드 포함)를 추가할 수 있습니다. WavPack · Monkey's Audio · DSD 파일과 커버 그림 · 가사 · CUE 파일은 설정에서 "모든 파일 접근"을 더 허용하면 읽습니다. TalkBack으로 쓸 수 있습니다(항목 사이를 밀어서 옮기기, 두 번 눌러 실행).
화면을 꺼도 재생이 이어지고(알림과 잠금 화면의 재생 조작), 전화가 오면 멈췄다가 이어 재생합니다. 시스템의 뒤로 가기가 통합니다.
꽂거나 블루투스로 연결한 키보드의 단축키(Space, Ctrl+→ …), 폴더를 추가하지 않고 음악 파일만 열기, 곡이 있는 폴더를 파일 앱에서 보기, 로그 저장이 됩니다.
없는 것: 화면 낭독기(TalkBack) 지원, 출력 장치 선택과 USB DAC의 샘플레이트 맞춤, 독점 출력, 태블릿용 화면.

**Android판은 Android 16 에뮬레이터에서 확인했습니다. 실제 Android 기기에는 아직 설치해 보지 못했습니다.**
사용법은 [docs/USER_GUIDE_ANDROID.md](docs/USER_GUIDE_ANDROID.md), 계획과 진행은 [docs/ANDROID_PLAN.md](docs/ANDROID_PLAN.md), 확인 범위는 [docs/TESTING.md](docs/TESTING.md)의 "Android"에 있습니다.

## 개발

```bash
scripts/check.sh
```

CI가 하는 검사를 그대로 돌립니다: 포맷, 린트(macOS, 그리고 대상이 설치되어 있으면 Windows, iPhone, Android), 테스트. 하나씩 돌리려면:

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

- 테스트는 오디오 장치 없이 돌아갑니다(오프라인 렌더로 샘플 단위 검증).
- 실제 앱을 임시 프로필로 띄워 스크립트로 조작하며 확인합니다: `scripts/dev/smoke.sh`(macOS), `scripts/dev/smoke-ios.sh`(iOS 시뮬레이터). 방법은 [docs/TESTING.md](docs/TESTING.md#앱-구동-검증).

## 문서

| 문서 | 내용 |
|---|---|
| [docs/USER_GUIDE.md](docs/USER_GUIDE.md) · [docs/USER_GUIDE.en.md](docs/USER_GUIDE.en.md) | macOS 사용 안내, 한국어와 영어(앱의 도움말 메뉴에서 보는 것과 같은 글) |
| [docs/USER_GUIDE_WINDOWS.md](docs/USER_GUIDE_WINDOWS.md) · [docs/USER_GUIDE_WINDOWS.en.md](docs/USER_GUIDE_WINDOWS.en.md) | Windows 사용 안내 |
| [docs/USER_GUIDE_IOS.md](docs/USER_GUIDE_IOS.md) · [docs/USER_GUIDE_IOS.en.md](docs/USER_GUIDE_IOS.en.md) | iPhone 사용 안내 |
| [docs/USER_GUIDE_ANDROID.md](docs/USER_GUIDE_ANDROID.md) · [docs/USER_GUIDE_ANDROID.en.md](docs/USER_GUIDE_ANDROID.en.md) | Android 사용 안내 |
| [CHANGELOG.md](CHANGELOG.md) | 바뀐 것 |
| [docs/PRD.md](docs/PRD.md) | 사용자, 범위, 완료 조건 |
| [docs/UX.md](docs/UX.md) | 화면, 조작, 재생 동작 정의 |
| [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) | 모듈, 스레드, 오디오 경로, 저장 |
| [docs/TESTING.md](docs/TESTING.md) | 테스트 방법, 포맷·gapless 지원표, 실측 결과, 검증 대기 항목 |
| [docs/RELEASE.md](docs/RELEASE.md) | 패키징, 서명, 공증, 배포 |
| [docs/PLAN.md](docs/PLAN.md) · [docs/STATUS.md](docs/STATUS.md) | 단계와 현재 상태 |
| [docs/adr/0001-gui-stack.md](docs/adr/0001-gui-stack.md) · [docs/adr/0002-iphone.md](docs/adr/0002-iphone.md) | 결정 기록: GUI 스택, iPhone판 |
| [docs/IOS_PLAN.md](docs/IOS_PLAN.md) | iPhone판 계획과 진행 결과 |
| [docs/ANDROID_PLAN.md](docs/ANDROID_PLAN.md) | Android판 계획과 진행 |
| [site/](site/) | 소개 홈페이지(GitHub Pages로 게시, docs/RELEASE.md "홈페이지") |

## 라이선스

[0BSD](LICENSE)(BSD Zero Clause): 조건 없이 누구나 어떤 목적으로든 쓰고, 고치고, 다시 배포하고, 팔 수 있습니다. 출처를 밝히지 않아도 됩니다(보증은 없습니다). OnEum이 쓰는 오픈 소스와 데이터는 각자의 라이선스를 따르며, 그것들이 든 설치 파일을 다시 배포할 때는 그 고지를 함께 두어야 합니다. 사용한 오픈 소스와 데이터(기본 헤드폰 보정 프로필은 AutoEq의 값): [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
