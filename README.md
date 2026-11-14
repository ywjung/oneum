# OnEum

폴더만 추가하면 바로 듣는 오프라인 로컬 음악 플레이어. Rust로 작성했고 macOS가 주 대상이며 Windows용으로도 빌드됩니다.
이름은 우리말 **온음**(온전한 소리, 그리고 음악의 온음)에서 왔습니다.

- 음악 폴더를 추가하면 스캔하면서 곧바로 재생할 수 있습니다.
- 5만 곡 라이브러리에서도 검색·정렬·스크롤이 즉시 반응합니다(한글 부분 검색, 초성 검색, `artist:`·`genre:`·`year:1990-1999`·`plays:>=5`·`added:30d` 같은 필드 지정).
- FLAC·WAV·AIFF·ALAC·MP3·AAC·DSD는 곡 사이 끊김 없이(gapless) 이어집니다. 모노와 5.1·7.1 파일은 스테레오로 섞어 재생합니다.
- DSD(.dsf, .dff, DSD64~512)는 PCM(88.2/96kHz)으로 바꿔 재생하고, Matroska·WebM 오디오(.mka, .webm)와 ADPCM WAV도 재생합니다.
- 앨범 전체가 파일 하나인 경우 옆의 CUE 시트대로 곡을 나눠 보여 주고, 곡 사이를 끊김 없이 잇습니다.
- 대기열, 플레이리스트(M3U8 가져오기/내보내기), 스마트 플레이리스트(저장한 검색), 즐겨찾기, 최근 재생, 셔플·반복, 현재 곡이 끝나면 정지, 잠자기 타이머.
- 앨범·아티스트·장르·폴더로 둘러보기, 사이드바 고정, 곡 목록에 연도·장르·재생 횟수·추가·마지막 재생·포맷 열을 골라서 표시.
- 파일 검사: 손상(FLAC은 파일의 MD5와 대조), 다이내믹 레인지(DR), 트루 피크. 통계(기간별 들은 시간, 많이 들은 아티스트·앨범·곡·장르). Apple Music 보관함에서 재생 기록·플레이리스트 옮겨 오기.
- 음량 보정(ReplayGain): 파일의 태그를 쓰고, 태그가 없는 곡은 앱이 음량을 재서(EBU R 128) 보정합니다. 음악 파일은 바꾸지 않습니다.
- 미터 덱: 좌·우 아날로그 VU 미터(규격대로 움직이는 바늘과 PEAK 램프)와 그 사이의 36대역 스펙트럼 표시, 소스·출력 경로 표시.
- 10대역 이퀄라이저(기본 프리셋 12개 + 내 프리셋 저장), 헤드폰 보정(널리 쓰이는 헤드폰 34종의 기본 프로필 + AutoEq의 `ParametricEQ.txt` 같은 보정값 파일 불러오기), 헤드폰 크로스피드, 그리고 파일에서 장치까지 앱이 하는 일을 순서대로 보여 주는 신호 경로.
- 다시 듣기(최근 추가 · 많이 들은 곡 · 오래 안 들은 곡), 가사 표시(태그·`.lrc`, 시간 표시가 있으면 싱크), 태그에 든 크레딧·작품 정보, 장르 보기, 같은 곡의 다른 버전, 라디오(대기열이 끝나면 라이브러리에서 어울리는 곡을 이어 재생).
- 사용 안내를 앱 안에서 봅니다(도움말 → OnEum 사용 안내). 언어를 English로 두면 안내도 영어로 나옵니다.
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
Mac에서 크로스 컴파일해 `dist/OnEum-<버전>-windows-x86_64.zip`을 만듭니다(`rustup target add x86_64-pc-windows-gnu`, `brew install mingw-w64` 필요).
**Windows판은 컴파일과 링크까지만 확인했고 실제 Windows에서 실행해 보지 못했습니다.** Windows에서 다른 점(창 안 메뉴 막대, Ctrl 단축키, 지원하지 않는 기능)은
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)의 "플랫폼"과 [docs/TESTING.md](docs/TESTING.md)의 "Windows"에 있습니다.

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
| [docs/USER_GUIDE.md](docs/USER_GUIDE.md) · [docs/USER_GUIDE.en.md](docs/USER_GUIDE.en.md) | 사용 안내, 한국어와 영어(앱의 도움말 메뉴에서 보는 것과 같은 글) |
| [docs/PRD.md](docs/PRD.md) | 사용자, 범위, 완료 조건 |
| [docs/UX.md](docs/UX.md) | 화면, 조작, 재생 동작 정의 |
| [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) | 모듈, 스레드, 오디오 경로, 저장 |
| [docs/TESTING.md](docs/TESTING.md) | 테스트 방법, 포맷·gapless 지원표, 실측 결과, 검증 대기 항목 |
| [docs/RELEASE.md](docs/RELEASE.md) | 패키징, 서명, 공증, 배포 |
| [docs/PLAN.md](docs/PLAN.md) · [docs/STATUS.md](docs/STATUS.md) | 단계와 현재 상태 |
| [docs/adr/0001-gui-stack.md](docs/adr/0001-gui-stack.md) | GUI 스택 결정 |

## 라이선스

MIT ([LICENSE](LICENSE)). 사용한 오픈 소스와 데이터(기본 헤드폰 보정 프로필은 AutoEq의 값): [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
