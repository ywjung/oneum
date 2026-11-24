# App Store에 넣을 글 (초안): iPhone과 Mac

작성 2026-11-22 · 절차는 [APP_STORE.md](APP_STORE.md) 8절. 본인 이름으로 공개되는 글이니 읽고 고쳐 쓰세요.

- 각 칸의 글은 아래 코드 블록 그대로 붙여 넣습니다. 블록 위의 `<!-- field … -->` 줄은 길이 검사용 표시입니다.
- 길이 검사: `python3 scripts/dev/check-listing.py` (한도를 넘으면 실패). 고친 뒤에 다시 돌리세요.
- 다른 운영체제의 이름과 가격은 넣지 않았습니다(App Store 심사 지침이 메타데이터에서 금하는 것).
- 실제 음악가의 이름을 예로 들지 않았습니다.
- **앱의 기능이 바뀌면 이 글도 고칩니다.** 특히 인터넷 가사를 iPhone판에서 빼기로 하면 "가사" 줄을, 멀티캐스트 권한을 받아 서버를 저절로 찾게 되면 "주소를 넣어" 부분을 고칩니다.

## 넣기 전에 실제 iPhone에서 확인할 문장

아래 기능은 시뮬레이터에서만 확인했습니다. TestFlight 빌드로 본 뒤, 되지 않는 것이 있으면 설명에서 그 줄을 빼세요.

| 설명의 문장 | 지금까지 확인한 곳 |
|---|---|
| 잠금 화면 · 제어 센터에서 조작 | 코드만. 실제 iPhone에서 본 적 없음 |
| 홈 화면 · 잠금 화면 위젯 | 시뮬레이터. 배포 서명에서는 본 적 없음 |
| iCloud Drive · 꽂은 저장 장치의 폴더를 복사 없이 연결 | 시뮬레이터(권한이 다음 실행에도 통하는지는 기기에서) |
| 전화가 오면 멈췄다가 이어서 재생, 이어폰이 빠지면 일시정지 | 코드만 |
| 같은 네트워크의 미디어 서버를 주소로 추가 | 가짜 서버로만 |

USB DAC에 관한 문장(곡의 샘플레이트로 출력, 업샘플링)은 실제 iPhone과 DAC으로 확인한 적이 없어 **설명에 넣지 않았습니다.**

## 공통

| 칸 | 값 |
|---|---|
| 이름 | `OnEum` (이미 쓰이고 있으면 아래 "이름의 다른 안") |
| 기본 카테고리 | 음악 |
| 보조 카테고리 | 없음 |
| 저작권 | `2026 <본인의 법적 이름>` |
| 지원 URL | `https://github.com/ywjung/oneum/issues` (정할 것: 다른 연락 방법을 쓸지) |
| 마케팅 URL | `https://ywjung.github.io/oneum/` |
| 개인정보 처리방침 URL | 한국어 `https://ywjung.github.io/oneum/privacy.html`, 영어 `https://ywjung.github.io/oneum/en/privacy.html` |
| 버전 | `0.1.0`(`Cargo.toml`). 스토어에 처음 내는 판을 1.0으로 할지는 정할 것 |

이름의 다른 안(30자까지): `OnEum 온음`, `OnEum 음악 플레이어`, `OnEum Music Player`.

## 한국어

<!-- field ko.subtitle chars=30 -->
```text
내 음악 파일을 위한 오프라인 플레이어
```

<!-- field ko.promo chars=170 -->
```text
가지고 있는 음악 파일을 있는 그대로 듣습니다. FLAC · ALAC · DSD까지 곡 사이 끊김 없이 재생하고, 가사 · VU 미터 · 이퀄라이저 · 헤드폰 보정을 갖췄습니다. 계정도 광고도 없습니다.
```

키워드는 100바이트까지입니다. 한글은 한 글자가 3바이트로 세어질 수 있어 짧게 잡았습니다. 이름과 부제에 있는 낱말(음악, 오프라인, 플레이어)은 넣지 않았습니다. 입력 칸이 글자 수로 세면 아래 "더 넣을 키워드"에서 골라 더하세요.

<!-- field ko.keywords bytes=100 -->
```text
FLAC,무손실,고음질,가사,이퀄라이저,DSD,ALAC,NAS,DLNA,로컬,MP3,WebDAV,헤드폰
```

더 넣을 키워드: `갭리스`, `VU`, `스펙트럼`, `플레이리스트`, `Opus`, `WAV`, `하이파이`.

<!-- field ko.description chars=4000 -->
```text
OnEum(온음)은 가지고 있는 음악 파일을 듣는 오프라인 플레이어입니다. 음악을 넣으면 바로 재생하고, 파일은 읽기만 합니다. 계정도 광고도 없고, 인터넷 없이 씁니다.

스트리밍 서비스가 아닙니다. 직접 가지고 있는 음악 파일이 있어야 합니다.

■ 내 파일 그대로
• FLAC, ALAC, WAV, AIFF, WavPack, MP3, AAC, Opus, Ogg Vorbis, Monkey's Audio(APE), DSD(PCM으로 바꿔 재생)
• 파일 앱이나 컴퓨터에서 음악 폴더를 넣으면 저절로 목록에 나타납니다
• iCloud Drive나 꽂은 저장 장치의 폴더를 복사하지 않고 연결할 수 있습니다
• 음악 파일과 태그는 바꾸지 않습니다. 즐겨찾기, 별점, 재생 횟수는 앱 안에만 둡니다

■ 있는 그대로 재생
• 같은 앨범의 곡은 사이가 끊기지 않습니다(gapless)
• 앨범 전체가 파일 하나여도 CUE 시트대로 곡을 나눠 보여 줍니다
• 음량 보정(ReplayGain): 파일의 태그를 쓰고, 태그가 없는 곡은 앱이 재서 맞춥니다
• 파일에서 출력까지 소리가 거치는 단계를 화면에 보여 줍니다. 켠 것이 없으면 파일의 소리가 그대로 나갑니다

■ 듣는 화면
• 표지, 가사(시간 정보가 있으면 부르는 줄을 따라갑니다), 좌우 VU 미터와 스펙트럼, 재생 대기열
• 화면을 꺼도 다른 앱을 써도 재생이 이어지고, 잠금 화면과 제어 센터에서 조작합니다
• 홈 화면과 잠금 화면 위젯
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
• 같은 네트워크의 미디어 서버(DLNA)와 NAS의 WebDAV 폴더를 주소를 넣어 추가하고 둘러보며 듣습니다
• 네트워크는 그 화면을 열 때와 서버를 추가할 때만 씁니다

■ 개인정보
• 수집하는 것이 없습니다. 보관함과 설정은 기기에만 있습니다
• 인터넷에서 가사 찾기는 꺼져 있습니다. 켜면 가사를 보는 곡의 제목 · 아티스트 · 앨범 · 길이만 LRCLIB에 보냅니다

■ 알아 둘 것
• 구독 서비스의 보관함에 있는 곡은 재생하지 않습니다. 파일로 가지고 있는 음악만 재생합니다
• iPad에서는 iPhone용 화면으로 열립니다
• 한국어와 영어를 지원합니다
• 소스가 공개된 소프트웨어입니다(0BSD)
```

## English

<!-- field en.subtitle chars=30 -->
```text
Offline player for your files
```

<!-- field en.promo chars=170 -->
```text
Your own music files, played as they are. Gapless FLAC, ALAC and DSD, with lyrics, VU meters, an equalizer and headphone correction. No account and no ads.
```

Keywords: up to 100 bytes. Words of the name and the subtitle (offline, player, files) are left out.

<!-- field en.keywords bytes=100 -->
```text
flac,lossless,music,local,alac,dsd,gapless,equalizer,lyrics,nas,dlna,webdav,hifi,mp3,audiophile
```

<!-- field en.description chars=4000 -->
```text
OnEum is an offline player for the music files you own. Put your music in and it plays at once; files are only read. There is no account, there are no ads, and it works without the internet.

It is not a streaming service. You need music files of your own.

■ YOUR FILES AS THEY ARE
• FLAC, ALAC, WAV, AIFF, WavPack, MP3, AAC, Opus, Ogg Vorbis, Monkey's Audio (APE) and DSD (played as PCM)
• Put a folder of music in with the Files app or from a computer and it shows up by itself
• Link a folder on iCloud Drive or on a storage device you plugged in, without copying it
• Music files and their tags are never changed. Favourites, ratings and play counts stay inside the app

■ PLAYED AS THEY ARE
• The songs of an album follow each other without a gap
• An album that is one file is shown song by song where a CUE sheet lies beside it
• Volume leveling (ReplayGain): tags are used, and songs without them are measured by the app
• The steps the sound passes from the file to the output are shown on screen. With nothing switched on, the file's sound goes out as it is

■ THE PLAYER
• Cover, lyrics (timed lyrics follow the song), a VU meter for each channel with a spectrum, and the play queue
• Playback goes on with the screen off or another app in front, with controls on the Lock Screen and in Control Center
• Home Screen and Lock Screen widgets
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
• Add a media server (DLNA) on your network or a WebDAV folder on a NAS by its address, then browse and play
• The network is used only when you open that screen or add a server

■ PRIVACY
• Nothing is collected. Your library and settings stay on the device
• Lyrics from the internet are off. If you switch them on, only the title, artist, album and length of the song whose lyrics you open are sent to LRCLIB

■ GOOD TO KNOW
• Songs in a subscription service's library are not played: only music you have as files
• On an iPad it opens with its iPhone screens
• In Korean and English
• Open-source software (0BSD)
```

## Mac (Mac App Store)

작성 2026-11-24 · 절차는 [MAC_APP_STORE.md](MAC_APP_STORE.md). 이름과 부제는 iPhone 앱과 같은 것을 씁니다(앱 하나의 정보). 프로모션 텍스트 · 키워드 · 주소 · 저작권은 위의 것과 같고, **설명과 심사 메모만 Mac의 것**입니다.

넣기 전에 TestFlight로 받은 앱에서 확인할 문장(스토어의 앱은 App Sandbox 안에서 돕니다):

| 설명의 문장 | 지금까지 확인한 곳 |
|---|---|
| 곡의 샘플레이트로 출력, 독점 출력, 업샘플링 | 내려받는 앱. 샌드박스 안에서는 돌려 보지 않음 |
| 키보드의 재생 키와 제어 센터의 "지금 재생 중" | 내려받는 앱. 샌드박스 안에서는 보지 않음 |
| 음악 폴더 추가(열기 창) | Finder가 넘긴 폴더로만 확인. 열기 창에서 고른 것은 사람이 해 봐야 함 |
| 같은 네트워크의 미디어 서버 찾기 · 주소로 추가 | 샌드박스를 켠 시험용 앱, 가짜 서버로 |
| WebDAV 폴더 | 내려받는 앱, 가짜 서버로 |

<!-- field mac.ko.description chars=4000 -->
```text
OnEum(온음)은 가지고 있는 음악 파일을 듣는 오프라인 플레이어입니다. 음악 폴더를 고르면 바로 재생하고, 파일은 읽기만 합니다. 계정도 광고도 없고, 인터넷 없이 씁니다.

스트리밍 서비스가 아닙니다. 직접 가지고 있는 음악 파일이 있어야 합니다.

■ 내 파일 그대로
• FLAC, ALAC, WAV, AIFF, WavPack, MP3, AAC, Opus, Ogg Vorbis, Monkey's Audio(APE), DSD(PCM으로 바꿔 재생)
• 음악 폴더를 추가하면 그 안의 음악이 목록에 나타납니다. 폴더는 여러 개 추가할 수 있고, 음악은 복사하지 않고 있는 자리에서 읽습니다
• 음악 파일과 태그는 바꾸지 않습니다. 즐겨찾기, 별점, 재생 횟수는 앱 안에만 둡니다

■ 있는 그대로 재생
• 같은 앨범의 곡은 사이가 끊기지 않습니다(gapless)
• 앨범 전체가 파일 하나여도 CUE 시트대로 곡을 나눠 보여 줍니다
• 음량 보정(ReplayGain): 파일의 태그를 쓰고, 태그가 없는 곡은 앱이 재서 맞춥니다
• 파일에서 출력까지 소리가 거치는 단계를 화면에 보여 줍니다. 켠 것이 없으면 파일의 소리가 그대로 나갑니다

■ DAC과 헤드폰을 위한 출력 (모두 선택)
• 곡의 샘플레이트로 출력: 곡마다 출력 장치를 그 곡의 샘플레이트로 맞춰 변환 없이 내보냅니다
• 독점 출력: 재생하는 동안 다른 앱의 소리가 섞이지 않습니다
• 업샘플링과 필터 선택, 메모리 재생
• 재생이 끝나면 장치의 설정을 원래대로 되돌립니다

■ 듣는 화면
• 표지, 가사(시간 정보가 있으면 부르는 줄을 따라갑니다), 좌우 VU 미터와 스펙트럼, 재생 대기열
• 창을 닫아도 재생이 이어지고, 키보드의 재생 키와 제어 센터로 조작합니다
• 미니 플레이어: 재생 막대만 한 작은 창으로 다른 창 위에 띄워 둡니다
• 잠자기 타이머, 현재 곡이 끝나면 정지, 구간 반복(A–B), 재생 속도(음높이는 그대로), 크로스페이드
• 대부분의 기능에 키보드 단축키가 있습니다

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
• 같은 네트워크의 미디어 서버(DLNA)를 찾거나 주소로 추가하고, NAS의 WebDAV 폴더를 둘러보며 듣습니다
• 네트워크는 그 화면을 열 때와 서버를 추가할 때만 씁니다

■ 개인정보
• 수집하는 것이 없습니다. 보관함과 설정은 이 Mac에만 있습니다
• 인터넷에서 가사 찾기는 꺼져 있습니다. 켜면 가사를 보는 곡의 제목 · 아티스트 · 앨범 · 길이만 LRCLIB에 보냅니다

■ 알아 둘 것
• 앱은 직접 고른 폴더와 파일만 읽습니다
• 구독 서비스의 보관함에 있는 곡은 재생하지 않습니다. 파일로 가지고 있는 음악만 재생합니다
• Apple silicon Mac에서 동작합니다
• 한국어와 영어를 지원합니다
• 소스가 공개된 소프트웨어입니다(0BSD)
```

<!-- field mac.en.description chars=4000 -->
```text
OnEum is an offline player for the music files you own. Choose your music folder and it plays at once; files are only read. There is no account, there are no ads, and it works without the internet.

It is not a streaming service. You need music files of your own.

■ YOUR FILES AS THEY ARE
• FLAC, ALAC, WAV, AIFF, WavPack, MP3, AAC, Opus, Ogg Vorbis, Monkey's Audio (APE) and DSD (played as PCM)
• Add a music folder and the music in it shows up in the list. You can add several folders, and music is read where it is, without copying
• Music files and their tags are never changed. Favourites, ratings and play counts stay inside the app

■ PLAYED AS THEY ARE
• The songs of an album follow each other without a gap
• An album that is one file is shown song by song where a CUE sheet lies beside it
• Volume leveling (ReplayGain): tags are used, and songs without them are measured by the app
• The steps the sound passes from the file to the output are shown on screen. With nothing switched on, the file's sound goes out as it is

■ OUTPUT FOR A DAC AND HEADPHONES (ALL OPTIONAL)
• Output at the song's sample rate: the output device is set to each song's rate, so nothing is converted
• Exclusive output: no other app's sound is mixed in while music plays
• Upsampling with a choice of filters, and playback from memory
• The device's settings are put back when playback ends

■ THE PLAYER
• Cover, lyrics (timed lyrics follow the song), a VU meter for each channel with a spectrum, and the play queue
• Playback goes on with the window closed, with the keyboard's media keys and Control Center
• Mini player: a small window the size of the playback bar that stays in front of everything else
• Sleep timer, stop after this song, A–B repeat, playback speed at the same pitch, crossfade
• Keyboard shortcuts for most functions

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
• Find a media server (DLNA) on your network or add one by its address, and browse a WebDAV folder on a NAS, then play
• The network is used only when you open that screen or add a server

■ PRIVACY
• Nothing is collected. Your library and settings stay on this Mac
• Lyrics from the internet are off. If you switch them on, only the title, artist, album and length of the song whose lyrics you open are sent to LRCLIB

■ GOOD TO KNOW
• The app reads only the folders and files you choose
• Songs in a subscription service's library are not played: only music you have as files
• Runs on a Mac with Apple silicon
• In Korean and English
• Open-source software (0BSD)
```

심사 메모(App Review Information ▸ Notes). 샌드박스 권한마다의 이유와 음악 없이 써 보는 방법입니다.

<!-- field mac.review-notes chars=4000 -->
```text
OnEum plays music files that are on the Mac. There is no account and nothing to sign in to.

How to try it: the app starts with an empty library. Click "Add Music Folder" and choose a folder with audio files (FLAC, MP3, AAC, WAV and others); the songs show up in the list, and a double-click plays one. Single files can be opened with File > Open Music Files. Unlike the iPhone app, the Mac app carries no sample music.

Why each App Sandbox entitlement is used:
- User Selected File, read/write: the app reads the music folders and files the user chooses. Music files are never changed. It writes only where the user picks a place in a save panel (exporting a playlist, exporting the library data, saving the log).
- App-scoped bookmarks: so that the chosen music folders can be read again the next time the app starts.
- Outgoing network connections (client): browsing and playing from a media server (DLNA/UPnP) on the same network, WebDAV folders the user adds, and, only if the user switches it on in Settings, looking up lyrics at LRCLIB. There are no analytics and no accounts.
- Incoming network connections (server): used only to find media servers. The app sends an SSDP search from its own UDP socket and receives the answers on that socket; in the sandbox the socket needs this entitlement to be bound. The app listens for nothing else and serves nothing.

The app uses the network only when the user opens the Network screen, adds a server, or has switched on online lyrics.
```

### Mac의 스크린샷

```bash
scripts/dev/mac-store-shots.sh
```
두 언어의 그림 여섯 장씩을 `dist/appstore/mac/ko`와 `dist/appstore/mac/en`에 만듭니다. 창 안의 화면만 담은 2560×1600(1280×800포인트 창, 한 포인트에 두 픽셀), 투명도 없는 PNG이고 저장소에는 넣지 않습니다. 음악과 가사는 iPhone 그림과 같은 지어낸 것입니다. 앱은 `dist/dev`의 검증용 빌드를 일회용 프로필로 띄우므로, 따로 실행 중인 OnEum과 그 데이터는 건드리지 않습니다.

| 파일 | 화면 |
|---|---|
| `01-albums.png` | 앨범 목록과 재생 대기열 |
| `02-playing.png` | 앨범의 곡 목록, 곡 정보, VU 미터와 스펙트럼(재생 중) |
| `03-lyrics.png` | 곡 정보의 신호 경로와 가사(부르는 줄이 켜진 상태) |
| `04-equalizer.png` | 이퀄라이저(재즈 프리셋) |
| `05-search.png` | 검색(한국어는 초성 `ㄷㅂ`, 영어는 `light`) |
| `06-output.png` | 설정의 재생 탭: 출력 장치와 출력 품질 |

알아 둘 것:
- **소리 없이 찍습니다.** 재생 중에는 앱 볼륨이 0입니다. 미터 그림(02)만 재생 중에 찍어서 볼륨 막대가 0이고 "처리"에 "앱 볼륨"이 나옵니다. 나머지는 곡을 멈춘 채 볼륨을 끝까지 올린 동안에 찍습니다.
- 화면이 잠겨 있지 않아야 하고, 찍는 동안 마우스를 건드리지 않아야 합니다. 1280×800포인트 창이 들어가는 Retina 화면이 필요합니다(1440×900은 Dock이 있는 14인치 화면에 들어가지 않았습니다).
- **출력 장치의 이름은 Mac이 알려 주는 그대로**라서 시스템 언어를 따릅니다. 한국어 Mac에서 찍으면 영어 그림에도 "MacBook Pro 스피커"로 나옵니다(02 · 03 · 06). `ONEUM_SHOTS_OUTPUT=<장치 이름의 일부>`로 그림에 나올 장치를 고를 수 있습니다(앱 자신의 선택일 뿐, 시스템의 기본 출력은 바꾸지 않습니다). 주지 않으면 시스템의 기본 출력 장치가 나옵니다.
- 신호 경로에 "리샘플링"이 나오는 것은 내장 스피커가 48kHz이기 때문입니다.
- 화면 안의 자리는 1280×800 창에 맞춰져 있습니다.

## iPhone의 심사 메모

App Review Information ▸ Notes에 넣는 글입니다(4,000바이트까지, 영어). 2026-11-24에 Apple이 지침 2.1 "Information Needed - New App Submission"으로 여섯 가지를 물어서 그 번호대로 답하도록 다시 썼습니다(처음 낸 짧은 글은 [RELEASE.md](RELEASE.md) "iPhone: App Store 제출 준비"에 있습니다). 1번의 화면 녹화는 같은 칸 아래의 **첨부 파일**에 넣습니다(찍을 순서는 [APP_STORE.md](APP_STORE.md) 11절).

- **앱이 바뀌면 이 글도 고칩니다.** 화면 이름(탭, 단추), 기기 밖으로 나가는 것(지금은 켰을 때의 LRCLIB뿐), 계정 · 결제 · 광고가 없다는 문장이 앱과 같아야 합니다.
- 녹화에 없는 것을 1번에 적지 않습니다. 찍은 뒤에 영상과 맞춰 보세요.

<!-- field review.notes bytes=4000 -->
```text
OnEum is an offline player for music files the user owns. It has no account, no sign-in, no purchases, no ads and no server of its own. Below are the six points asked for under Guideline 2.1 on 24 November 2026.

1. SCREEN RECORDING
Attached to this section (Attachment): oneum-review-0.1.0-147.mp4, 93 seconds, without sound. It is the screen of a physical iPhone 18 Pro running iOS 27.2.1, with build 0.1.0 (147) installed from TestFlight; the taps were made by a UI test on the device. It begins with launching the app and shows: the Library, the albums, an album, playing a song, the player (cover, level meters, queue, lyrics), the next song, pause and resume, the equalizer with a preset, the Playback settings, search and playing a result, and pausing from Control Center.
The music in it is the developer's own files; on a new install the Library is empty and offers Add Sample Music (see 3). On that iPhone Find Lyrics Online (see 4) is switched on.
The app has no account registration, login or account deletion. It has no user-generated content: nothing a user makes is shown to anyone else or leaves the device. It has no paid content or features.

2. PURPOSE AND AUDIENCE
For people who keep their own music as files (CD rips, purchased downloads, their own recordings) and want to play them on the iPhone without a subscription service or an account. The problem: such files, lossless and high-resolution ones above all, need a player that reads them as they are. OnEum plays FLAC, ALAC, WAV, AIFF, DSD, MP3, AAC, Opus and others without gaps between the songs of an album, shows lyrics and level meters, offers an equalizer and headphone correction, and keeps the library, playlists and statistics on the device only.

3. SETUP AND MAIN FEATURES
No login and no credentials are needed.
- Without music of your own: open the app; on the empty Library tab tap "Add Sample Music". Nine short songs in three albums come with the app (synthesized by the developer, not anyone's recordings) and are copied into the app's folder.
- Play: Library > Albums > an album > a song. The bar at the bottom opens the player: cover, lyrics, level meters, queue. (The sample songs carry no lyrics; lyrics are shown for files that have them.)
- Search tab: type a word, or a field such as genre:jazz.
- Playlists tab: a song's "More" menu adds it to a playlist.
- Settings tab: Equalizer, Playback, Library, General (language: Korean or English).
- Your own files: in the Files app put them into On My iPhone > OnEum, or use Import Files, Import Folder or Link Folder in the app.
- Playback goes on in the background, with controls on the Lock Screen and in Control Center (background audio mode).

4. EXTERNAL SERVICES
None for the core functionality: no analytics, no advertising, no authentication, no payment processor, no AI service, no backend. The app works without the internet. Two optional features use a network, and only when the user asks:
- Find Lyrics Online (Settings > General), off by default. When switched on, the title, artist, album and length of the song whose lyrics are opened are sent to LRCLIB (lrclib.net), a free public lyrics database.
- Network (Library tab): the user can enter the address of their own media server (DLNA) or WebDAV folder on their own network; the app reads music from it. These are the user's devices, not a service of ours.

5. REGIONAL DIFFERENCES
None. The app works the same in every region; only the interface language (Korean or English) differs.

6. REGULATED INDUSTRY / THIRD-PARTY MATERIAL
The app is not in a regulated industry. It contains and provides no third-party music or other protected content: it plays files the user already has, and the only music inside it is the sample music made by the developer. The source code is the developer's own, published under the 0BSD license; the open-source libraries it uses are listed in the app (Settings > About) with their licenses.
```

## 스크린샷

```bash
scripts/dev/appstore-shots.sh
```
큰 화면 시뮬레이터(기본 iPhone 18 Pro Max, `ONEUM_IOS_DEVICE`로 바꿈)에서 두 언어의 스크린샷을 `dist/appstore/ko`와 `dist/appstore/en`에 만듭니다. 1320×2868, 투명도 없는 PNG이고 저장소에는 넣지 않습니다. **App Store Connect의 필수 칸(중형 디스플레이)에는 `dist/appstore/medium/ko`와 `dist/appstore/medium/en`의 것(1206×2622, 큰 것을 줄인 그림)을 넣습니다.** 큰 것은 선택 칸("대형 디스플레이")에 넣을 수 있습니다. 음악과 가사는 지어낸 것입니다(`scripts/dev/gen-demo-library.py`와 스크립트 안의 가사). 처음에는 음악을 만드는 데 몇 분이 걸리고 `target/demo-library`에 남겨 다음에 다시 씁니다. ffmpeg와 Pillow가 필요합니다. **그 시뮬레이터에 깔려 있던 OnEum과 그 데이터는 지워집니다.**

| 파일 | 화면 |
|---|---|
| `01-albums.png` | 앨범 목록(아래에 재생 중인 곡의 막대) |
| `02-album.png` | 앨범 한 장의 곡 목록 |
| `03-player.png` | 플레이어(표지) |
| `04-lyrics.png` | 가사(부르는 줄이 켜진 상태) |
| `05-meters.png` | VU 미터와 스펙트럼, 그 아래 파일 형식과 소리의 경로 |
| `06-equalizer.png` | 이퀄라이저(재즈 프리셋) |
| `07-search.png` | 검색(한국어는 초성 `ㄷㅂ`, 영어는 `light`) |

알아 둘 것:
- **소리 없이 찍습니다.** 재생 중에는 앱 볼륨이 0입니다. 볼륨 막대가 보이는 플레이어와 가사는 곡을 멈춘 채 볼륨을 끝까지 올린 순간에 찍어서, 재생 단추가 ▶(일시정지 상태)로 나옵니다. 미터는 곡이 재생되어야 움직이므로 볼륨 0으로 찍고, 그래서 그 그림의 볼륨 막대는 0이고 경로에 "앱 볼륨"이 나옵니다.
- 미터 아래의 경로에 "리샘플링"이 나오는 것은 시뮬레이터의 출력이 48kHz이기 때문입니다.
- 화면 안의 자리는 폭 440포인트 화면(Pro Max)에 맞춰져 있습니다. 다른 크기의 기기에서는 스크립트가 멈춥니다.
- 영어 화면은 앱을 영어로 띄워 찍습니다(`-AppleLanguages`). 시뮬레이터의 언어는 바꾸지 않습니다.
- 스토어에 올릴 순서와 장수(한 크기에 10장까지)는 정하시면 됩니다. iPhone Duo용 그림은 만들지 않았습니다([APP_STORE.md](APP_STORE.md) 8절).
