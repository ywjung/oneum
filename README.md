# OnEum

폴더만 추가하면 바로 듣는, macOS용 오프라인 로컬 음악 플레이어. Rust로 작성했습니다.

- 음악 폴더를 추가하면 스캔하면서 곧바로 재생할 수 있습니다.
- 5만 곡 라이브러리에서도 검색·정렬·스크롤이 즉시 반응합니다(한글 부분 검색, 초성 검색).
- FLAC·WAV·AIFF·ALAC·MP3는 곡 사이 끊김 없이(gapless) 이어집니다.
- 대기열, 플레이리스트(M3U8 가져오기/내보내기), 즐겨찾기, 최근 재생, 셔플·반복, ReplayGain.
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

## 개발

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
- 실제 앱을 임시 프로필로 띄워 스크립트로 조작하는 방법: [docs/TESTING.md](docs/TESTING.md#앱-구동-검증).

## 문서

| 문서 | 내용 |
|---|---|
| [docs/USER_GUIDE.md](docs/USER_GUIDE.md) | 사용 안내 |
| [docs/PRD.md](docs/PRD.md) | 사용자, 범위, 완료 조건 |
| [docs/UX.md](docs/UX.md) | 화면, 조작, 재생 동작 정의 |
| [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) | 모듈, 스레드, 오디오 경로, 저장 |
| [docs/TESTING.md](docs/TESTING.md) | 테스트 방법, 포맷·gapless 지원표, 실측 결과, 검증 대기 항목 |
| [docs/RELEASE.md](docs/RELEASE.md) | 패키징, 서명, 공증, 배포 |
| [docs/PLAN.md](docs/PLAN.md) · [docs/STATUS.md](docs/STATUS.md) | 단계와 현재 상태 |
| [docs/adr/0001-gui-stack.md](docs/adr/0001-gui-stack.md) | GUI 스택 결정 |

## 라이선스

MIT ([LICENSE](LICENSE)). 사용한 오픈 소스: [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
