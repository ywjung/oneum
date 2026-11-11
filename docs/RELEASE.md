# 릴리스 절차

기본 배포 형태는 직접 내려받는 `.dmg`(안에 `OnEum.app`)입니다. App Store·자동 업데이트는 범위 밖입니다.

## 현재 상태 (0.1.0)
| 단계 | 상태 |
|---|---|
| 재현 가능한 빌드·번들·DMG | 완료 (`scripts/package.sh --dmg`) |
| ad-hoc 서명, `codesign --verify` | 완료 |
| 별도 위치에 설치해 실행 | 완료 (docs/TESTING.md "패키지 검증") |
| Developer ID 서명 + Hardened Runtime | **검증 대기** — 인증서 없음 |
| 공증(notarytool), stapling, `spctl` 평가 | **검증 대기** — Apple 계정 자격 증명 없음 |
| 라이선스 전문 묶음 | **대기** — 현재는 의존성 목록만 포함 |

서명·공증되지 않은 빌드를 정식 배포물로 취급하지 마세요. 다른 Mac에서는 Gatekeeper가 실행을 막습니다.

## 1. 준비
- `Cargo.toml`의 `version`을 올리고 `CHANGELOG.md`를 갱신합니다. 빌드 번호는 git 커밋 수에서 자동으로 정해집니다.
- 의존성이 바뀌었으면 `python3 scripts/gen-notices.py`로 `THIRD_PARTY_NOTICES.md`를 다시 만듭니다.
- 아이콘을 바꿨으면 `scripts/gen-icon.sh`.

## 2. 검사
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
- `dist/OnEum.app` — 번들(`Info.plist`, 아이콘, 사용 안내, 고지 포함)
- `dist/OnEum-<버전>-arm64.dmg` — 앱과 `/Applications` 바로가기
- `dist/SHA256SUMS.txt`

아키텍처: Apple Silicon(arm64) 전용입니다. Intel/유니버설 빌드는 검증 장비가 없어 만들지 않습니다
(`rustup target add x86_64-apple-darwin` 후 `lipo`로 합칠 수는 있으나, 검증 전에는 지원한다고 표기하지 않습니다).

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
- 공개 배포 전에 라이선스 전문 묶음을 추가합니다. 권장: `cargo install cargo-about` 후 `cargo about generate`로
  각 crate의 라이선스 본문을 모아 번들 `Resources`에 넣습니다(MIT·Apache-2.0·MPL-2.0 등의 고지 의무).

## 샌드박스와 권한
- 직접 배포이므로 **App Sandbox는 쓰지 않습니다.** 사용자가 고른 폴더를 일반 파일 권한으로 읽습니다.
  (샌드박스를 도입하려면 security-scoped bookmark의 저장·복원·해제를 함께 구현해야 합니다.)
- Hardened Runtime은 Developer ID 서명 시 켭니다(샌드박스와 별개).
- 마이크, 카메라, 전체 디스크 접근, 네트워크 권한을 요구하지 않습니다. 앱은 네트워크에 접속하지 않습니다.
