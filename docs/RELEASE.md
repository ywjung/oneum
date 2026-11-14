# 릴리스 절차

기본 배포 형태는 직접 내려받는 `.dmg`(안에 `OnEum.app`)입니다. App Store·자동 업데이트는 범위 밖입니다.

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
| iPhone: 시뮬레이터 실행, 기기용 빌드 | 완료 — `scripts/dev/smoke-ios.sh`(56항목), Xcode 프로젝트로 기기용 릴리스 빌드 |
| 소스 공개 준비 | 준비됨(2026-11-14) — 원격 `github`(https://github.com/ywjung/oneum, 비어 있는 공개 저장소). 추적 파일과 지난 커밋의 내용에 비밀 값·개인 주소·홈 경로·서명 팀 ID·기기 식별자가 없음을 확인, 저장소 크기 약 5MB(가장 큰 파일 1MB), 오픈 소스 고지가 iPhone 빌드까지 포함, CI에 iPhone 린트 추가. 커밋 기록의 작성자는 모두 GitHub의 noreply 주소로 통일해 실제 e-mail과 로컬 계정 이름이 공개되지 않게 했다(이 저장소의 git 설정도 그 주소). 새 클론에서 빌드와 테스트 562개가 통과함을 확인. push와 CI 첫 실행은 **아직 하지 않음** |
| iPhone: 실제 기기 설치·실행 | 설치·실행 확인(2026-11-14, 개인 팀 서명, `scripts/ios-device.sh`). 기기에서의 화면·재생 확인은 **검증 대기**. TestFlight·App Store는 범위 밖 |

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
scripts/package-windows.sh
```
`dist/OnEum-<버전>-windows-x86_64.zip`(OnEum.exe, LICENSE.txt, Windows 빌드 기준 THIRD_PARTY_NOTICES.md, README.txt)과
`dist/SHA256SUMS-windows.txt`가 만들어집니다. 설치 프로그램은 없고, 압축을 푼 폴더에서 바로 실행합니다.
- 서명하지 않으므로 Windows SmartScreen 경고가 나옵니다(코드 서명 인증서가 필요합니다).
- 스크립트는 실행 파일이 컴파일러 런타임 DLL을 필요로 하지 않는지만 검사합니다. **Windows에서 실행해 보는 단계는 없습니다** — 배포 전에 실제 Windows에서
  docs/TESTING.md "Windows"의 항목을 확인해야 합니다.
- 실행 파일 자체의 아이콘(탐색기에 보이는 아이콘)은 넣지 않았습니다. 창과 작업 표시줄 아이콘만 설정됩니다.

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

## 샌드박스와 권한
- 직접 배포이므로 **App Sandbox는 쓰지 않습니다.** 사용자가 고른 폴더를 일반 파일 권한으로 읽습니다.
  (샌드박스를 도입하려면 security-scoped bookmark의 저장·복원·해제를 함께 구현해야 합니다.)
- Hardened Runtime은 Developer ID 서명 시 켭니다(샌드박스와 별개).
- 마이크, 카메라, 전체 디스크 접근, 네트워크 권한을 요구하지 않습니다. 앱은 네트워크에 접속하지 않습니다.
