# 릴리스 절차

기본 배포 형태는 직접 내려받는 `.dmg`(안에 `OnEum.app`)입니다. Mac App Store·자동 업데이트는 범위 밖입니다. iPhone 앱을 App Store에 내는 절차는 [APP_STORE.md](APP_STORE.md)에 있습니다.

## 현재 상태 (0.1.0)
| 단계 | 상태 |
|---|---|
| 재현 가능한 빌드·번들·DMG | 완료 (`scripts/package.sh --dmg`) |
| ad-hoc 서명(하드닝 런타임 포함), `codesign --verify` | 완료 |
| 별도 위치에 설치해 실행 | 완료 (docs/TESTING.md "패키지 검증") |
| Developer ID 서명 + Hardened Runtime | 완료(2026-11-23) — Xcode에 로그인된 계정으로(`ONEUM_MAC_TEAM`, 4절 "가"). 키체인의 인증서로 하는 길(4절 "나")은 **검증 대기** |
| 공증, 티켓 붙이기, `spctl` 평가 | 완료(2026-11-23) — `scripts/package.sh --dmg --notarize`(5절 "가"): 앱이 `Notarized Developer ID`로 받아들여짐. 디스크 이미지 자체는 서명 없음. notarytool로 하는 길(5절 "나")과 **다른 Mac에서 내려받아 여는 것은 검증 대기** |
| 라이선스 전문 묶음 | 완료 — `THIRD_PARTY_LICENSES.txt`(`scripts/gen-licenses.py`, 패키징 때 자동 생성) |
| 검증 채널 제외 | 완료 — 배포 빌드에는 `automation` 기능이 없음 |
| Windows 실제 실행 확인 | **검증 대기** — 실제 Windows 필요 |
| Windows 코드 서명 | 서명하는 길과 검사는 준비됨(2026-11-23, `scripts/windows-sign.sh`, 시험용 인증서로 확인). **인증서가 없어 배포물은 서명되지 않음**. 무료 서명(SignPath Foundation) 신청 준비: 정책 페이지(게시됨)와 GitHub 러너 빌드 워크플로(2026-11-23 세 번째 실행에서 통과). **2026-11-24에 신청함, 심사 대기** — 아래 "Windows 코드 서명" |
| iPhone: 시뮬레이터 실행, 기기용 빌드 | 완료 — `scripts/dev/smoke-ios.sh`(69항목), Xcode 프로젝트로 기기용 릴리스 빌드 |
| 소스 공개 | 공개됨(2026-11-22) — https://github.com/ywjung/oneum (`github` 원격, `main`만 올림). 올리기 전에 추적 파일과 커밋에 비밀 값 · 개인 주소 · 홈 경로 · 서명 팀 ID · 기기 식별자가 없음을 확인했다. 커밋의 작성자는 모두 한 이름과 한 e-mail 주소다. CI는 여섯 번의 실행에 걸쳐 고친 뒤 네 작업(macOS · Windows · Android · 의존성 권고) 모두 통과(docs/TESTING.md "GitHub에 올린 뒤의 CI"). 올라간 커밋은 다시 쓰지 않는다 |
| 첫 릴리스 | **v0.1.0 시험판**(pre-release, 2026-11-22, 빌드 149): https://github.com/ywjung/oneum/releases/tag/v0.1.0 — dmg, Windows 설치 프로그램과 zip, Android APK, 체크섬 세 파일. 내려받은 파일이 만든 파일과 같음을 확인. 처음에는 서명 · 공증이 없었고, **2026-11-23에 dmg를 Developer ID 서명 · 공증한 빌드 153으로 바꿨다**(내려받아 같은 파일임과 검사 8항목 통과를 확인, 릴리스 노트도 고침). Windows는 서명이 없고 Android는 개발용 키라 시험판 표시는 그대로다(아래 "공개") |
| Android: 에뮬레이터 실행, 릴리스 APK | 완료(2026-11-15) — `scripts/dev/smoke-android.sh`(44항목), 설치 파일 `scripts/package-android.sh` → `dist/OnEum-<버전>-android-arm64.apk`(에뮬레이터의 파일 앱에서 열어 설치·실행 확인). 디버그 키 서명이며 배포용 키 서명과 Play 스토어는 범위 밖. 실제 기기 설치는 **검증 대기** |
| iPhone: 실제 기기 설치·실행 | 설치·실행 확인(2026-11-14, 개인 팀 서명, `scripts/ios-device.sh`). 기기에서의 화면·재생 확인은 **검증 대기** |
| iPhone: App Store용 아카이브 | 서명된 아카이브 · `.ipa` · 검사 · 업로드까지 완료(2026-11-23, 유료 팀, 0.1.0 빌드 147이 App Store Connect에 올라감). Apple의 처리가 끝나 사용자가 TestFlight로 본인 iPhone에 설치했고 위젯에 곡이 나옴. 실제 iPhone에서 볼 나머지 항목, 스토어 정보와 심사는 **남음**([APP_STORE.md](APP_STORE.md)) |

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
- 서명에 쓸 것을 주지 않으면 실행 파일도 설치 프로그램도 서명하지 않으므로 Windows SmartScreen 경고가 나옵니다(아래 "Windows 코드 서명").
- 스크립트는 실행 파일이 컴파일러 런타임 DLL을 필요로 하지 않는지와 범용 C 런타임(UCRT)을 쓰는지만 검사합니다. **Windows에서 실행해 보는 단계는 없습니다** — 배포 전에 실제 Windows에서
  docs/TESTING.md "Windows"의 항목을 확인해야 합니다.
- **설치 프로그램은 만들어지는 것까지만 확인했습니다.** Windows에서 설치·실행·제거해 본 적이 없습니다(docs/TESTING.md "Windows").

### Windows 코드 서명
**상태(2026-11-23)**: 서명하는 길과 검사는 스크립트에 들어 있고 시험용 인증서로 확인했습니다. **진짜 인증서는 없습니다.** 인증서를 구하는 일(신청 · 결제 · 본인 확인)은 배포하는 사람이 직접 해야 합니다. `ONEUM_WIN_SIGN`을 주지 않은 `scripts/package-windows.sh`는 전과 같은 서명 없는 파일을 만듭니다.

서명해도 SmartScreen 경고가 바로 사라지지는 않습니다. Microsoft의 문서(code-signing-options, 2026-11-10 갱신)에 따르면 평판은 내려받기가 쌓이면서 생기고, EV 인증서가 주던 "즉시 통과"는 2024년에 없어졌으며, OV 인증서와 Azure Artifact Signing은 SmartScreen에서 같은 대접을 받습니다. 서명이 주는 것은 게시자 이름이 표시되는 것, 파일이 바뀌지 않았다는 확인, 그리고 같은 인증서로 계속 서명하면 평판이 다음 버전으로 이어진다는 것입니다.

#### 인증서를 구하는 방법 (2026-11-23에 각 공식 페이지에서 확인한 것)
| 방법 | 비용 | 한국의 개인 | 게시자로 표시되는 이름 | 걸리는 점 |
|---|---|---|---|---|
| **Certum Open Source Code Signing** (클라우드형, SimplySign) | €49 (카드+리더 세트 €69, 가진 카드에 넣는 코드만 €25) | 개인에게만 발급. 국가 제한은 페이지에 없음 | "Open Source Developer, 〈본인 실명〉" | 본인 확인(자동 확인, 또는 신분증을 든 사진, 또는 공증), 본인 이름의 공과금 고지서, 공개된 프로젝트 주소(신청자와 프로젝트의 관계가 드러나야 함). **상업적으로 배포하는 소프트웨어에 쓰면 폐기**. 클라우드형은 한 달 5,000번까지 |
| **SignPath Foundation** (오픈 소스 무료 서명) | 무료 | 조건에 국가 제한이 적혀 있지 않음 | "SignPath Foundation" | OSI 승인 라이선스, 독점 구성 요소 없음, 유지되고 있고 이미 릴리스된 프로젝트, 팀 전원 2단계 인증, 홈페이지에 "Code signing policy" 페이지(정해진 문구 · 역할 · 개인정보 문구), 릴리스마다 사람이 승인. **빌드는 GitHub Actions의 GitHub 러너에서 해야 함** — 이 Mac에서 만든 파일은 서명해 주지 않음. 프로젝트의 평판을 심사함(기준은 적혀 있지 않음) |
| 일반 OV 인증서 (CA나 재판매처) | 재판매처 기준 한 해 $200~$330쯤에 USB 토큰 값, CA 직판은 $129~$529 (검색으로 본 값, 사기 전에 확인) | 개인 검증으로 가능 | 본인 실명 | 키는 USB 토큰이나 클라우드 HSM에만 둘 수 있음(2023-06부터). 신분 서류 |
| Azure Artifact Signing (전 Trusted Signing) | 한 달 약 $9.99 (5,000번), 계정을 만든 때부터 과금 | **개인은 미국 · 캐나다만** — 안 됨. 조직은 한국 포함(quickstart 문서; 같은 날 본 다른 Microsoft 문서는 미국 · 캐나다 · EU · 영국만 적음) | 검증된 조직 이름 | 사업자로 신청할 때만 해당. 종량제 Azure 구독 필요 |
| Microsoft Store 배포 | 개인 개발자 등록 무료(2026) | "약 200개 시장"(한국이 적혀 있지는 않음) | — | 서명이 아니라 다른 배포 길. Store가 패키지에 서명해 주는지, 지금의 설치 프로그램을 그대로 올릴 수 있는지는 **확인하지 않음** |

확인하지 못한 것: Certum 인증서의 유효 기간(검색 결과의 요약에는 1년, 상점 페이지에는 없음)과 한국에서의 발급 사례, SimplySign Desktop이 macOS에서 내어 주는 PKCS #11 라이브러리의 위치, SignPath Foundation의 신청 양식 · 대기 기간(신청 페이지가 읽히지 않음), EV 인증서의 지금 조건(개인에게는 발급되지 않는 것으로 알고 있으나 오늘 확인하지 않음).

#### SignPath Foundation 신청 준비 (2026-11-23)
사용자의 결정: 인증서를 사지 않고, 무료인 SignPath Foundation에 신청하는 쪽으로 준비합니다. 받아들일지는 재단이 정하고(이의 절차가 없다고 조건에 적혀 있음), 내려받아 실행하는 프로그램에는 "검증할 수 있는 평판"을 요구합니다. 공개한 지 얼마 안 된 시험판이고 Windows판을 실제 PC에서 실행해 본 적이 없으므로, **실행 확인을 먼저 하고 신청하는 편이 낫습니다.**

준비해 둔 것:
- **코드 서명 정책 페이지**: `site/code-signing.html`(한국어), `site/en/code-signing.html`(영어). 재단이 요구하는 것(용어 "Code signing policy", 정해진 문구, 역할과 사람, 개인정보 문구)을 담았고, **아직 신청 전이며 지금의 파일은 서명되지 않았다**고 적었습니다. 두 홈페이지의 "받기"와 아래쪽, 두 README에서 연결됩니다. 받아들여지면 첫 문단과 "서명하는 곳"의 조건문을 고칩니다.
- **GitHub 러너에서 패키지를 만드는 워크플로**: `.github/workflows/windows-package.yml`. 버전 태그(`v*`)를 올리거나 Actions 탭에서 직접 실행하면 GitHub의 macOS 러너에서 Homebrew의 mingw-w64와 NSIS로(릴리스를 만들어 온 Mac과 같은 도구) `scripts/package-windows.sh`를 돌려 zip과 설치 프로그램을 실행의 아티팩트로 남깁니다(서명 없음). 재단은 서명할 파일이 GitHub이 운영하는 러너의 워크플로에서 만들어져 아티팩트로 올라가 있을 것을 요구합니다 — 이 Mac에서 만든 파일은 서명해 주지 않습니다.

직접 하셔야 하는 일(순서대로):
1. 실제 Windows PC에서 지금의 설치 프로그램과 zip을 실행해 봅니다(docs/TESTING.md "Windows").
2. GitHub 계정에 2단계 인증을 켭니다(조건: 팀 전원이 저장소와 SignPath 계정에 2단계 인증).
3. (끝남, 2026-11-23) `main`에 합쳐 올려 정책 페이지가 게시되었고(https://ywjung.github.io/oneum/code-signing.html), "Windows package"가 GitHub에서 통과했습니다.
4. (끝남, 2026-11-24) https://signpath.org/apply 의 양식으로 신청했습니다. 접수 화면("Form submitted — Thank you, we'll be in touch soon.")까지 확인했고, 결과는 신청한 사람의 e-mail로 옵니다. 조건은 https://signpath.org/terms.
5. 받아들여지면 SignPath의 계정 · 프로젝트 · 서명 정책을 만들고, API 토큰을 저장소의 secret(`SIGNPATH_API_TOKEN`)으로 넣고, 릴리스 때마다 서명 요청을 승인합니다.

신청 양식이 물은 것(2026-11-24): 프로젝트 이름, 저장소 · 홈페이지 · 내려받는 페이지 · 개인정보 처리방침의 주소, 한 줄 소개, 분류(Audio), 설명, **평판**(널리 쓰이거나 신뢰받는다는 근거 — 필수), 유지하는 주체(개인), 빌드 시스템(GitHub Actions), SignPath 계정이 될 이름과 e-mail, 재단을 알게 된 경로, 행동 강령과 개인정보 처리에 대한 동의. 내려받는 페이지는 "SignPath Foundation을 코드 서명에 쓴다고 언급해야 한다"고 되어 있어, 두 홈페이지의 "받기"에 신청해 심사를 기다린다는 문장을 넣었습니다. 평판 칸에는 사실대로 적었습니다: 2026-11-22에 공개한 새 프로젝트이고 보도나 내려받기 기록이 없으며, 확인할 수 있는 것은 공개된 소스와 기록, CI, GitHub 러너에서의 빌드, 서명 · 공증된 macOS 릴리스라는 것. **이 때문에 거절될 수 있습니다.** 거절되면 서명 없이 시험판으로 두고, 릴리스 기록이 쌓인 뒤 다시 신청합니다.

받아들여진 뒤에 할 일(아직 만들지 않음 — 조직 ID와 프로젝트 이름이 정해져야 쓸 수 있습니다):
- 워크플로에 서명 단계를 넣습니다: `actions/upload-artifact`(v4 이상)로 올린 아티팩트의 ID를 `signpath/github-action-submit-signing-request@v3`에 넘깁니다(`organization-id`, `project-slug`, `signing-policy-slug`, `github-artifact-id`, `wait-for-completion`, `output-artifact-directory`; 승인을 기다리는 시간의 기본값은 600초).
- 설치 프로그램 안의 `OnEum.exe`도 서명되어야 하므로 요청은 두 번입니다: `OnEum.exe` → 서명된 것을 받아 zip과 설치 프로그램을 만듦 → 설치 프로그램. `scripts/package-windows.sh`가 이미 서명된 `OnEum.exe`를 받아 쓰는 길은 그때 넣습니다. 이 길에서는 제거 프로그램에 서명하지 않습니다(요청이 한 번 더 필요해서).
- 마지막 검사는 지금의 `scripts/windows-sign.sh verify`를 그대로 씁니다(러너에서도 `brew install osslsigncode`).

확인한 것: 두 정책 페이지와 고친 두 홈페이지를 로컬 서버로 열어 넓은 화면(1280px, 밝은 화면)과 좁은 화면(375px, 어두운 화면)에서 넘침 없이 그려지는 것, 페이지가 가리키는 사이트 안의 파일이 모두 있는 것, 다른 서버로 가는 요청이 없는 것, 정해진 두 문구가 글자 그대로 들어 있는 것. 올린 뒤 두 정책 페이지가 공개 주소에서 열리는 것(HTTP 200)과 홈페이지에 그 연결이 있는 것.

워크플로의 실행(2026-11-23, 직접 실행 세 번):
| 실행 | 러너 | 결과 |
|---|---|---|
| 1 | Ubuntu 26.04, `mingw-w64` | **링크 실패**: `_get_timezone`을 찾지 못함. Ubuntu의 `x86_64-w64-mingw32-gcc`는 옛 C 런타임(`msvcrt.dll`)으로 링크하는데, 프로그램(`src/platform/clock.rs`)과 Homebrew의 mingw-w64는 범용 C 런타임(UCRT)을 씀 |
| 2 | Ubuntu 26.04, `gcc-mingw-w64-ucrt64` | **설치 실패**: 그 패키지가 이 Ubuntu에 없음 |
| 3 | macOS(`macos-26-arm64`), Homebrew의 mingw-w64 14.0.0_3 · makensis 3.13 | **통과**(약 9분): zip과 설치 프로그램이 만들어지고, 스크립트의 검사(아이콘, 런타임 DLL, UCRT)와 러너의 경로가 파일에 없는지의 검사를 지나, 아티팩트 `OnEum-windows-x86_64`(약 26MB)로 올라감 |

이 일로 `scripts/package-windows.sh`에 검사 하나를 더했습니다: 만든 프로그램이 UCRT를 쓰지 않으면(`msvcrt.dll`로 링크되었거나 `api-ms-win-crt-*`가 없으면) 실패합니다.

확인하지 못한 것: 러너가 만든 아티팩트를 내려받아 보지 않았고 Windows에서 실행해 보지도 않았습니다. 버전 태그로 시작되는 실행(직접 실행만 해 봄). 심사 기간과 결과, SignPath 쪽 설정 전부.

#### 서명해서 패키징하기 (이 Mac에서, 인증서가 있을 때)
```bash
brew install osslsigncode
```
무엇으로 서명할지는 환경 변수로만 줍니다(저장소에는 인증서 · 키 · PIN · 계정이 없습니다). 변수의 뜻은 `scripts/windows-sign.sh`의 머리말에 있습니다.

- **USB 토큰이나 클라우드 HSM** (`ONEUM_WIN_SIGN=pkcs11`): 키가 든 장치의 PKCS #11 라이브러리로 서명합니다.
  ```bash
  ONEUM_WIN_SIGN=pkcs11 ONEUM_WIN_SIGN_PKCS11_MODULE=<라이브러리 경로> ONEUM_WIN_SIGN_KEY='pkcs11:<키>' ONEUM_WIN_SIGN_CERT=<인증서와 체인.pem> scripts/package-windows.sh
  ```
  PIN을 주지 않으면 파일마다(세 번) 터미널에서 묻습니다. 이 Mac의 osslsigncode(OpenSSL 4)는 PKCS #11을 쓸 때 `pkcs11prov`라는 OpenSSL provider를 찾는데 이 Mac에는 없습니다(없어서 실패하는 것까지 확인). 토큰을 쓰려면 그 provider나, `ONEUM_WIN_SIGN_PROVIDER`로 가리킬 다른 PKCS #11 provider를 설치해야 합니다. **이 길은 토큰이 없어 서명까지 가 보지 못했습니다.**
- **자기 서명 도구가 있는 서비스** (`ONEUM_WIN_SIGN=command`): `ONEUM_WIN_SIGN_COMMAND`에 적은 명령을 파일마다 실행합니다(`"$1"`이 파일). 그 명령이 그 파일에 타임스탬프까지 넣어 서명해야 합니다. 예를 들어 jsign(7.6, Java)은 Azure Artifact Signing · DigiCert ONE · SSL.com eSigner · Certum 카드 같은 것을 직접 다룹니다(jsign으로 서명해 본 적은 없습니다).
  ```bash
  ONEUM_WIN_SIGN=command ONEUM_WIN_SIGN_COMMAND='jsign --storetype <종류> <그 서비스의 옵션> --tsaurl <타임스탬프 서버> "$1"' scripts/package-windows.sh
  ```
- **파일로 된 인증서** (`ONEUM_WIN_SIGN=pkcs12`, `ONEUM_WIN_SIGN_PKCS12`, 암호는 `ONEUM_WIN_SIGN_PASS_FILE`): 공인 CA는 이제 키를 파일로 주지 않으므로 시험용입니다.

그 밖에: 타임스탬프 서버는 `ONEUM_WIN_SIGN_TS`(기본 `http://timestamp.digicert.com`, RFC 3161; CA가 자기 서버를 쓰라고 하면 바꿉니다), 검사가 믿는 루트는 `ONEUM_WIN_SIGN_CAFILE` · `ONEUM_WIN_SIGN_TSA_CAFILE`(기본은 osslsigncode의 목록 `/etc/ssl/cert.pem`; Microsoft의 루트로 끝나는 인증서는 그 목록에 없을 수 있어 그때는 루트 파일을 줍니다), 서명한 인증서의 이름 확인은 `ONEUM_WIN_SIGN_SUBJECT`.

서명을 청하면 스크립트가 하는 일:
1. 빌드하기 전에 필요한 것(도구, 변수, 읽을 수 있는 파일)이 다 있는지 봅니다.
2. `OnEum.exe`에 한 번 서명합니다. zip과 설치 프로그램에 같은 서명된 파일이 들어갑니다.
3. 설치 프로그램을 만드는 중에 제거 프로그램(`Uninstall.exe`)에 서명하고(NSIS의 `!uninstfinalize`), 다 만들어진 설치 프로그램에 서명합니다.
4. `dist`에 놓인 그대로 다시 검사합니다: zip에서 꺼낸 `OnEum.exe`와 설치 프로그램. 서명이 없거나, 파일과 맞지 않거나, 체인이 믿는 루트로 끝나지 않거나, 검증되는 타임스탬프가 없으면 실패합니다(osslsigncode는 타임스탬프가 없어도 성공으로 끝나므로 따로 봅니다).
5. 어디서든 실패하면 `dist`에 이 이름의 파일을 남기지 않습니다(서명 안 된 파일이나 지난 실행의 파일이 섞여 나가지 않게).

이미 만든 파일만 검사할 때:
```bash
scripts/windows-sign.sh verify dist/OnEum-<버전>-windows-x86_64-setup.exe
```

#### 확인한 것 (2026-11-23, osslsigncode 2.16 · OpenSSL 4.1.0 · makensis 3.12)
스크래치 폴더에 만든 시험용 루트와 그 루트가 발급한 코드 서명 인증서(이틀짜리)로 했고, 끝난 뒤 지웠습니다. 어느 신뢰 저장소에도 넣지 않았습니다.
- **서명 없는 빌드가 그대로인지**: 전의 스크립트와 새 스크립트로 각각 패키징해 zip의 내용이 같음(파일 다섯 개 모두). 같은 재료로 전의 `installer.nsi`와 새 것이 만든 설치 프로그램이 바이트 단위로 같음. 두 파일 모두 검사에서 "서명 없음".
- **서명한 패키징**(`pkcs12`, 타임스탬프는 실제 DigiCert 서버): `OnEum.exe`, 제거 프로그램, 설치 프로그램 순으로 서명되고 `dist`의 두 파일이 검사를 통과. 체크섬 파일이 서명된 파일과 맞음. zip에서 달라진 것은 `OnEum.exe`와 `README.txt`의 한 문장뿐. 서명된 실행 파일에 아이콘과 버전 정보가 그대로 있고, 두 파일에 `/Users/`가 0건. 기본 루트 목록으로 검사하면 실패(시험용 인증서이므로 맞는 결과).
- **실패해야 하는 경우**(전체 패키징): 변수가 빠지면 빌드 전에 멈춤. 서명 명령이 아무것도 하지 않으면 `OnEum.exe`에서, 제거 프로그램만 서명하지 않으면 makensis가, 설치 프로그램만 서명하지 않으면 마지막에 실패하고, 세 경우 모두 `dist`가 비워짐. `command`로 세 파일을 모두 서명하면 통과.
- **스크립트 단독**: 틀린 암호, 없는 암호 파일, 암호도 터미널도 없는 경우, 닿지 않는 타임스탬프 서버, 타임스탬프 없이 서명한 파일, 다른 인증서로 서명한 파일(`ONEUM_WIN_SIGN_SUBJECT`), 서명 뒤 한 바이트가 바뀐 파일은 모두 실패. 이미 서명된 파일에 다시 서명하면 서명이 하나로 남음. 이름에 빈칸이 있는 파일도 됨.
- 전체 패키징 한 묶음에서 시험용 서명 명령이 인증서 파일의 암호를 읽지 못해 세 번 실패한 일이 한 번 있었습니다. 같은 순서로 네 번 다시 돌리고 같은 명령을 600번 돌려도 다시 나오지 않아 **원인을 찾지 못했습니다**(실패 때 패키징은 실패로 끝나고 `dist`는 비워졌습니다).

#### 확인하지 못한 것
- **진짜 인증서로 서명하는 것 전부**: 공인 CA의 체인과 폐기 목록 확인, 토큰 · 클라우드(`pkcs11`), jsign 같은 다른 도구.
- **Windows가 이 서명을 받아들이는지**: 실제 Windows에서 아래를 봐야 합니다. 서명된 설치 프로그램이 설치 · 제거되는지도 본 적이 없습니다(서명 없는 것도 마찬가지 — docs/TESTING.md "Windows").
  ```powershell
  Get-AuthenticodeSignature .\OnEum-<버전>-windows-x86_64-setup.exe | Format-List Status, StatusMessage, SignerCertificate, TimeStamperCertificate
  ```
  (`Status`가 `Valid`여야 합니다. Windows SDK가 있으면 `signtool verify /pa /v <파일>`도 같은 것을 봅니다.) 파일의 속성 → 디지털 서명 탭, 그리고 브라우저로 내려받아 실행할 때 SmartScreen이 보이는 게시자 이름도 확인합니다.
- 설치된 뒤의 `Uninstall.exe`의 서명: 서명한 파일이 설치 프로그램에 들어가는 것까지 보았고, 설치되어 나온 파일은 Windows에서 봐야 합니다.

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

## 4. Developer ID 서명
Apple Developer Program 멤버십이 필요합니다. 방법은 둘이고, 하나만 씁니다. 어느 쪽이든 Hardened Runtime이 켜지고 서명에 Apple의 타임스탬프가 들어갑니다.
OnEum은 JIT·외부 라이브러리 로딩·마이크·카메라를 쓰지 않으므로 **entitlements 파일이 필요 없습니다.**

### 가. Xcode에 로그인된 계정으로 (확인된 방법)
필요한 것: Xcode ▸ Settings ▸ Accounts에 로그인된 Apple 계정과 팀 ID(열 글자, developer.apple.com ▸ Membership details). 인증서를 직접 만들거나 키체인에 넣지 않습니다. Xcode가 Apple에 보관하는 Developer ID 인증서로 서명합니다(자동 서명). 암호를 묻지 않고, 계정의 어떤 값도 저장소에 남지 않습니다.
```bash
ONEUM_MAC_TEAM=<팀 ID> scripts/package.sh --dmg
```
- `scripts/package.sh`가 앱을 만든 뒤 `scripts/mac-developer-id.sh`를 부릅니다. 이 스크립트는 앱을 아카이브로 싸서 `xcodebuild -exportArchive`(`method=developer-id`)로 다시 서명하게 하고, 돌아온 앱을 검사한 뒤 `dist/OnEum.app`을 바꿉니다. 검사: 서명이 내용과 맞는지, Developer ID 인증서인지, 지정한 팀인지, Hardened Runtime, 타임스탬프, 검증 채널 없음.
- **계정이 여러 팀에 속해 있으면 본인 팀의 ID를 줍니다.** 지정한 팀이 서명합니다. 다른 조직의 팀을 주면 그 팀의 계정 소유자만 할 수 있는 일이라 Xcode가 거절합니다(Developer ID 인증서는 계정 소유자만 만듭니다).
- **화면 잠금이 풀려 있어야 합니다.** 잠긴 채 돌린 한 번(2026-11-23)은 Xcode가 키체인에서 계정을 읽지 못해 15분쯤 뒤 `No Accounts`로 끝났고, 그 뒤 Xcode의 계정 목록이 비어 **다시 로그인해야 했습니다.** 그래서 스크립트가 먼저 확인하고, 잠겨 있으면 `xcodebuild`를 부르지 않고 끝냅니다. 서명이 끝날 때까지 화면이 잠기지 않게 둡니다.
- 디버그 빌드와 검증 채널이 든 빌드(`ONEUM_AUTOMATION=1`)는 거절합니다.
- 이 방법에서는 **디스크 이미지 자체는 서명되지 않습니다**(서명할 인증서가 이 Mac에 없습니다). 안의 앱이 서명 · 공증되어 있으면 Gatekeeper는 앱을 통과시킵니다.

있는 앱만 검사하려면:
```bash
scripts/mac-developer-id.sh --check dist/OnEum.app --notarized
```

### 나. 키체인의 인증서로 (**검증 대기** — 이 Mac에는 그런 인증서가 없음)
필요한 것: 키체인에 설치된 **Developer ID Application** 인증서.
```bash
security find-identity -v -p codesigning
```
```bash
ONEUM_SIGN_IDENTITY="Developer ID Application: <이름> (<팀 ID>)" scripts/package.sh --dmg
```
스크립트가 `--options runtime`(Hardened Runtime)과 `--timestamp`로 앱과 디스크 이미지를 서명합니다.
```bash
codesign --verify --deep --strict --verbose=2 dist/OnEum.app
```
```bash
codesign -d --entitlements - --verbose=2 dist/OnEum.app
```

## 5. 공증
### 가. Xcode에 로그인된 계정으로 (확인된 방법)
```bash
ONEUM_MAC_TEAM=<팀 ID> scripts/package.sh --dmg --notarize
```
- 서명한 앱을 Xcode가 Apple의 공증 서비스에 보내고(`destination=upload`), 스크립트가 답을 기다렸다가(`xcodebuild -exportNotarizedApp`, 기본 30분까지, `ONEUM_NOTARY_WAIT`로 바꿈) **티켓이 붙은 앱**을 받아 옵니다. 앱 암호나 키체인 프로필이 필요 없습니다.
- 검사가 더해집니다: 티켓이 붙어 있는지(`stapler validate`), Gatekeeper가 공증된 앱으로 받아들이는지(`spctl -a -t exec` → `source=Notarized Developer ID`).
- 디스크 이미지와 `SHA256SUMS.txt`는 그 앱으로 만들어집니다.

### 나. notarytool로 직접 (**검증 대기** — 자격 증명을 저장한 적 없음)
4절 "나"로 서명한 디스크 이미지에 씁니다. 필요한 것: Apple ID, 앱 암호, 팀 ID. 비밀 값은 키체인 프로필에만 저장하고 저장소·로그에 남기지 않습니다.
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
- 판매·공개 전 남은 일: 서명 · 공증한 dmg를 다른 Mac에서 내려받아 여는 확인(v0.1.0 시험판의 dmg는 2026-11-23에 서명 · 공증한 빌드 153으로 바꿈), Windows 실제 실행 확인(docs/TESTING.md "Windows"), 그리고 가능하면 Windows 코드 서명(없으면 SmartScreen 경고).

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
코드 서명 정책(`site/code-signing.html`, `site/en/code-signing.html`)도 여기 있습니다: Windows 파일의 서명 상태가 바뀌면(신청, 승인, 첫 서명된 릴리스) 두 언어의 페이지와 날짜를 함께 고칩니다.
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
