# Google Play에 넣을 글 (초안)

작성 2026-11-24 · 절차는 [PLAY_STORE.md](PLAY_STORE.md) 8절. 본인 이름으로 공개되는 글이니 읽고 고쳐 쓰세요.

- 각 칸의 글은 아래 코드 블록 그대로 붙여 넣습니다. 블록 위의 `<!-- field … -->` 줄은 길이 검사용 표시입니다.
- 길이 검사: `python3 scripts/dev/check-listing.py docs/PLAY_STORE_LISTING.md` (한도를 넘으면 실패). 고친 뒤에 다시 돌리세요. 인자 없이 돌리면 App Store의 글과 이 글을 함께 봅니다.
- 한도(Play Console 도움말 "Create and set up your app", 2026-11-24 확인): 앱 이름 30자, 짧은 설명 80자, 자세한 설명 4,000자. 전각 · 반각 글자를 똑같이 한 자로 셉니다. 출시 노트는 500자입니다(Play Console의 입력 칸에 표시되는 한도, 문서로는 확인 못 함).
- 다른 운영체제 · 다른 스토어의 이름, 가격, "최고" · "1위" · "무료" 같은 홍보 문구는 넣지 않았습니다(Play의 메타데이터 정책이 제목 · 아이콘 · 개발자 이름에서 금하고, 설명에서도 피하라고 하는 것). 검사 스크립트가 이름과 짧은 설명에서 그런 낱말을 찾습니다.
- 실제 음악가의 이름을 예로 들지 않았습니다.
- **이 글은 Play용 빌드(모든 파일 접근이 없는 빌드)에 맞춘 것입니다.** 그 빌드가 보관함에서 읽지 못하는 것은 적지 않았습니다: WavPack · Monkey's Audio · DSD 파일, 곡 옆에 따로 놓인 표지 그림 · 가사(.lrc) · CUE 시트. 직접 설치하는 APK의 설명에 쓰면 안 맞는 곳이 생깁니다.
- **앱의 기능이 바뀌면 이 글도 고칩니다.**

## 넣기 전에 실제 휴대폰에서 확인할 문장

아래 기능은 에뮬레이터(Android 16)에서만 확인했습니다. 비공개 테스트로 받은 앱에서 본 뒤, 되지 않는 것이 있으면 설명에서 그 줄을 빼세요.

| 설명의 문장 | 지금까지 확인한 곳 |
|---|---|
| 화면을 꺼도 재생이 이어지고 알림 · 잠금 화면에서 조작 | 에뮬레이터(미디어 세션과 알림의 버튼). 실제 휴대폰의 잠금 화면은 본 적 없음 |
| 홈 화면 위젯 | 에뮬레이터 |
| 전화가 오면 멈췄다가 이어서 재생, 이어폰이 빠지면 일시정지 | 에뮬레이터(전화는 에뮬레이터의 가짜 전화로, 이어폰 분리는 코드만) |
| 같은 네트워크의 미디어 서버 · WebDAV 폴더 | 가짜 서버로만 |
| 가로 화면 | 에뮬레이터 |

USB DAC, SD 카드의 폴더, 연결한 키보드의 단축키, TalkBack에 관한 문장은 실제 휴대폰으로 확인한 적이 없어 **설명에 넣지 않았습니다.**

## 공통

| 칸 | 값 |
|---|---|
| 앱 이름 | 아래 `ko.title` · `en.title` (이름만 쓰려면 `OnEum`) |
| 패키지 이름 | `app.oneum.android` (한 번 올리면 바꿀 수 없음) |
| 앱 / 게임 | 앱 |
| 카테고리 | 음악 및 오디오 |
| 무료 / 유료 | 무료 (Play에서는 무료로 낸 앱을 나중에 유료로 바꿀 수 없는 것으로 알려져 있음 — 확인 못 함) |
| 연락처 e-mail | 본인의 것(스토어에 공개됨) |
| 웹사이트 | `https://ywjung.github.io/oneum/` |
| 개인정보 처리방침 | 한국어 `https://ywjung.github.io/oneum/privacy.html`, 영어 `https://ywjung.github.io/oneum/en/privacy.html` (Play Console의 칸은 하나: 기본 언어의 것을 넣음) |
| 버전 | `0.1.0`(`Cargo.toml`), 버전 코드는 커밋 수 |

## 그림

`scripts/dev/play-shots.sh`가 `dist/play/store/`에 만듭니다(저장소에는 넣지 않음). 요구되는 크기와 규칙은 [PLAY_STORE.md](PLAY_STORE.md) 6절.

| 파일 | 무엇 |
|---|---|
| `ko/01-albums.png` … `07-search.png`, `en/…` | 휴대폰 스크린샷 일곱 장씩, 1080×1920: 앨범 목록, 앨범, 플레이어, 가사, VU 미터, 이퀄라이저, 검색 |
| `feature-graphic-ko.png`, `feature-graphic-en.png` | 기능 그래픽 1024×500: 아이콘의 온음표, 이름, 한 줄("내 음악 파일을 있는 그대로" / "Your music files, as they are") |
| `icon-512.png` | 스토어 아이콘 512×512 |
| `foreground-service.mp4` | 재생 서비스 신고에 넣을 영상(링크로 넣어야 하므로 올릴 곳이 필요) |

알아 둘 것: 에뮬레이터의 화면이고(실제 휴대폰이 아님), Play용 빌드를 찍은 것이며, 음악가 · 표지 · 가사는 모두 지어낸 것입니다. 한국어 앨범 목록은 목록의 끝쪽(한글 이름의 앨범)을 찍습니다. 검색 화면은 자판을 내린 뒤에 찍습니다.

## 한국어 (ko-KR)

<!-- field ko.title chars=30 -->
```text
OnEum: 오프라인 음악 플레이어
```

<!-- field ko.short chars=80 -->
```text
가지고 있는 음악 파일을 있는 그대로 듣습니다. 끊김 없는 재생, 가사, VU 미터, 이퀄라이저. 계정도 광고도 없습니다.
```

<!-- field ko.full chars=4000 -->
```text
OnEum(온음)은 가지고 있는 음악 파일을 듣는 오프라인 플레이어입니다. 휴대폰의 음악을 읽어 바로 재생하고, 파일은 읽기만 합니다. 계정도 광고도 없고, 인터넷 없이 씁니다.

스트리밍 서비스가 아닙니다. 직접 가지고 있는 음악 파일이 있어야 합니다.

■ 내 파일 그대로
• FLAC, ALAC, WAV, AIFF, MP3, AAC, Opus, Ogg Vorbis
• 휴대폰의 Music 폴더에 있는 음악이 저절로 목록에 나타납니다. 다른 폴더도 추가할 수 있고, 음악은 복사하지 않고 있는 자리에서 읽습니다
• 음악 파일과 태그는 바꾸지 않습니다. 즐겨찾기, 별점, 재생 횟수는 앱 안에만 둡니다

■ 있는 그대로 재생
• 같은 앨범의 곡은 사이가 끊기지 않습니다(gapless)
• 음량 보정(ReplayGain): 파일의 태그를 쓰고, 태그가 없는 곡은 앱이 재서 맞춥니다
• 파일에서 출력까지 소리가 거치는 단계를 화면에 보여 줍니다. 켠 것이 없으면 파일의 소리가 그대로 나갑니다

■ 듣는 화면
• 표지, 가사(시간 정보가 있으면 부르는 줄을 따라갑니다), 좌우 VU 미터와 스펙트럼, 재생 대기열
• 화면을 꺼도 다른 앱을 써도 재생이 이어지고, 알림과 잠금 화면에서 조작합니다
• 홈 화면 위젯
• 잠자기 타이머, 현재 곡이 끝나면 정지, 구간 반복(A–B), 재생 속도(음높이는 그대로), 크로스페이드
• 가로 화면

■ 찾고 정리하기
• 곡, 앨범, 아티스트, 장르, 폴더로 둘러보기
• 검색: 초성만 넣어도 찾고, genre:재즈 · year:1990-1999 · format:flac 처럼 찾을 곳을 정할 수 있습니다
• 플레이리스트와 스마트 플레이리스트(저장한 검색), M3U8 가져오기와 내보내기
• 즐겨찾기, 별점, 자주 가는 앨범과 아티스트 고정
• 통계: 기간별로 들은 시간, 많이 들은 곡과 앨범
• 다시 듣기: 최근에 넣은 곡, 많이 들은 곡, 오래 안 들은 곡

■ 소리 다듬기 (모두 선택)
• 10밴드 이퀄라이저와 프리셋
• 헤드폰 보정: 널리 쓰이는 헤드폰의 프로필, 보정값 파일 불러오기, 직접 만드는 파라메트릭 이퀄라이저
• 헤드폰 크로스피드, 스피커를 위한 룸 보정
• 파일 검사: 손상된 파일 찾기, 다이내믹 레인지, 트루 피크

■ 네트워크의 음악 (선택)
• 같은 네트워크의 미디어 서버(DLNA)와 NAS의 WebDAV 폴더를 추가하고 둘러보며 듣습니다
• 네트워크는 그 화면을 열 때와 서버를 추가할 때만 씁니다

■ 권한
• 음악 및 오디오: 휴대폰의 음악 파일을 읽는 데 씁니다. 사진, 동영상, 다른 파일은 읽지 않습니다
• 알림: 재생 중인 곡과 조작 버튼을 알림에 보여 줍니다

■ 개인정보
• 수집하는 것이 없습니다. 보관함과 설정은 기기에만 있습니다
• 인터넷에서 가사 찾기는 꺼져 있습니다. 켜면 가사를 보는 곡의 제목 · 아티스트 · 앨범 · 길이만 LRCLIB에 보냅니다

■ 알아 둘 것
• 구독 서비스의 보관함에 있는 곡은 재생하지 않습니다. 파일로 가지고 있는 음악만 재생합니다
• 표지와 가사는 음악 파일 안에 든 것을 보여 줍니다
• 64비트 ARM 휴대폰, Android 11 이상
• 한국어와 영어를 지원합니다
• 소스가 공개된 소프트웨어입니다(0BSD)
```

<!-- field ko.whatsnew chars=500 -->
```text
첫 공개 버전입니다.
```

## English (en-US)

<!-- field en.title chars=30 -->
```text
OnEum: Offline Music Player
```

<!-- field en.short chars=80 -->
```text
Plays the music files you own, as they are. Gapless, lyrics, VU meters. No ads.
```

<!-- field en.full chars=4000 -->
```text
OnEum is an offline player for the music files you own. It reads the music on your phone and plays it at once; files are only read. There is no account, there are no ads, and it works without the internet.

It is not a streaming service. You need music files of your own.

■ YOUR FILES AS THEY ARE
• FLAC, ALAC, WAV, AIFF, MP3, AAC, Opus and Ogg Vorbis
• The music in your phone's Music folder shows up by itself. You can add other folders; music is read where it is, not copied
• Music files and their tags are never changed. Favourites, ratings and play counts stay inside the app

■ PLAYED AS THEY ARE
• The songs of an album follow each other without a gap
• Volume leveling (ReplayGain): tags are used, and songs without them are measured by the app
• The steps the sound passes from the file to the output are shown on screen. With nothing switched on, the file's sound goes out as it is

■ THE PLAYER
• Cover, lyrics (timed lyrics follow the song), a VU meter for each channel with a spectrum, and the play queue
• Playback goes on with the screen off or another app in front, with controls in the notification and on the lock screen
• Home screen widget
• Sleep timer, stop after this song, A–B repeat, playback speed at the same pitch, crossfade
• Landscape

■ FIND AND ORGANISE
• Browse by song, album, artist, genre and folder
• Search with fields such as genre:jazz, year:1990-1999 or format:flac; Korean can be searched by initial consonants
• Playlists and smart playlists (saved searches); import and export M3U8
• Favourites, ratings, and pins for the albums and artists you go to most
• Statistics: listening time by period, your most played songs and albums
• Rediscover: recently added, most played, not heard for long

■ SHAPING THE SOUND (ALL OPTIONAL)
• 10-band equalizer with presets
• Headphone correction: profiles for widely used headphones, correction files you load, or a parametric equalizer you make yourself
• Crossfeed for headphones, room correction for speakers
• File check: finds damaged files, measures dynamic range and true peak

■ MUSIC ON YOUR NETWORK (OPTIONAL)
• Add a media server (DLNA) on your network or a WebDAV folder on a NAS, then browse and play
• The network is used only when you open that screen or add a server

■ PERMISSIONS
• Music and audio: to read the music files on your phone. Photos, videos and other files are not read
• Notifications: to show the song that plays, with its controls

■ PRIVACY
• Nothing is collected. Your library and settings stay on the device
• Lyrics from the internet are off. If you switch them on, only the title, artist, album and length of the song whose lyrics you open are sent to LRCLIB

■ GOOD TO KNOW
• Songs in a subscription service's library are not played: only music you have as files
• Covers and lyrics are shown from what is inside the music files
• For 64-bit ARM phones with Android 11 or later
• In Korean and English
• Open-source software (0BSD)
```

<!-- field en.whatsnew chars=500 -->
```text
The first public version.
```
