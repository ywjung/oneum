# Windows: Microsoft Store 배포 준비

작성 2026-11-25 · 대상은 Windows용 OnEum입니다. 내려받는 파일(zip, 설치 프로그램)과 그 서명은 [RELEASE.md](RELEASE.md) "Windows판" · "Windows 코드 서명"에 있습니다.

**왜 이 길인가.** Microsoft Store에 MSIX 패키지로 올리면, 심사를 통과한 패키지를 Store가 Microsoft의 인증서로 서명합니다. 인증서를 살 필요가 없고 개인 개발자 등록도 무료입니다. 다만 **서명되는 것은 Store로 설치하는 패키지뿐**입니다. GitHub 릴리스의 zip과 설치 프로그램은 이 길로는 서명되지 않습니다(그쪽은 SignPath Foundation에 신청해 둔 상태).

Windows용 OnEum은 이제 두 가지로 나갈 수 있습니다. **프로그램(`OnEum.exe`)은 하나**이고 포장만 다릅니다.

| | 내려받는 것 (zip · 설치 프로그램) | Microsoft Store의 패키지 (MSIX) |
|---|---|---|
| 만드는 법 | `scripts/package-windows.sh` | 그 뒤에 `scripts/package-windows-store.sh`, Windows에서 `scripts/pack-msix.ps1` |
| 서명 | 없음 (SignPath Foundation 심사 대기) | Store가 서명 (올리는 패키지는 서명 없음) |
| 설치 · 업데이트 | 사용자가 직접 | Store가 설치하고 새 버전으로 바꿔 줌 |
| 버전 표시 | 0.1.0 | 패키지 버전 `1.0.<커밋 수>.0` (프로그램 안의 표시는 0.1.0 그대로) |
| 데이터 위치 | `%APPDATA%\OnEum`, `%LOCALAPPDATA%\OnEum` | 같은 경로로 쓰지만 Windows가 패키지 전용 위치로 돌릴 수 있음 (6절, **미확인**) |

표시: **[인증]** Microsoft 계정 로그인이 필요, **[결정]** 정해야 진행, **[동의]** 약관 동의. 이 단계들은 계정을 가진 사람이 직접 합니다.

## 1. 준비된 것

- **패키지의 매니페스트** `assets/windows/store/AppxManifest.xml.in`: 64비트 Windows 10(1809) 이상, 언어 영어 · 한국어, 일반 Windows 프로그램으로 실행(`EntryPoint="Windows.FullTrustApplication"`). 선언한 기능은 셋입니다.
  - `runFullTrust`: 패키지에 담긴 일반 Windows 프로그램이면 선언해야 하는 "제한된 기능"입니다. 제출할 때 쓰는 이유를 적어야 합니다(5절).
  - `internetClient`: 사용자가 적은 WebDAV 서버, 켰을 때의 온라인 가사.
  - `privateNetworkClientServer`: 집 네트워크에서 미디어 서버(DLNA) 찾기.
- **그림 21장** `assets/windows/store/Assets`: 앱 아이콘(`assets/icon-1024.png`)에서 `scripts/gen-windows-store-logos.py`가 만듭니다. Store와 설치 화면의 그림(50), 시작 메뉴 타일(150), 앱 목록(44)과 각각의 200% 크기, 그리고 작업 표시줄 · Alt+Tab용 고정 크기(16 · 24 · 32 · 48 · 256)를 판 있는 것 · 판 없는 것(어두운 테마, 밝은 테마)으로. 판 없는 그림이 없으면 Windows가 아이콘을 작게, 자기 판 위에 그립니다(Microsoft의 아이콘 문서).
- **패키지가 될 폴더를 만드는 스크립트** `scripts/package-windows-store.sh`(Mac): zip에서 `OnEum.exe`와 라이선스 파일을 꺼내고 매니페스트와 그림을 더해 `dist/OnEum-<버전>-windows-x86_64-store/`를 만듭니다. zip의 것과 바이트까지 같은 프로그램인지, 매니페스트가 가리키는 파일이 다 있는지, 버전이 Store의 규칙(첫 자리 1 이상, 끝자리 0)에 맞는지 검사합니다.
- **패키지로 묶는 스크립트** `scripts/pack-msix.ps1`(Windows, PowerShell 7, Windows SDK): `MakePri`로 그림 색인(`resources.pri`)을 만들고, 색인이 그림마다 모든 크기 · 테마를 알고 있는지 되읽어 세고, `MakeAppx`로 묶은 뒤(이때 매니페스트를 검사) 다시 풀어 프로그램과 매니페스트가 넣은 그대로인지 봅니다.
- **워크플로** `.github/workflows/windows-package.yml`: Mac 러너에서 zip · 설치 프로그램 · 패키지 폴더를 만들고, Windows 러너의 `store` 작업이 폴더를 `.msix`로 묶어 아티팩트 `OnEum-windows-store`로 남깁니다.
- **패키지의 주인을 정하는 세 값**은 저장소에 적지 않고 환경 변수(러너에서는 저장소의 secret)로 받습니다: `ONEUM_STORE_NAME`, `ONEUM_STORE_PUBLISHER`, `ONEUM_STORE_PUBLISHER_NAME`. Partner Center에서 이름을 예약하면 나오는 값입니다(4절).
  - **셋 다 없으면 시험용 패키지**가 만들어집니다(이름 `OnEum.Test`). Store에는 올릴 수 없고, Windows 11에서 서명 없이 설치해 볼 수 있게 Microsoft가 정해 둔 게시자 값을 씁니다(3절).

## 2. 만들기

GitHub에서: **Actions → Windows package → Run workflow**. 끝나면 아티팩트가 셋 남습니다 — `OnEum-windows-x86_64`(zip, 설치 프로그램, 체크섬), `OnEum-windows-store-folder`(패키지가 될 폴더), `OnEum-windows-store`(`.msix`).

손으로 할 때:

```sh
scripts/package-windows.sh           # Mac: zip과 설치 프로그램
scripts/package-windows-store.sh     # Mac: dist/OnEum-<버전>-windows-x86_64-store/
```

```powershell
# Windows (Windows SDK가 있는 PC): 위의 폴더를 가져와서
pwsh scripts/pack-msix.ps1 -Layout .\OnEum-0.1.0-windows-x86_64-store -Package .\OnEum-0.1.0-windows-x86_64.msix
```

Store에 올릴 패키지를 만들 때는 세 값을 줍니다(값은 Partner Center의 것. 저장소 파일이나 커밋에 적지 않습니다).

```sh
ONEUM_STORE_NAME=… ONEUM_STORE_PUBLISHER=… ONEUM_STORE_PUBLISHER_NAME=… scripts/package-windows-store.sh
```

패키지 버전은 `ONEUM_STORE_VERSION`으로 바꿀 수 있습니다(기본 `1.0.<커밋 수>.0` — 다른 패키지들이 빌드 번호로 쓰는 그 수). Store는 첫 자리가 0인 버전을 받지 않아서 프로그램의 0.1.0을 그대로 쓸 수 없습니다. **[결정]** 이 기본값이 싫으면 다른 규칙을 정해야 합니다. 사용자의 기기가 새 패키지로 바뀌려면 버전이 전보다 커야 합니다.

## 3. Windows PC에서 확인하기 (올리기 전에)

**먼저 할 일**: Windows판은 아직 실제 Windows에서 한 번도 실행해 보지 못했습니다([TESTING.md](TESTING.md) "Windows"). Store의 정책(10.1.2)은 제품이 완전히 동작할 것을 요구하므로, 내려받는 zip으로 먼저 실행해 보고 그다음에 패키지를 봅니다.

시험용 패키지 설치(Windows 11, Microsoft의 "unsigned package" 문서에 있는 방법 — **직접 해 보지 못했습니다**):

```powershell
# PowerShell을 관리자 권한으로 열고, 아티팩트 OnEum-windows-store의 파일로
Add-AppxPackage -Path .\OnEum-0.1.0-windows-x86_64.msix -AllowUnsigned
# 지울 때
Get-AppxPackage OnEum.Test | Remove-AppxPackage
```

볼 것:

- 시작 메뉴에 OnEum이 생기고 실행되는가. 작업 표시줄과 Alt+Tab의 아이콘이 판 없이 제 크기로 보이는가(밝은 테마, 어두운 테마 둘 다).
- 다른 드라이브의 음악 폴더를 보관함에 더해 재생되는가. 껐다 켜도 보관함과 설정이 남아 있는가.
- **데이터가 실제로 어디에 생겼는가**: `%APPDATA%\OnEum`에 보이는가, 아니면 `%LOCALAPPDATA%\Packages\OnEum.Test_…\LocalCache\` 아래에만 있는가. 패키지를 지우면 함께 지워지는가. (결과에 따라 사용 안내의 경로 안내를 Store판에 맞게 고쳐야 합니다.)
- 키보드의 미디어 키와 Windows의 미디어 표시, 저장한 서버 비밀번호(자격 증명 관리자), 미디어 서버 찾기, 온라인 가사.
- Windows SDK의 **Windows App Certification Kit**로 검사(Microsoft가 제출 전에 돌려 보라고 하는 검사. Store의 심사도 이것을 씁니다).
- Store에 넣을 **스크린샷**도 이때 찍습니다.

## 4. 계정과 제출 순서

1. **[인증] [동의] 개발자 등록.** `https://storedeveloper.microsoft.com`에서 시작해 "Individual developer"를 고릅니다(이 입구로 들어가야 등록비가 없는 절차가 나옵니다). 개인 Microsoft 계정이 필요하고, 신분증과 셀카를 휴대폰으로 찍어 본인 확인을 합니다.
2. **[인증] [결정] 이름 예약.** Partner Center에서 새 앱을 만들고 이름 "OnEum"을 예약합니다. 이미 누가 쓴 이름이면 다른 이름을 정해야 합니다.
3. **[인증] 식별 값 세 개를 저장소의 secret으로.** 그 앱의 Product identity 화면에 나오는 `Package/Identity/Name`, `Package/Identity/Publisher`, `Package/Properties/PublisherDisplayName`을 GitHub 저장소의 Settings → Secrets and variables → Actions에 `ONEUM_STORE_NAME`, `ONEUM_STORE_PUBLISHER`, `ONEUM_STORE_PUBLISHER_NAME`으로 넣습니다. (명령으로는 `gh secret set ONEUM_STORE_NAME --repo ywjung/oneum`처럼 — 값은 묻는 자리에 붙여 넣습니다.)
4. **패키지 만들기.** 워크플로를 돌려 아티팩트 `OnEum-windows-store`의 `.msix`를 받습니다. 이번에는 Store의 식별 값이 들어간 패키지입니다(3절의 `-AllowUnsigned`로는 설치되지 않습니다).
5. **[인증] [결정] 제출.** 가격(무료)과 판매 지역, 분류(음악), 개인정보 처리방침 주소, 연령 등급 설문, 패키지 올리기, Store에 보일 글과 스크린샷(한국어 · 영어), `runFullTrust`를 쓰는 이유(5절).
6. **심사.** Microsoft의 문서에는 최대 3영업일이라고 적혀 있습니다. 통과하면 Store가 패키지에 서명해 게시합니다.
7. **이후의 릴리스.** 버전이 커진 패키지를 같은 순서로 올립니다(4 → 5 → 6).

## 5. 제출에 적을 것 (초안)

- **개인정보 처리방침 주소**: `https://ywjung.github.io/oneum/privacy.html` (영어 `https://ywjung.github.io/oneum/en/privacy.html`). 일반 Windows 프로그램을 담은 제품은 정책 10.5.1에 따라 반드시 있어야 합니다.
- **제한된 기능 `runFullTrust`를 쓰는 이유** (Submission options에 적는 글, 영어):

  ```text
  OnEum is a classic Win32 desktop music player (one executable, written in Rust) that is
  packaged as MSIX. It is not a UWP app and cannot run in an AppContainer, so it declares
  runFullTrust like every packaged desktop program. It plays the music files in the folders
  the user adds to its library, on any drive including removable and network drives, and
  plays them through the standard Windows audio output. It installs no driver and no
  service, does not start with Windows, and changes no system setting.
  ```

- **Store에 보일 글**(설명, 기능 목록, 검색어)과 **연령 등급 설문의 답**은 아직 쓰지 않았습니다. App Store · Google Play용 글([APP_STORE_LISTING.md](APP_STORE_LISTING.md), [PLAY_STORE_LISTING.md](PLAY_STORE_LISTING.md))을 바탕으로, Partner Center의 글자 수 제한을 확인한 뒤에 씁니다.

## 6. 확인한 것과 확인하지 못한 것 (2026-11-25)

Microsoft의 문서에서 확인한 것(문서 이름은 learn.microsoft.com의 것):

- 개인 개발자 등록은 무료이고 `storedeveloper.microsoft.com`이 그 절차의 유일한 입구입니다. 본인 확인은 신분증과 셀카. (Free developer registration for individual developers; Steps to open a Microsoft Store developer account)
- MSIX 패키지는 Store가 게시할 때 Microsoft의 인증서로 다시 서명하고, 올리는 패키지는 공인 인증서로 서명되어 있을 필요가 없습니다. Store로 설치한 MSIX에서는 SmartScreen 경고가 나오지 않습니다. EXE · MSI 설치 프로그램으로 올리는 길은 Store가 서명해 주지 않아 직접 서명해야 합니다. (App package requirements for MSIX app; Code signing options for Windows app developers)
- 패키지 버전의 첫 자리는 0일 수 없고 끝자리는 0으로 두어야 합니다. (App package requirements for MSIX app)
- `runFullTrust`는 제한된 기능이고, 제출할 때 쓰는 이유를 적어 승인받아야 하며 승인되지 않으면 심사에서 떨어집니다. (App capability declarations)
- 심사는 최대 3영업일. (The app certification process for MSIX app)
- GitHub의 Windows 러너(Windows Server 2025)에는 Windows SDK 10.0.26100과 PowerShell 7이 들어 있습니다. (actions/runner-images의 목록)

이 Mac에서 확인한 것:

- `scripts/package-windows-store.sh`가 폴더를 만듭니다(파일 26개, 패키지 버전 `1.0.191.0`). 식별 값이 없을 때(시험용)와 있을 때 모두 매니페스트가 XML로 읽히고(`xmllint`), 준 값이 그대로 되읽힙니다(이름에 `&`, `"`, `<`가 있어도).
- 실패해야 할 때 실패하고 폴더를 남기지 않습니다: 식별 값이 일부만 있을 때, 버전이 `0.1.0.0` · `1.0.0.1` · `1.0.70000.0` · `1.2.3` · `1.02.3.0`일 때, 이름이나 게시자의 꼴이 틀릴 때, 그림 폴더에 숨은 파일이 있을 때, zip이 없을 때.

확인하지 못한 것:

- **`scripts/pack-msix.ps1`과 워크플로의 `store` 작업은 한 번도 돌려 보지 못했습니다.** 이 Mac에는 PowerShell도 Windows SDK도 없습니다. 매니페스트가 Windows의 규칙에 맞는지(`MakeAppx`의 검사), `MakePri`가 그림 이름을 기대한 대로 읽는지는 워크플로를 GitHub에서 돌려야 알 수 있습니다.
- **Windows에서의 모든 것**: 패키지가 설치되는지, 프로그램이 패키지 안에서 제대로 도는지(오디오 출력, 미디어 키, 자격 증명, 네트워크), 아이콘이 어떻게 보이는지, Windows App Certification Kit의 결과.
- **데이터 위치**: 패키지된 프로그램이 `%APPDATA%` · `%LOCALAPPDATA%`에 새로 만드는 파일은 패키지 전용 위치로 돌려지고 제거할 때 함께 지워진다고 문서에 있습니다. 그런데 그 설명이 OnEum처럼 full trust로 도는 프로그램에도 해당하는지는 Microsoft의 문서끼리 다르게 읽힙니다. PC에서 봐야 합니다(3절).
- **서명이 전혀 없는 패키지를 Partner Center가 받는지**: 문서에는 "공인 인증서로 서명할 필요가 없다"고만 있습니다.
- **한국에서의 개인 등록**: 문서는 "약 200개 시장"이라고만 하고 나라 목록이 없습니다.
- **게시자로 보이는 이름**: 개인 계정은 "본인 이름으로" 게시한다고 되어 있어 실명이 보일 수 있습니다. 다른 이름을 쓸 수 있는지는 문서에 없습니다.
- **온라인 가사와 정책 11.2**(콘텐츠를 쓸 권리): 심사에서 문제가 되는지 모릅니다.
- 매니페스트의 `MaxVersionTested`(Windows 11 24H2)는 관례에 따라 적은 값이고, 그 Windows에서 시험했다는 뜻이 아닙니다.

## 7. 내려받는 것과 다른 점 (사용자에게 알릴 것)

- Store의 패키지는 Store가 업데이트합니다. 내려받은 것은 새 파일을 직접 받아 설치해야 합니다.
- Store에 보이는 버전은 `1.0.<수>.0` 꼴이고, 프로그램 안(설정 → 정보)의 버전은 0.1.0처럼 그대로입니다.
- 데이터 위치와 제거할 때 데이터가 함께 지워지는지는 3절의 확인 결과에 따라 적습니다.
- GitHub 릴리스의 zip과 설치 프로그램은 Store와 관계없이 서명되지 않은 채입니다(SignPath Foundation의 결과에 따라 달라집니다).
