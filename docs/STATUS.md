# 진행 상태

마지막 갱신: 2026-11-11 · 버전 0.1.0

## 한눈에
| 구분 | 상태 |
|---|---|
| v1.0 필수 기능 구현 | 완료 |
| 자동 검증 | 완료 — 181개 테스트, fmt·clippy 통과 |
| macOS 실기기 검증 | 완료한 범위와 대기 항목을 docs/TESTING.md에 구분해 기록 |
| 외부 자격 증명·장비가 없어 검증 대기 | Developer ID 서명·공증·Gatekeeper, VoiceOver 음성, 물리 미디어 키, Bluetooth, 잠자기·깨우기, 실제 드래그 앤 드롭, 파일 패널 안에서의 선택, 실제 음반 라이브러리, macOS 14~26, Intel |

## 단계별
| 단계 | 상태 | 비고 |
|---|---|---|
| 0 조사와 계획 | 완료 | 기술 검증 7종, ADR 0001 |
| 1 최소 수직 구현 | 완료 | 실제 장치 재생, .app |
| 2 라이브러리 | 완료 | 스캔, DB, 검색, 가상화 목록 |
| 3 MVP | 완료 | 대기열, 플레이리스트, 상태 복원, 테마 |
| 4 v1.0 재생 | 완료 | 포맷 7종, gapless, ReplayGain, 셔플·반복, 장치 분리 대응 |
| 5 macOS·UX | 완료(일부 검증 대기) | 메뉴, Now Playing, Finder 열기, 한/영, 접근성 이름 |
| 6 안정화 | 완료(장시간 재생 결과는 TESTING.md) | 5만 곡 성능, 오류 복구 |
| 7 패키징 | 완료(서명·공증 대기) | .app, .dmg, 체크섬, 문서 |

## 남은 작업
1. Developer ID 인증서가 준비되면 docs/RELEASE.md 4~6절을 수행하고 결과를 TESTING.md에 기록.
2. docs/TESTING.md "검증 대기" 표의 수동 항목 확인(VoiceOver, 미디어 키, Bluetooth, 잠자기, 드래그 앤 드롭, 파일 패널).
3. 실제 음반 라이브러리로 스캔·재생을 확인하고 발견되는 태그/코덱 문제를 기록.
4. 공개 배포 전 라이선스 전문 묶음 추가(RELEASE.md 7절).

## 재개 지점
1. 이 파일과 docs/TESTING.md를 읽는다.
2. `cargo test --workspace --locked`로 현재 상태를 확인한다.
3. 앱을 띄워 확인하려면 docs/TESTING.md "앱 구동 검증"의 세 명령을 쓴다(실제 사용자 데이터와 분리된 프로필).

## 구조 요약
- `src/audio` 디코드·리샘플·실시간 렌더러·출력, `src/playback` 대기열·플레이어 스레드,
  `src/library`·`src/storage` 스캔·검색·SQLite, `src/ui` egui 화면, `src/platform/macos` 시스템 연동, `src/app` 연결.
- 결정 근거: docs/ARCHITECTURE.md, docs/adr/0001-gui-stack.md. 동작 정의: docs/UX.md.
