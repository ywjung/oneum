//! Korean and English UI text. Every user-visible string goes through [`Strings`], so a
//! missing translation is a compile error.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum LanguageChoice {
    /// Follow the system language (Korean if preferred, otherwise English).
    #[default]
    System,
    Korean,
    English,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    Ko,
    En,
}

impl LanguageChoice {
    pub fn resolve(self, system_prefers_korean: bool) -> Lang {
        match self {
            Self::Korean => Lang::Ko,
            Self::English => Lang::En,
            Self::System if system_prefers_korean => Lang::Ko,
            Self::System => Lang::En,
        }
    }
}

macro_rules! strings {
    ($( $name:ident : $ko:expr, $en:expr; )*) => {
        /// All fixed UI strings.
        pub struct Strings { $( pub $name: &'static str, )* }
        pub static KO: Strings = Strings { $( $name: $ko, )* };
        pub static EN: Strings = Strings { $( $name: $en, )* };

        #[cfg(test)]
        impl Strings {
            /// Every string with its name, for tests over the whole table.
            fn all(&self) -> Vec<(&'static str, &'static str)> {
                vec![ $( (stringify!($name), self.$name), )* ]
            }
        }
    };
}

strings! {
    app_name: "OnEum", "OnEum";
    // Sidebar
    library: "라이브러리", "Library";
    all_songs: "모든 곡", "Songs";
    albums: "앨범", "Albums";
    artists: "아티스트", "Artists";
    favorites: "즐겨찾기", "Favorites";
    recently_played: "다시 듣기", "Listen Again";
    rediscover_played: "최근 재생", "Recently Played";
    rediscover_added: "최근 추가", "Recently Added";
    rediscover_most: "많이 들은 곡", "Most Played";
    rediscover_forgotten: "오래 안 들은 곡", "Not Heard Lately";
    empty_forgotten: "오래 안 들은 곡이 아직 없습니다", "Nothing has gone unheard for long yet";
    empty_forgotten_hint: "들은 지 30일이 지난 곡이 여기에 모입니다.", "Songs you have not played for 30 days gather here.";
    playlists: "플레이리스트", "Playlists";
    new_playlist: "새 플레이리스트", "New Playlist";
    // Columns
    col_title: "제목", "Title";
    col_artist: "아티스트", "Artist";
    col_album: "앨범", "Album";
    col_duration: "길이", "Time";
    col_format: "포맷", "Format";
    col_year: "연도", "Year";
    col_genre: "장르", "Genre";
    col_plays: "재생", "Plays";
    col_rating: "별점", "Rating";
    rating_none: "별점 없음", "No Rating";
    rating_1: "별 1개", "1 Star";
    rating_2: "별 2개", "2 Stars";
    rating_3: "별 3개", "3 Stars";
    rating_4: "별 4개", "4 Stars";
    rating_5: "별 5개", "5 Stars";
    col_added: "추가", "Added";
    col_last_played: "마지막 재생", "Last Played";
    columns_menu: "표시할 열", "Columns";
    genres: "장르", "Genres";
    empty_genres: "장르가 적힌 곡이 없습니다", "No songs with a genre";
    empty_genres_hint: "파일의 태그에 장르가 있으면 여기에 모입니다.", "Songs whose tags name a genre gather here.";
    go_to_album: "앨범으로 이동", "Go to Album";
    go_to_artist: "아티스트로 이동", "Go to Artist";
    queue_save: "저장", "Save";
    queue_save_hint: "지금 대기열을 플레이리스트로 저장합니다.", "Saves the queue as a playlist.";
    smart_playlist: "스마트 플레이리스트", "Smart Playlist";
    smart_save: "이 검색을 스마트 플레이리스트로 저장", "Save this search as a smart playlist";
    smart_name: "스마트 플레이리스트 이름", "Smart playlist name";
    smart_edit: "검색 조건 고치기", "Edit Search";
    smart_hint: "저장한 검색입니다. 조건에 맞는 곡이 그때그때 모입니다.", "A saved search: it holds whatever matches at the moment.";
    never: "—", "—";
    col_track_no: "#", "#";
    col_favorite: "즐겨찾기", "Favorite";
    // Top bar
    search_hint: "제목, 아티스트, 앨범 검색", "Search title, artist, album";
    search_label: "검색", "Search";
    clear_search: "검색어 지우기", "Clear search";
    show_queue: "재생 대기열", "Up Next";
    show_info: "곡 정보", "Song Info";
    show_meter: "VU 미터", "VU Meter";
    meter_left: "L", "L";
    meter_right: "R", "R";
    meter_left_name: "VU 미터, 왼쪽 채널", "VU meter, left channel";
    meter_right_name: "VU 미터, 오른쪽 채널", "VU meter, right channel";
    spectrum_name: "스펙트럼 표시", "Spectrum display";
    meter_note: "0 VU = −11 dBFS (RMS). 앱 볼륨과 상관없이 음악 자체의 레벨을 보여 줍니다.",
        "0 VU = −11 dBFS (RMS). Shows the level of the music itself, whatever the app volume.";
    settings: "설정", "Settings";
    back: "뒤로", "Back";
    // Transport
    play: "재생", "Play";
    pause: "일시정지", "Pause";
    next_track: "다음 곡", "Next";
    previous_track: "이전 곡", "Previous";
    shuffle: "셔플", "Shuffle";
    shuffle_on: "셔플 켜짐", "Shuffle On";
    shuffle_off: "셔플 끔", "Shuffle Off";
    // Media servers of the network
    network: "네트워크", "Network";
    past_errors: "지난 실행의 오류 기록", "Errors of Earlier Runs";
    past_errors_desc: "앱이 오류로 끝났을 때 남은 기록입니다. 문제를 알릴 때 이 내용을 함께 알려 주세요.", "What was written down when the app ended with an error. Please pass it on when you report a problem.";
    meter_failed: "VU 미터를 그리다 오류가 났습니다. 아래 내용을 알려 주세요.", "The VU meter could not be drawn. Please report what it says below.";
    graphics_restarted: "화면을 그리는 장치를 잃어 앱을 다시 시작했습니다.", "The graphics device was lost, so the app started anew.";
    graphics_lost_again: "화면을 그리는 장치를 다시 잃어 앱을 닫습니다. 기록은 설정 → 정보에 있습니다.", "The graphics device was lost again: OnEum closes. See Settings → About.";
    graphics_lost_title: "화면을 더 그릴 수 없습니다", "Nothing More Can Be Drawn";
    graphics_lost_body: "화면을 그리는 장치를 잃었습니다. 재생 목록과 듣던 위치는 저장했습니다. 앱을 다시 열어 주세요.", "The graphics device was lost. The queue and the position are saved. Please open the app again.";
    built: "빌드", "built";
    copy_record: "기록 복사", "Copy the Record";
    copy_record_desc: "버전, 그리는 장치, 지난 실행이 끝난 이유, 로그의 경고와 오류, 메모리를 한꺼번에 복사합니다. 문제를 알릴 때 붙여 넣어 주세요.", "Copies the version, what the screen is drawn with, how earlier runs ended, the warnings and errors of the log and the memory, all at once. Paste it when you report a problem.";
    copy_short: "복사", "Copy";
    copied: "복사했습니다.", "Copied.";
    graphics_gave_up: "고른 그리기 방식으로는 시작하지 못해 자동으로 되돌렸습니다.", "The app did not start with the chosen way of drawing: it is back on Automatic.";
    graphics_section: "화면 그리기", "Drawing the Screen";
    graphics_choice: "그리는 방식", "Drawn Through";
    graphics_choice_desc: "바꾸면 앱이 다시 시작됩니다. 자동은 폰의 기본 방식을 쓰고, 그 방식에서 화면을 그리는 장치를 잃으면 다른 방식으로 바꿉니다. glow는 OpenGL ES를 다른 그리기 라이브러리로 쓰는 시험용입니다.", "Changing it starts the app anew. Automatic uses the phone's first choice, and the other one after the graphics device was lost on that. glow is a trial: OpenGL ES through another drawing library.";
    graphics_auto: "자동", "Automatic";
    graphics_now: "지금", "Now";
    memory_section: "메모리", "Memory";
    memory_desc: "앱이 지금 쓰는 메모리입니다. 문제를 알릴 때 이 숫자도 함께 알려 주세요.", "What the app holds of memory right now. Please pass these numbers on, too, when you report a problem.";
    network_open: "네트워크의 미디어 서버", "Media Servers on the Network";
    network_search: "다시 찾기", "Search Again";
    network_searching: "서버를 찾는 중…", "Looking for servers…";
    network_add: "주소로 추가…", "Add by Address…";
    network_add_title: "미디어 서버의 주소", "A Media Server's Address";
    network_add_hint: "192.168.0.10 또는 http://…/rootDesc.xml", "192.168.0.10 or http://…/rootDesc.xml";
    network_add_failed: "그 주소에서 미디어 서버를 찾지 못했습니다.", "No media server was found at that address.";
    network_saved: "추가한 서버", "Added";
    network_found: "찾은 서버", "Found on the Network";
    network_none: "찾은 미디어 서버가 없습니다. 주소로 추가해 보세요.", "No media server was found. Try adding one by its address.";
    network_forget: "목록에서 지우기", "Remove from the List";
    folder_add: "네트워크 폴더 추가…", "Add a Network Folder…";
    folder_list: "네트워크 폴더", "Network Folders";
    folder_add_title: "네트워크 폴더 (WebDAV)", "A Network Folder (WebDAV)";
    folder_address_hint: "주소: https://nas.local:5006/music/", "Address: https://nas.local:5006/music/";
    folder_name_hint: "이름 (비워 두면 서버 이름)", "Name (the server's, if left empty)";
    folder_user_hint: "사용자 이름 (없으면 비워 둠)", "User name (empty if none is asked for)";
    folder_password_hint: "비밀번호", "Password";
    folder_password_note: "비밀번호는 이 기기의 비밀번호 보관소(키체인 · 자격 증명 관리자 · Android Keystore)에만 저장됩니다. 설정 파일과 로그에는 남지 않습니다.", "The password is kept only in this device's own store for passwords (the keychain, the Credential Manager, the Android Keystore). It is in neither the settings file nor the log.";
    folder_plain_warning: "http:// 주소는 암호화되지 않습니다. 서버가 Digest 인증을 쓰지 않으면 비밀번호가 그대로 전송되고, 같은 네트워크의 다른 기기가 볼 수 있습니다. 가능하면 https:// 주소를 쓰세요.", "An http:// address is not encrypted. Unless the server uses Digest authentication the password is sent as it is, where other devices on the network can see it. Use an https:// address if you can.";
    folder_trying: "서버에 물어보는 중…", "Asking the server…";
    folder_connect: "연결", "Connect";
    folder_trust: "신뢰하고 연결", "Trust and Connect";
    folder_untrusted: "서버의 인증서를 확인해 줄 인증 기관이 없습니다(NAS가 스스로 만든 인증서일 수 있습니다). 인증서의 지문:", "No authority vouches for the server's certificate (a NAS may have made its own). The certificate's fingerprint:";
    folder_untrusted_how: "NAS의 관리 화면에 나오는 인증서 지문(SHA-256)과 같은지 확인하세요. 같으면 신뢰해도 됩니다. 신뢰하면 이 인증서일 때만 연결하고, 다른 인증서가 나오면 다시 거부합니다.", "Compare it with the certificate fingerprint (SHA-256) your NAS shows in its own settings. If they are the same, it can be trusted. Once trusted, the app connects only while the server shows this certificate, and refuses any other.";
    folder_refused: "서버가 이 이름과 비밀번호를 받아들이지 않습니다.", "The server does not accept this name and password.";
    folder_bad_address: "폴더의 주소가 아닙니다. http:// 또는 https:// 로 시작하는 주소를 넣으세요.", "That is no address of a folder. Enter one that begins with http:// or https://.";
    folder_unreachable: "그 주소의 폴더를 읽지 못했습니다.", "The folder at that address could not be read.";
    folder_moved: "서버가 이 주소를 다른 주소로 넘기는데, 그 주소는 따라가지 않습니다. https:// 로 시작하는 주소(와 그 포트)를 넣어 보세요.", "The server sends this address on to another one, which is not followed. Try the address that begins with https:// (and its port).";
    folder_not_found: "서버에 그 폴더가 없습니다. 주소의 경로를 확인하세요.", "The server has no folder at that path. Check the path of the address.";
    folder_not_webdav: "이 주소는 공유 폴더(WebDAV)가 아닙니다. NAS의 WebDAV 주소를 포트와 폴더 경로까지 넣으세요.", "That address is not a shared folder (WebDAV). Enter your NAS's WebDAV address, with its port and the folder's path.";
    folder_not_tls: "이 포트는 암호화된 연결(https)을 받지 않습니다. 주소를 http:// 로 시작하게 적거나, 서버의 https 포트를 넣으세요.", "This port does not take encrypted connections (https). Begin the address with http://, or use the server's https port.";
    folder_no_password: "이 폴더의 비밀번호를 시스템의 보관소에서 읽지 못했습니다. 기기의 잠금을 풀고 다시 열어 보세요. 계속 안 되면 목록에서 지우고 다시 추가하세요.",
        "The password of this folder could not be read from the system's store. Unlock the device and open the folder again; if that does not help, remove it from the list and add it again.";
    folder_full: "네트워크 폴더는 32개까지 둘 수 있습니다. 쓰지 않는 폴더를 목록에서 지운 뒤 추가하세요.",
        "Up to 32 network folders are kept. Remove one you do not use from the list, then add this one.";
    folder_refused_later: "서버가 저장된 이름과 비밀번호를 받아들이지 않습니다. 목록에서 지우고 다시 추가하세요.", "The server no longer accepts the kept name and password. Remove the folder from the list and add it again.";
    folder_untrusted_later: "서버의 인증서를 믿을 수 없어 연결하지 않았습니다(기간이 지났거나 다른 이름의 인증서입니다). 목록에서 지우고 다시 추가하면 인증서를 확인할 수 있습니다.", "The server's certificate cannot be trusted, so nothing was sent to it (it has run out, or it is for another name). Remove the folder from the list and add it again to look at the certificate.";
    folder_changed_certificate: "서버가 전에 신뢰한 것과 다른 인증서를 보여 연결하지 않았습니다. 인증서를 바꾼 것이 맞다면 목록에서 지우고 다시 추가하세요.", "The server shows another certificate than the one that was trusted, so nothing was sent to it. If the certificate was replaced on purpose, remove the folder from the list and add it again.";
    network_reload: "다시 읽기", "Reload";
    network_failed: "서버에서 읽지 못했습니다.", "Could not read from the server.";
    network_empty: "비어 있습니다.", "Nothing here.";
    notice_not_for_network: "네트워크 서버의 곡에는 쓸 수 없는 기능입니다.", "Not available for songs of a media server.";
    repeat_off: "반복 끔", "Repeat Off";
    repeat_all: "전체 반복", "Repeat All";
    repeat_one: "한 곡 반복", "Repeat One";
    volume: "볼륨", "Volume";
    time_remaining_hint: "남은 시간으로 보기", "Show time remaining";
    time_total_hint: "전체 길이로 보기", "Show total time";
    mute: "음소거", "Mute";
    unmute: "음소거 해제", "Unmute";
    play_album_hint: "앨범 재생", "Play Album";
    undo: "실행 취소", "Undo";
    notice_removed_from_playlist: "플레이리스트에서 제거했습니다.", "Removed from the playlist.";
    startup_failed_title: "OnEum을 시작할 수 없습니다", "OnEum could not start";
    startup_failed_body: "라이브러리를 열지 못했습니다. 디스크 공간과 폴더 권한을 확인한 뒤 다시 실행해 주세요.", "The library could not be opened. Check free disk space and folder permissions, then start OnEum again.";
    startup_failed_log: "자세한 내용(로그)", "Details (log)";
    startup_graphics_body: "창을 그릴 그래픽 장치를 준비하지 못했습니다. 그래픽 드라이버를 업데이트하거나, 가상 머신에서 실행 중이라면 3D 가속을 켠 뒤 다시 실행해 주세요.", "No graphics device could be set up to draw the window. Update the graphics driver or, in a virtual machine, turn on 3D acceleration, then start OnEum again.";
    startup_window_body: "창을 열지 못했습니다.", "The window could not be opened.";
    crashed_title: "OnEum에 오류가 생겨 종료됩니다", "OnEum ran into an error and will quit";
    crashed_body: "예기치 못한 오류입니다. 아래 내용은 로그에도 남습니다.", "This was not expected. What follows is also in the log.";
    alert_copy_hint: "이 창에서 Ctrl+C를 누르면 내용을 복사할 수 있습니다.", "Press Ctrl+C in this window to copy its text.";
    seek_bar: "재생 위치", "Playback position";
    nothing_playing: "재생 중인 곡 없음", "Not Playing";
    drop_hint: "여기에 놓아 음악 추가", "Drop to add music";
    // Context menu
    play_now: "재생", "Play";
    play_next: "다음에 재생", "Play Next";
    add_to_queue: "대기열 끝에 추가", "Add to Queue";
    add_to_playlist: "플레이리스트에 추가", "Add to Playlist";
    add_favorite: "즐겨찾기에 추가", "Add to Favorites";
    remove_favorite: "즐겨찾기에서 제거", "Remove from Favorites";
    show_in_finder: if cfg!(windows) {
            "탐색기에서 보기"
        } else if cfg!(target_os = "android") {
            "파일 앱에서 보기"
        } else {
            "Finder에서 보기"
        },
        if cfg!(windows) {
            "Show in Explorer"
        } else if cfg!(target_os = "android") {
            "Show in Files"
        } else {
            "Show in Finder"
        };
    remove_from_playlist: "플레이리스트에서 제거", "Remove from Playlist";
    remove_from_library: "라이브러리에서 제거", "Remove from Library";
    song_info: "곡 정보", "Song Info";
    move_up: "위로 이동", "Move Up";
    move_down: "아래로 이동", "Move Down";
    move_to_top: "맨 위로 이동", "Move to Top";
    remove_from_queue: "대기열에서 제거", "Remove from Queue";
    clear_queue: "대기열 비우기", "Clear Queue";
    rename: "이름 변경", "Rename";
    delete: "삭제", "Delete";
    export_m3u8: "M3U8로 내보내기…", "Export as M3U8…";
    import_m3u8: "플레이리스트 가져오기…", "Import Playlist…";
    play_album: "앨범 재생", "Play Album";
    shuffle_album: "셔플 재생", "Shuffle";
    // Empty states
    empty_library_title: "음악을 추가해 보세요", "Add your music";
    empty_library_body: "음악이 들어 있는 폴더를 고르면 바로 들을 수 있습니다.\n원본 파일은 읽기만 하며 바꾸지 않습니다.",
        "Pick a folder that contains your music and start listening.\nYour files are only read, never changed.";
    add_music_folder: "음악 폴더 추가", "Add Music Folder";
    open_files: "음악 파일 열기", "Open Music Files";
    empty_search: "검색 결과가 없습니다", "No results";
    empty_search_hint: "다른 검색어를 입력해 보세요.", "Try a different search.";
    empty_favorites: "즐겨찾기한 곡이 없습니다", "No favorites yet";
    empty_favorites_hint: "곡을 오른쪽 클릭해 즐겨찾기에 추가하세요.", "Right-click a song to add it to Favorites.";
    empty_recent: "아직 재생한 곡이 없습니다", "Nothing played yet";
    empty_playlist: "플레이리스트가 비어 있습니다", "This playlist is empty";
    empty_playlist_hint: "곡을 오른쪽 클릭해 이 플레이리스트에 추가하세요.", "Right-click a song to add it to this playlist.";
    empty_queue: "대기열이 비어 있습니다", "The queue is empty";
    empty_queue_hint: "곡을 더블클릭하면 재생이 시작됩니다.", "Double-click a song to start playing.";
    no_selection: "선택한 곡이 없습니다", "No song selected";
    unknown_artist: "알 수 없는 아티스트", "Unknown Artist";
    unknown_album: "알 수 없는 앨범", "Unknown Album";
    // Scan
    scanning: "스캔 중", "Scanning";
    // Settings
    settings_general: "일반", "General";
    settings_library: "라이브러리", "Library";
    settings_playback: "재생", "Playback";
    settings_about: "정보", "About";
    language: "언어", "Language";
    language_system: "시스템 설정 따르기", "Follow System";
    theme: "화면 모드", "Appearance";
    theme_system: "시스템 설정 따르기", "Follow System";
    theme_system_short: "시스템", "System";
    theme_light: "밝게", "Light";
    theme_dark: "어둡게", "Dark";
    music_folders: "음악 폴더", "Music Folders";
    no_music_folders: "추가한 음악 폴더가 없습니다.", "No music folders added.";
    remove_folder: "제거", "Remove";
    rescan: "다시 스캔", "Rescan";
    folder_offline: "연결 끊김", "Disconnected";
    show_format_column: "곡 목록에 포맷 열 표시", "Show Format column in song lists";
    output_device: "출력 장치", "Output Device";
    output_system_default: "시스템 기본 장치", "System Default";
    output_missing: "(연결되지 않음)", "(not connected)";
    replaygain: "음량 보정 (ReplayGain)", "Volume Leveling (ReplayGain)";
    rg_off: "끔", "Off";
    rg_track: "곡 기준", "Track";
    rg_album: "앨범 기준", "Album";
    rg_preamp: "프리앰프", "Pre-amp";
    rg_prevent_clipping: "클리핑 방지", "Prevent clipping";
    rg_hint: "파일에 든 ReplayGain 태그를 씁니다. 태그가 없는 곡은 ‘태그 없는 곡 분석’으로 분석해 두면 같은 방식으로 보정합니다. 분석 결과는 앱 안에만 저장하고 음악 파일은 바꾸지 않습니다.",
        "Uses the ReplayGain tags in your files. Songs without tags are leveled the same way once they have been analyzed here. The results are kept inside the app; your music files are not changed.";
    rg_measure: "태그 없는 곡 분석", "Analyze Songs Without Tags";
    rg_measure_start: "분석 시작", "Analyze";
    rg_measure_stop: "중지", "Stop";
    rg_measure_none: "분석할 곡이 없습니다.", "Nothing to analyze.";
    rg_measured: "앱이 측정한 값", "Values Measured by the App";
    rg_forget: "지우기", "Clear";
    info_measured: "앱 측정", "measured by the app";
    settings_section_display: "화면", "Display";
    settings_section_output: "출력", "Output";
    settings_shortcuts: "단축키", "Shortcuts";
    shortcuts_group_window: "창과 앱", "Window and App";
    shortcut_play_selected: "선택한 곡 재생", "Play the selected song";
    shortcut_move_selection: "선택 이동 / 범위 선택", "Move the selection / extend it";
    shortcut_remove: "플레이리스트·대기열에서 제거", "Remove from the playlist or queue";
    shortcut_reorder_queue: "대기열에서 순서 바꾸기", "Reorder in the queue";
    shortcut_clear_search: "검색어 지우기", "Clear the search";
    shortcuts_note: "화살표·Return·Delete는 마지막으로 클릭한 목록(곡 목록 또는 대기열)에 적용됩니다.",
        "Arrows, Return and Delete apply to the list you clicked last (songs or queue).";
    settings_section_lists: "곡 목록", "Song Lists";
    language_desc: "메뉴와 화면에 쓰는 언어입니다.", "The language of menus and the window.";
    theme_desc: "밝은 화면과 어두운 화면을 고릅니다.", "Choose a light or a dark look.";
    show_format_title: "포맷 열 표시", "Show Format Column";
    show_format_desc: "곡 목록에 코덱(FLAC, MP3 등)을 함께 보여 줍니다.", "Adds the codec (FLAC, MP3, …) to song lists.";
    music_folders_hint: "이 폴더들의 음악이 라이브러리에 나타납니다. 파일은 읽기만 하고 바꾸지 않습니다.",
        "Music in these folders appears in your library. Files are only read, never changed.";
    folder_never_scanned: "아직 스캔하지 않음", "Not scanned yet";
    output_device_desc_idle: "재생을 시작하면 사용 중인 장치가 표시됩니다.", "The device in use is shown once playback starts.";
    output_hint: "선택한 장치가 분리되면 다른 장치로 넘기지 않고 일시정지합니다.",
        "If the device is disconnected, playback pauses instead of moving to another device.";
    settings_section_quality: "출력 품질", "Output Quality";
    out_match_rate: "곡의 샘플레이트로 출력", "Match the Song's Sample Rate";
    out_match_rate_desc: "장치를 곡의 샘플레이트에 맞춰 리샘플링 없이 내보냅니다.",
        "Runs the device at each song's own rate, so nothing is resampled.";
    out_exclusive: "독점 출력", "Exclusive Access";
    out_exclusive_desc: "재생하는 동안 이 앱만 장치를 씁니다. 다른 앱의 소리는 이 장치로 나오지 않습니다.",
        "Only OnEum uses the device while it plays. Other apps cannot play through it.";
    out_memory: "메모리 재생", "Memory Playback";
    out_memory_desc: "재생할 파일을 미리 메모리에 올려 디스크 상태와 상관없이 이어 재생합니다.",
        "Loads each file into memory ahead of time, so playback does not depend on the disk.";
    out_macos_only: "macOS에서만 쓸 수 있습니다.", "Available on macOS only.";
    out_upsample: "업샘플링", "Upsampling";
    out_upsample_desc: "장치가 낼 수 있는 가장 높은 정수배(8·4·2배, 384kHz까지)로 곡을 올려 내보냅니다.",
        "Raises each song to the highest whole multiple of its sample rate the device offers (×8, ×4 or ×2, up to 384 kHz).";
    phone_upsample_desc: "곡의 샘플레이트의 정수배로 내보내 달라고 iPhone에 요청하고, 받아 주면 그 레이트로 올립니다.",
        "Asks the iPhone for a whole multiple of each song's sample rate, and raises the song to it when it agrees.";
    out_filter: "업샘플링 필터", "Upsampling Filter";
    out_filter_desc: "출력이 곡의 정수배 샘플레이트일 때 곡을 올리는 필터입니다.",
        "The filter a song is raised with when the output runs at a whole multiple of its sample rate.";
    filter_sharp: "선명", "Sharp";
    filter_slow: "완만", "Slow";
    filter_minimum: "최소 위상", "Minimum Phase";
    out_filter_hint: "선명: 대역 끝까지 평탄하고 그 위는 남기지 않지만, 갑작스러운 소리의 앞뒤로 길게 울립니다(지금까지의 변환). 완만: 울림이 1ms 안쪽으로 짧고, 18kHz부터 천천히 내려가며 대역 바로 위의 소리가 조금 남습니다. 최소 위상: 20kHz까지 평탄하고, 울림이 소리 뒤에만 있습니다.",
        "Sharp: flat to the top of the band with nothing left above it, but it rings for a while before and after anything sudden (the conversion used so far). Slow: rings for under a millisecond, rolls off gently from 18 kHz and leaves a little just above the band. Minimum Phase: flat to 20 kHz, and all of its ringing comes after the sound.";
    out_quality_hint: "장치 설정을 바꿨다면 재생이 끝나거나 30초 넘게 멈춰 있을 때 원래대로 되돌립니다. 샘플레이트가 다른 곡으로 넘어갈 때는 장치를 바꾸는 동안 잠깐 끊깁니다.",
        "Changed device settings are put back when playback ends or stays paused for 30 seconds. Moving to a song with another sample rate pauses briefly while the device switches.";
    rg_mode: "기준", "Mode";
    rg_mode_desc: "곡마다 다른 음량을 비슷하게 맞춥니다.", "Evens out loudness differences between songs.";
    rg_preamp_desc: "보정한 곡의 음량을 더 올리거나 내립니다.", "Raises or lowers adjusted songs further.";
    rg_clip_desc: "보정 때문에 소리가 깨지지 않게 이득을 제한합니다.", "Limits the gain so adjusted songs never distort.";
    about_guide_desc: "기능과 단축키를 앱 안에서 바로 봅니다.", "Features and shortcuts, right here in the app.";
    log_folder: "로그 폴더", "Log Folder";
    log_folder_desc: "문제가 생겼을 때 확인하는 기록입니다.", "Records to look at when something goes wrong.";
    data_folder: "데이터 폴더", "Data Folder";
    data_folder_desc: "라이브러리 데이터베이스와 설정이 있는 곳입니다.", "Where the library database and settings live.";
    close_settings: "설정 닫기", "Close Settings";
    close_guide: "사용 안내 닫기", "Close the User Guide";
    open_guide: "열기", "Open";
    about_body: "로컬 음악 파일을 위한 오프라인 플레이어.\n계정, 서버, 사용량 수집이 없습니다.",
        "An offline player for your local music files.\nNo accounts, no servers, no analytics.";
    version: "버전", "Version";
    licenses: "오픈 소스 라이선스", "Open Source Licenses";
    licenses_desc: "OnEum을 이루는 오픈 소스 구성 요소와 그 라이선스입니다.", "The open source components OnEum is built from, and their licenses.";
    licenses_show: "보기", "Show";
    licenses_hide: "접기", "Hide";
    close: "닫기", "Close";
    // Dialogs
    cancel: "취소", "Cancel";
    ok: "확인", "OK";
    confirm_remove_folder_title: "이 폴더를 라이브러리에서 제거할까요?", "Remove this folder from the library?";
    confirm_remove_folder_body: "이 폴더의 곡이 라이브러리와 플레이리스트에서 빠집니다.\n원본 파일은 삭제되지 않습니다.",
        "Its songs are removed from the library and from playlists.\nThe original files are not deleted.";
    confirm_delete_playlist_title: "플레이리스트를 삭제할까요?", "Delete this playlist?";
    confirm_delete_playlist_body: "플레이리스트만 삭제됩니다. 원본 파일은 삭제되지 않습니다.",
        "Only the playlist is deleted. The original files are not deleted.";
    confirm_remove_tracks_title: "선택한 곡을 라이브러리에서 제거할까요?", "Remove the selected songs from the library?";
    confirm_remove_tracks_body: "라이브러리와 플레이리스트에서만 빠집니다. 원본 파일은 삭제되지 않습니다. 폴더를 다시 스캔해도 돌아오지 않으며, 설정의 \"제거한 곡 되돌리기\"로 다시 넣을 수 있습니다.",
        "They are only removed from the library and playlists. The original files are not deleted. A scan of their folder does not bring them back; \"Bring Back Removed Songs\" in the settings does.";
    restore_removed: "제거한 곡 되돌리기", "Bring Back Removed Songs";
    restore_removed_desc: "라이브러리에서 제거한 곡은 파일이 남아 있어도 스캔이 다시 넣지 않습니다. 그 곡들을 모두 다시 넣습니다.", "Songs removed from the library are not taken in again by a scan, though their files are still there. This takes all of them in again.";
    restore_removed_button: "되돌리기", "Bring Back";
    playlist_name: "플레이리스트 이름", "Playlist name";
    untitled_playlist: "새 플레이리스트", "New Playlist";
    // Info panel
    info_file: "파일", "File";
    info_format: "포맷", "Format";
    info_sample_rate: "샘플레이트", "Sample rate";
    info_channels: "채널", "Channels";
    info_bit_depth: "비트 깊이", "Bit depth";
    info_bitrate: "비트레이트", "Bitrate";
    info_size: "크기", "Size";
    info_year: "연도", "Year";
    info_genre: "장르", "Genre";
    info_track: "트랙", "Track";
    info_plays: "재생 횟수", "Plays";
    info_album_artist: "앨범 아티스트", "Album artist";
    info_replaygain: "ReplayGain", "ReplayGain";
    info_none: "없음", "None";
    info_source: "소스", "Source";
    info_output: "출력", "Output";
    info_exclusive: "독점", "exclusive";
    info_processing: "처리", "Processing";
    info_proc_none: "없음", "None";
    info_proc_resample: "리샘플링", "Resampling";
    info_proc_replaygain: "음량 보정", "Volume leveling";
    info_proc_volume: "앱 볼륨", "App volume";
    info_proc_channels: "채널 변환", "Channel mapping";
    info_proc_dither: "디더", "Dither";
    info_proc_dsd: "DSD → PCM 변환", "DSD to PCM";
    info_proc_limiter: "피크 보호", "Peak protection";
    info_proc_eq: "이퀄라이저", "Equalizer";
    info_signal_path: "신호 경로", "Signal Path";
    info_lyrics: "가사", "Lyrics";
    lyrics_seek_hint: "누르면 이 줄부터 재생합니다.", "Click to play from this line.";
    lyrics_found_online: "인터넷에서 찾은 가사 (LRCLIB)", "Lyrics found online (LRCLIB)";
    lyrics_seeking: "가사를 찾는 중…", "Looking for lyrics…";
    online_lyrics_row: "인터넷에서 가사 찾기", "Find Lyrics Online";
    online_lyrics_desc: "파일에 싱크 가사가 없는 곡의 가사를 LRCLIB(lrclib.net)에서 찾습니다.",
        "Looks on LRCLIB (lrclib.net) for the lyrics of songs whose file has none that follow the song.";
    online_lyrics_hint: "꺼져 있으면 가사를 인터넷에서 찾지 않습니다. 켜면 곡 정보나 가사 화면에 곡이 보일 때 그 곡의 제목·아티스트·앨범·길이만 보내고, 받은 가사는 이 기기에 보관해 같은 곡을 다시 묻지 않습니다. 파일에 싱크 가사가 있는 곡은 묻지 않습니다.",
        "While this is off, lyrics are never looked for on the internet. While it is on, a song's title, artist, album and length are sent when the song is shown in the song info or the lyrics view, and nothing else; what comes back is kept on this device, so the same song is not asked for twice. Songs whose file has synced lyrics are not asked for.";
    info_credits: "크레딧", "Credits";
    role_composer: "작곡", "Composer";
    role_lyricist: "작사", "Lyricist";
    role_conductor: "지휘", "Conductor";
    role_arranger: "편곡", "Arranger";
    role_performer: "연주", "Performer";
    role_producer: "프로듀서", "Producer";
    role_engineer: "엔지니어", "Engineer";
    role_mixer: "믹싱", "Mixing";
    role_remixer: "리믹스", "Remixer";
    info_work: "작품", "Work";
    info_movement: "악장", "Movement";
    info_label: "레이블", "Label";
    info_catalog: "카탈로그 번호", "Catalog no.";
    info_recorded: "녹음", "Recorded";
    info_released: "발매", "Released";
    info_versions: "이 곡의 다른 버전", "Other Versions of This Song";
    info_versions_hint: "누르면 그 버전의 정보를 봅니다.", "Click to see that version.";
    path_untouched: "원본 그대로", "Unchanged";
    path_adjusted: "사용자 조정", "Adjusted";
    path_converted: "변환", "Converted";
    path_converted_adjusted: "변환 · 사용자 조정", "Converted · Adjusted";
    path_untouched_hint: "앱은 파일에서 읽은 샘플을 바꾸지 않고 출력 장치로 보냅니다. 그 뒤의 구간(운영체제, 드라이버, DAC)은 앱이 확인할 수 없습니다.",
        "The app passes the samples read from the file to the output device unchanged. What happens after that (the system, the driver, the DAC) the app cannot see.";
    path_adjusted_hint: "사용자가 켠 기능(음량 보정, 이퀄라이저, 앱 볼륨)이 소리를 바꾸고 있습니다. 끄면 원본 그대로 나갑니다.",
        "Features you switched on (volume leveling, the equalizer, the app volume) are changing the sound. Without them it goes out unchanged.";
    path_converted_hint: "이 장치로 내보내기 위해 변환하고 있습니다(샘플레이트, 채널, 비트 깊이). 설정 → 재생의 \"곡의 샘플레이트로 출력\"을 켜면 샘플레이트 변환을 피할 수 있습니다.",
        "The audio is being converted for this device (sample rate, channels, bit depth). \"Match the song's sample rate\" in Settings → Playback avoids the rate conversion.";
    path_channels_unit: "채널", "ch";
    path_muted: "음소거", "muted";
    // Crossfeed
    crossfeed: "헤드폰", "Headphones";
    crossfeed_row: "크로스피드", "Crossfeed";
    crossfeed_desc: "한쪽 채널의 저음을 반대쪽에도 조금 섞어, 스피커로 듣는 것처럼 만듭니다.", "Mixes a little of each channel's low end into the other, as speakers do.";
    crossfeed_light: "약하게", "Light";
    crossfeed_medium: "보통", "Medium";
    crossfeed_strong: "강하게", "Strong";
    crossfeed_hint: "악기가 한쪽 귀에만 몰린 옛 녹음을 헤드폰으로 들을 때 피로를 줄여 줍니다. 스피커로 들을 때는 꺼 두세요. 켜면 가운데 소리의 고음이 최대 2dB 낮아집니다.",
        "Eases listening on headphones to older recordings with instruments hard on one side. Leave it off for speakers. While on, the highs of what is in the centre are up to 2 dB lower.";
    info_proc_crossfeed: "크로스피드", "Crossfeed";
    crossfade: "크로스페이드", "Crossfade";
    crossfade_row: "곡이 바뀔 때 겹치기", "Overlap Between Songs";
    crossfade_desc: "앞 곡의 끝과 다음 곡의 처음을 섞어 넘어갑니다.", "Blends the end of a song into the beginning of the next.";
    crossfade_2: "2초", "2 s";
    crossfade_4: "4초", "4 s";
    crossfade_6: "6초", "6 s";
    crossfade_10: "10초", "10 s";
    crossfade_hint: "곡이 스스로 다음 곡으로 넘어갈 때만 겹칩니다. 같은 앨범의 곡 사이는 지금처럼 끊김 없이 이어지고, 직접 고른 곡은 바로 시작합니다. 겹치는 길이는 두 곡 길이의 절반을 넘지 않습니다. 켜면 겹치는 구간의 소리는 원본과 달라집니다. DSD 곡의 끝과, 곡의 샘플레이트로 출력할 때 장치의 샘플레이트가 바뀌는 자리에서는 겹치지 않습니다.", "Songs overlap only where one follows another by itself. Songs of one album go on without a gap as before, and a song you choose starts at once. The overlap is never longer than half of either song. While on, the sound differs from the original where songs overlap. The end of a DSD song is not overlapped, and neither is a place where the device changes its sample rate (output at the song's sample rate).";
    info_proc_crossfade: "크로스페이드", "Crossfade";
    speed: "재생 속도", "Playback Speed";
    speed_row: "곡의 빠르기", "Speed of the Songs";
    speed_desc: "음높이는 그대로 두고 빠르기만 바꿉니다.", "Faster or slower, at the pitch the song has.";
    speed_hint: "1×가 아닐 때의 소리는 원본을 잘게 나눠 다시 이어 붙인 것입니다(원본 그대로가 아닙니다). 말소리에 잘 맞고, 음악에서는 길게 끄는 음이나 타악기가 조금 다르게 들릴 수 있습니다. 재생 위치와 남은 시간은 곡의 시간 그대로이고, 곡이 그만큼 빨리 또는 천천히 지나갑니다.", "At any speed but 1× the sound is the original cut into short pieces and joined again (not the original as it is). It suits speech well; in music, long notes and drums may sound a little different. Position and time left stay in the song's own time: the song passes that much faster or slower.";
    info_proc_speed: "재생 속도", "Playback speed";
    speed_cancel_hint: "재생 속도. 누르면 1×로 돌아갑니다.", "Playback speed. Click for 1× again.";
    info_proc_correction: "헤드폰 보정", "Headphone correction";
    correction: "헤드폰 보정", "Headphone Correction";
    correction_row: "보정 프로필", "Correction Profile";
    correction_none_desc: "쓰는 헤드폰의 프로필을 고르면 그 주파수 특성을 바로잡습니다.", "Choose your headphone's profile to correct its frequency response.";
    correction_file_row: "보정값 파일", "Correction File";
    correction_file_desc: "목록에 없는 헤드폰은 AutoEq의 ParametricEQ.txt 같은 Equalizer APO 형식의 파일을 불러옵니다.", "For a headphone that is not listed: imports a file in Equalizer APO format, such as AutoEq's ParametricEQ.txt.";
    correction_import: "불러오기…", "Import…";
    correction_edit_row: "화면에서 만들기", "Make One on Screen";
    correction_edit_desc: "밴드의 종류·주파수·게인·Q를 직접 정합니다. 고른 프로필에서 시작해 고칠 수도 있습니다.", "Set the kind, frequency, gain and Q of each band yourself, or start from the selected profile and change it.";
    correction_new: "새로 만들기", "New";
    correction_edit: "편집", "Edit";
    correction_editor: "보정 프로필 편집", "Editing a Correction Profile";
    correction_name: "이름", "Name";
    correction_preamp: "프리앰프", "Preamp";
    correction_add_band: "밴드 추가", "Add Band";
    correction_remove_band: "밴드 지우기", "Remove Band";
    correction_kind_peak: "피크", "Peak";
    correction_kind_low: "로우 셸프", "Low Shelf";
    correction_kind_high: "하이 셸프", "High Shelf";
    correction_save: "저장", "Save";
    correction_new_name: "내 보정", "My Correction";
    correction_copy_mark: "내 값", "mine";
    correction_edit_hint: "바꾸는 대로 소리에 적용됩니다. 저장하면 \"불러온 프로필\" 묶음에 들어가고, 취소하면 전에 고른 프로필로 돌아갑니다. 밴드를 올려 소리가 원래보다 12dB 넘게 커지면 프리앰프가 그만큼 스스로 내려갑니다(곡선에 그대로 보입니다). 밴드는 32개까지입니다.", "What you change is heard at once. Saving puts the profile among the imported ones; cancelling goes back to the profile that was selected before. Where bands would make the sound more than 12 dB louder than it was, the preamp gives way by as much (the curve shows it). Up to 32 bands.";
    correction_group_imported: "불러온 프로필", "Imported";
    correction_group_builtin: "기본 프로필 (AutoEq)", "Built in (AutoEq)";
    correction_choose: "보정값 파일 선택", "Choose a Correction File";
    room: "룸 보정", "Room Correction";
    room_row: "룸 보정 적용", "Apply Room Correction";
    room_none_desc: "방과 스피커를 측정해 만든 보정 필터(임펄스 응답)를 불러오면 모든 소리에 적용합니다.",
        "Import a correction filter (an impulse response) made by measuring your room and speakers, and it is applied to everything.";
    room_file_row: "보정 필터", "Correction Filter";
    room_file_desc: "REW · DRC 등이 내보낸 임펄스 응답 파일(WAV · FLAC, 모노 또는 스테레오).",
        "An impulse response file as REW, DRC and the like export it (WAV or FLAC, mono or stereo).";
    room_choose: "보정 필터 선택", "Choose a Correction Filter";
    room_filter_text: "소리 파일", "Sound files";
    room_err_read: "보정 필터로 쓸 수 없는 파일입니다.", "This file cannot be used as a correction filter.";
    room_err_long: "보정 필터가 너무 깁니다(6초까지).", "The correction filter is too long (up to 6 seconds).";
    room_err_keep: "보정 필터를 저장하지 못했습니다.", "The correction filter could not be kept.";
    room_lost: "보관해 둔 룸 보정 필터를 읽을 수 없어 룸 보정을 껐습니다. 설정 → 재생 → 룸 보정에서 다시 불러오세요.",
        "The room correction filter that was kept cannot be read, so room correction is off. Import it again in Settings → Playback → Room Correction.";
    room_hint: "필터는 곡의 샘플레이트가 달라도 맞게 변환해 쓰고, 어떤 주파수도 원래보다 커지지 않게 전체를 낮춥니다(낮춘 양은 곡 정보의 신호 경로에 나옵니다). 지연은 없습니다. 헤드폰으로 들을 때는 꺼 두세요.",
        "The filter is converted to fit whatever sample rate plays, and turned down as a whole so that no frequency gets louder than it was (by how much is shown in the signal path of Song Info). It adds no delay. Switch it off for headphones.";
    info_proc_room: "룸 보정", "Room correction";
    correction_filter_text: "텍스트 파일", "Text files";
    correction_hint: "헤드폰마다 다른 주파수 특성을, 측정해서 만든 필터 값으로 바로잡습니다. 기본 프로필은 AutoEq 프로젝트가 oratory1990의 측정으로 만든 값이며(Harman 목표 곡선 기준; Momentum 5 Wireless만 Super* Review의 측정을 AutoEq로 계산한 값), 같은 모델이라도 제품마다 조금씩 다를 수 있습니다. 이퀄라이저 다음, 크로스피드 앞에서 적용됩니다. 켜면 소리는 원본과 달라집니다. 불러오는 파일에서 지원하는 필터는 PK, LSC, HSC입니다.",
        "Corrects a headphone's frequency response with filter values made from measurements. The built-in profiles are the AutoEq project's results for measurements by oratory1990 (to the Harman target; the Momentum 5 Wireless alone was computed with AutoEq from a measurement by Super* Review); units of the same model differ a little. Applied after the equalizer and before crossfeed. While on, the sound differs from the original. Imported files may use PK, LSC and HSC filters.";
    correction_err_read: "보정값 파일을 읽을 수 없습니다.", "The correction file could not be read.";
    correction_err_empty: "보정값(Preamp, Filter 줄)이 없는 파일입니다.", "The file holds no correction (no Preamp or Filter lines).";
    correction_err_many: "필터가 너무 많은 파일입니다(32개까지).", "The file has too many filters (32 at most).";
    // Radio
    radio: "라디오", "Radio";
    radio_row: "대기열이 끝나면 이어서 재생", "Keep Playing When the Queue Ends";
    play_order: "재생 순서", "Playing Order";
    shuffle_by_album_row: "앨범 단위로 셔플", "Shuffle by Album";
    shuffle_by_rating_row: "별점 높은 곡을 앞에 (셔플)", "Favor Higher Ratings When Shuffling";
    shuffle_by_rating_desc: "셔플할 때 별이 많은 곡일수록 앞쪽에 올 가능성이 커집니다. 모든 곡이 한 번씩 나오는 것은 같습니다.", "When shuffling, a song with more stars is more likely to come early. Every song still comes once.";
    shuffle_by_album_desc: "앨범의 순서만 섞고, 앨범 안의 곡은 순서대로 이어 재생합니다.", "Mixes the order of albums; each album plays through in order.";
    resume_long_row: "긴 곡은 이어 듣기", "Resume Long Tracks";
    resume_long_desc: "20분이 넘는 곡은 멈춘 곳에서 다시 시작합니다.", "Tracks over 20 minutes start where you left off.";
    play_order_hint: "이어 듣기는 곡을 다시 고를 때 적용되며, 앨범이 이어서 재생될 때는 처음부터 나옵니다. 끝까지 들은 곡은 다음에 처음부터 시작합니다.", "Resuming applies when you choose the track again; when an album plays on into it, it starts from the beginning. A track heard to the end starts from the beginning next time.";
    radio_desc: "마지막 곡과 어울리는 곡을 라이브러리에서 골라 대기열에 더합니다.", "Adds songs from your library that go with the last one.";
    radio_hint: "장르·아티스트·연도가 가까운 곡을 고르고, 방금 들은 곡과 같은 아티스트의 연속은 피합니다. 인터넷을 쓰지 않으며, 반복 재생이 켜져 있으면 동작하지 않습니다.",
        "Picks songs close in genre, artist and year, and avoids what was just heard and long runs of one artist. Nothing goes online. Does nothing while Repeat is on.";
    // Equalizer
    equalizer: "이퀄라이저", "Equalizer";
    menu_equalizer: "이퀄라이저…", "Equalizer…";
    menu_mini_player: "미니 플레이어 / 전체 창", "Mini Player / Full Window";
    mini_enter: "미니 플레이어 (다른 창 위에 뜨는 작은 창)", "Mini player (a small window that stays above others)";
    mini_leave: "전체 창으로", "Back to the full window";
    eq_on: "켬", "On";
    eq_off: "끔", "Off";
    eq_preset: "프리셋", "Preset";
    eq_custom: "사용자 설정", "Custom";
    eq_my_presets: "내 프리셋", "My Presets";
    eq_reset: "초기화", "Reset";
    eq_preamp: "프리앰프", "Pre-amp";
    eq_name_hint: "프리셋 이름", "Preset name";
    eq_save: "내 프리셋으로 저장", "Save as My Preset";
    eq_delete: "이 프리셋 삭제", "Delete This Preset";
    eq_saved: "프리셋을 저장했습니다.", "Preset saved.";
    eq_replaced: "같은 이름의 프리셋을 지금 값으로 바꿨습니다.", "The preset of that name now has the current values.";
    eq_full: "프리셋은 40개까지 저장할 수 있습니다. 하나를 삭제한 뒤 다시 저장하세요.", "Up to 40 presets can be saved. Delete one, then save again.";
    eq_deleted: "프리셋을 삭제했습니다. 슬라이더의 값은 그대로입니다.", "Preset deleted. The sliders keep their values.";
    eq_note: "조절한 소리는 1초 안에 들립니다. 슬라이더를 더블클릭하면 0 dB로 돌아갑니다. 대역을 올린 만큼 프리앰프를 내려 두면 소리가 최대 음량을 넘지 않습니다(기본 프리셋은 그렇게 맞춰져 있습니다). 그래도 넘는 순간은 잘리지 않고 피크 보호가 눌러 줍니다.",
        "Changes are heard within a second. Double-click a slider to return it to 0 dB. Lowering the pre-amp by as much as the highest band is raised keeps the sound within full scale (the built-in presets are set that way). What still goes over is not clipped: peak protection brings it down.";
    eq_row_on: "켜져 있음", "On";
    eq_row_off: "꺼져 있음", "Off";
    eq_row_desc: "10개 대역으로 음색을 조절합니다. 프리셋을 고르거나 직접 만들어 저장할 수 있습니다.", "Shape the tone with ten bands. Pick a preset or make and save your own.";
    eq_preset_flat: "플랫 (조절 없음)", "Flat";
    eq_preset_bass_boost: "저음 강화", "Bass Boost";
    eq_preset_bass_cut: "저음 줄이기", "Bass Reducer";
    eq_preset_treble_boost: "고음 강화", "Treble Boost";
    eq_preset_vocal: "보컬", "Vocal";
    eq_preset_loudness: "라우드니스 (작은 음량용)", "Loudness (for low volume)";
    eq_preset_pop: "팝", "Pop";
    eq_preset_rock: "록", "Rock";
    eq_preset_jazz: "재즈", "Jazz";
    eq_preset_classical: "클래식", "Classical";
    eq_preset_electronic: "일렉트로닉", "Electronic";
    eq_preset_acoustic: "어쿠스틱", "Acoustic";
    info_memory: "메모리에서 재생", "from memory";
    info_missing: "파일을 찾을 수 없습니다", "File not found";
    // Notices
    notice_device_lost: "출력 장치 연결이 끊겨 일시정지했습니다.", "The output device was disconnected. Playback is paused.";
    notice_player_stuck: "출력 장치가 응답하지 않아 재생을 제어할 수 없습니다. 장치 연결을 확인하고, 계속되면 OnEum을 다시 시작하세요.",
        "The output device is not responding, so playback cannot be controlled. Check the device; if this goes on, restart OnEum.";
    notice_device_lost_action: "기본 장치로 재생", "Play on Default Device";
    notice_no_output: "소리를 낼 출력 장치를 열 수 없습니다.", "Could not open an audio output device.";
    notice_selected_device_missing: "선택한 출력 장치가 연결되어 있지 않습니다.", "The selected output device is not connected.";
    notice_too_many_failures: "여러 곡을 연속으로 재생하지 못해 멈췄습니다. 파일 위치나 드라이브 연결을 확인하세요.",
        "Playback stopped because several songs in a row could not be played. Check the files or the drive connection.";
    notice_retry: "다시 시도", "Try Again";
    notice_open_settings: "설정 열기", "Open Settings";
    notice_db_recovered: "라이브러리 데이터가 손상되어 새로 만들었습니다. 음악 폴더를 다시 추가해 주세요. 손상된 파일은 보관해 두었습니다.",
        "The library database was damaged and has been recreated. Please add your music folders again. The damaged file was kept.";
    notice_folder_already_added: "이미 추가한 폴더입니다.", "This folder has already been added.";
    notice_folder_inside_existing: "이미 추가한 폴더 안에 있는 폴더입니다.", "This folder is inside a folder you already added.";
    notice_folder_contains_existing: "이미 추가한 폴더를 포함하는 폴더입니다. 기존 폴더를 먼저 제거하세요.",
        "This folder contains a folder you already added. Remove that one first.";
    notice_not_a_folder: "폴더가 아닙니다.", "That is not a folder.";
    notice_folder_permission: "이 폴더를 읽을 권한이 없습니다. 시스템 설정 → 개인정보 보호 및 보안 → 파일 및 폴더에서 OnEum을 허용한 뒤 설정 → 라이브러리에서 다시 스캔하세요.",
        "OnEum is not allowed to read this folder. Allow it in System Settings → Privacy & Security → Files and Folders, then rescan in Settings → Library.";
    scan_waiting_permission: if cfg!(windows) {
            "폴더를 읽는 데 시간이 걸리고 있습니다. 폴더에 접근할 수 있는지 확인하세요."
        } else {
            "폴더 접근 허용을 기다리는 중일 수 있습니다. macOS 승인 창을 확인하세요."
        },
        if cfg!(windows) {
            "Reading the folder is taking a while. Check that the folder can be accessed."
        } else {
            "Possibly waiting for permission. Check for a macOS prompt asking to allow access to the folder."
        };
    notice_generic_error: "작업을 완료하지 못했습니다.", "The operation could not be completed.";
    pin_to_sidebar: "사이드바에 고정", "Pin to Sidebar";
    check_title: "파일 검사", "File Check";
    folders: "폴더", "Folders";
    import_title: "다른 앱에서 가져오기", "Import from Another App";
    apple_import_row: "Apple Music 보관함", "Apple Music Library";
    apple_import_desc: "재생 횟수·마지막 재생·추가한 날짜·좋아요·플레이리스트", "Play counts, last plays, dates added, loved songs, playlists";
    apple_import_button: "가져오기…", "Import…";
    apple_import_hint: "Apple Music에서 파일 ▸ 보관함 ▸ 보관함 내보내기로 저장한 XML 파일을 고르세요. 라이브러리에 있는 곡(파일 위치로 찾음)에만 적용되고, 재생 횟수는 더하지 않고 큰 쪽을 남깁니다. 음악 파일은 바꾸지 않습니다.", "Choose the XML file saved with File ▸ Library ▸ Export Library… in Apple Music. It applies to the songs in your library (found by their file); play counts are not added up, the higher one is kept. Music files are not changed.";
    notice_apple_import_failed: "Apple Music 보관함 파일을 읽지 못했습니다.", "Could not read the Apple Music library file.";
    dialog_filter_apple: "Apple Music 보관함", "Apple Music library";
    data_title: "라이브러리 데이터", "Library Data";
    data_export_row: "파일로 내보내기", "Export to a File";
    data_export_desc: "재생 횟수·재생 기록·추가한 날짜·즐겨찾기·플레이리스트·저장한 검색", "Play counts, plays, dates added, favorites, playlists, saved searches";
    data_export_button: "내보내기…", "Export…";
    data_import_row: "파일에서 가져오기", "Import from a File";
    data_import_desc: "다른 기기에서 내보낸 파일을 이 라이브러리에 합칩니다", "Merges a file exported on another device into this library";
    data_hint: "다른 기기로 옮기거나 보관해 둘 수 있습니다. 파일은 곡을 음악 폴더 아래의 폴더와 파일 이름으로 적으므로, 같은 음악을 그대로 복사해 둔 기기에서 곡을 찾습니다. 가져올 때 재생 횟수는 더하지 않고 큰 쪽을 남기며, 두 번 가져와도 달라지지 않습니다. 화면·출력 같은 설정과 고정한 항목은 들어가지 않습니다. 음악 파일은 바꾸지 않습니다.", "To take to another device, or to keep. The file names songs by their folders and file names below the music folder, so songs are found on a device that holds a copy of the same music. On import, play counts are not added up, the higher one is kept; importing twice changes nothing more. Settings such as appearance and output, and what is pinned, are not part of it. Music files are not changed.";
    dialog_filter_data: "OnEum 라이브러리 데이터", "OnEum library data";
    data_file_name: "OnEum 라이브러리 데이터.json", "OnEum Library Data.json";
    notice_data_export_failed: "라이브러리 데이터를 내보내지 못했습니다. 저장 위치를 확인하세요.", "Could not export the library data. Check the destination.";
    notice_data_import_failed: "라이브러리 데이터 파일을 읽지 못했습니다.", "Could not read the library data file.";
    notice_data_import_newer: "더 새로운 버전의 OnEum에서 만든 파일입니다. 이 기기의 앱을 업데이트한 뒤 가져오세요.", "This file was made by a later version of OnEum. Update the app on this device, then import it.";
    stats: "통계", "Statistics";
    stats_7d: "7일", "7 Days";
    stats_30d: "30일", "30 Days";
    stats_all: "전체", "All Time";
    top_artists: "많이 들은 아티스트", "Top Artists";
    top_albums: "많이 들은 앨범", "Top Albums";
    top_songs: "많이 들은 곡", "Top Songs";
    top_genres: "많이 들은 장르", "Top Genres";
    stats_none: "이 기간에 들은 곡이 없습니다.", "Nothing was played in this period.";
    stats_library: "라이브러리 구성", "Your Library";
    stats_formats: "형식", "Formats";
    stats_rates: "샘플레이트", "Sample Rates";
    stats_note: "재생은 곡의 절반(긴 곡은 4분)을 넘게 들었을 때 셉니다. 기록은 이 컴퓨터에만 있습니다. 연도(올해·작년)는 이 기기의 시간대로 가릅니다. 재생 하나하나의 기록은 최근 2만 번까지 보관하므로, 그보다 많이 들은 기간은 최근 것만 나옵니다.", "A play counts once half of a song (4 minutes of a long one) has been heard. The history stays on this computer. The years (this one and the last) are those of this device's time zone. The plays themselves are kept for the last 20,000, so a period with more than that shows only the latest of them.";
    subfolders: "하위 폴더", "Folders inside";
    check_row: "음악 파일 검사", "Check Music Files";
    check_start: "검사", "Check";
    check_stop: "멈추기", "Stop";
    check_none: "모든 파일을 검사했습니다.", "Every file has been checked.";
    check_hint: "파일을 끝까지 읽어 손상(FLAC은 파일에 든 MD5와 대조), 다이내믹 레인지(DR), 트루 피크를 잽니다. 파일은 읽기만 하고, 결과는 곡 정보에 나옵니다. 검색: check:damaged, dr:<8", "Reads every file to its end and looks for damage (for FLAC, against the MD5 the file carries), and measures dynamic range (DR) and true peak. Files are only read; the results show in Song Info. Search: check:damaged, dr:<8";
    check_show_damaged: "손상된 곡 보기", "Show Damaged";
    check_results: "검사 결과", "Results";
    check_forget: "지우기", "Clear";
    info_check: "무결성", "Integrity";
    info_check_verified: "정상 (MD5 일치)", "Intact (MD5 matches)";
    info_check_decoded: "정상 (끝까지 디코드됨)", "Intact (decodes to the end)";
    info_check_damaged: "손상됨", "Damaged";
    info_dr: "다이내믹 레인지", "Dynamic range";
    info_true_peak: "트루 피크", "True peak";
    unpin: "고정 해제", "Unpin";
    pinned: "고정", "Pinned";
    notice_not_saved: "변경 내용을 저장하지 못했습니다. 디스크의 남은 공간을 확인하세요.", "The change could not be saved. Check that the disk has free space.";
    notice_export_failed: "플레이리스트를 내보내지 못했습니다. 저장 위치를 확인하세요.", "Could not export the playlist. Check the destination.";
    notice_import_failed: "플레이리스트 파일을 읽지 못했습니다.", "Could not read the playlist file.";
    notice_no_playable_files: "재생할 수 있는 음악 파일이 없습니다.", "No playable music files were found.";
    skip_missing: "파일을 찾을 수 없어 건너뛰었습니다", "Skipped (file not found)";
    skip_unreadable: "파일을 읽을 수 없어 건너뛰었습니다", "Skipped (file could not be read)";
    skip_unsupported: "지원하지 않는 형식이라 건너뛰었습니다", "Skipped (unsupported format)";
    skip_corrupt: "파일이 손상되어 건너뛰었습니다", "Skipped (file is damaged)";
    dismiss: "닫기", "Dismiss";
    // Menus
    menu_file: "파일", "File";
    menu_edit: "편집", "Edit";
    menu_controls: "제어", "Controls";
    menu_playlist_rename: "플레이리스트 이름 변경", "Rename Playlist";
    menu_playlist_export: "플레이리스트 내보내기…", "Export Playlist…";
    menu_playlist_delete: "플레이리스트 삭제…", "Delete Playlist…";
    notice_open_playlist_first: "먼저 사이드바에서 플레이리스트를 여세요.", "Open a playlist in the sidebar first.";
    menu_song: "곡", "Song";
    menu_song_play: "선택한 곡 재생", "Play Selection";
    menu_song_favorite: "즐겨찾기 전환", "Toggle Favorite";
    menu_mute: "음소거 전환", "Toggle Mute";
    menu_view: "보기", "View";
    menu_window: "윈도우", "Window";
    menu_help: "도움말", "Help";
    menu_about: "OnEum에 관하여", "About OnEum";
    menu_settings: "설정…", "Settings…";
    menu_hide: "OnEum 가리기", "Hide OnEum";
    menu_hide_others: "기타 가리기", "Hide Others";
    menu_show_all: "모두 보기", "Show All";
    menu_quit: "OnEum 종료", "Quit OnEum";
    menu_open_files: "음악 파일 열기…", "Open Music Files…";
    menu_add_folder: "음악 폴더 추가…", "Add Music Folder…";
    menu_new_playlist: "새 플레이리스트", "New Playlist";
    menu_close_window: "윈도우 닫기", "Close Window";
    menu_undo: "실행 취소", "Undo";
    menu_redo: "실행 복귀", "Redo";
    menu_cut: "오려두기", "Cut";
    menu_copy: "복사하기", "Copy";
    menu_paste: "붙여넣기", "Paste";
    menu_select_all: "전체 선택", "Select All";
    menu_find: "검색", "Find";
    menu_play_pause: "재생/일시정지", "Play/Pause";
    menu_next: "다음 곡", "Next";
    menu_previous: "이전 곡", "Previous";
    menu_volume_up: "볼륨 올리기", "Increase Volume";
    menu_volume_down: "볼륨 내리기", "Decrease Volume";
    menu_toggle_shuffle: "셔플 켜기/끄기", "Toggle Shuffle";
    menu_cycle_repeat: "반복 모드 바꾸기", "Change Repeat Mode";
    menu_stop_after_current: "현재 곡이 끝나면 정지", "Stop After Current Song";
    menu_repeat_section: "구간 반복 (A–B) 지정", "Mark Section to Repeat (A–B)";
    section_mark_start: "구간 반복: 여기서 시작 (A)", "Repeat Section: Start Here (A)";
    section_mark_end: "구간 반복: 여기까지 (B)", "Repeat Section: End Here (B)";
    section_off: "구간 반복 끄기", "Stop Repeating Section";
    section_cancel_hint: "이 곡의 표시한 구간을 되풀이합니다. 누르면 끕니다.", "The marked section of this song is played over and over. Click to end it.";
    shortcut_repeat_section: "구간 반복: 시작(A) → 끝(B) → 끄기", "Repeat a section: start (A) → end (B) → off";
    menu_sleep_timer: "잠자기 타이머", "Sleep Timer";
    sleep_off: "끄기", "Off";
    sleep_15: "15분 뒤", "In 15 Minutes";
    sleep_30: "30분 뒤", "In 30 Minutes";
    sleep_45: "45분 뒤", "In 45 Minutes";
    sleep_60: "1시간 뒤", "In 1 Hour";
    sleep_90: "1시간 30분 뒤", "In 1 Hour 30 Minutes";
    sleep_cancel_hint: "잠자기 타이머: 이 시간이 지나면 재생을 멈춥니다. 누르면 끕니다.",
        "Sleep timer: playback pauses when this time is up. Click to switch it off.";
    menu_seek_forward: "5초 앞으로", "Skip Ahead 5 Seconds";
    menu_seek_back: "5초 뒤로", "Skip Back 5 Seconds";
    stop_after_current_on: "이 곡이 끝나면 정지합니다", "Stops when this song ends";
    stop_after_short: "곡 끝에 정지", "Stops after song";
    stop_after_cancel_hint: "이 곡이 끝나면 재생을 멈춥니다. 누르면 끕니다.",
        "Playback stops when this song ends. Click to switch it off.";
    search_syntax_hint: "찾을 곳을 정할 수 있습니다: artist:아이유 · album:… · title:… · genre:재즈 · year:1990-1999 · format:flac\n들은 기록으로: plays:>=5 · fav:yes · added:30d · played:1y\n여러 낱말을 하나로: \"let it be\"\n모든 곡에서 검색한 뒤 ＋를 누르면 스마트 플레이리스트로 저장됩니다.",
        "Narrow it to a field: artist:beatles · album:… · title:… · genre:jazz · year:1990-1999 · format:flac\nBy what you listened to: plays:>=5 · fav:yes · added:30d · played:1y\nKeep words together: \"let it be\"\nSearch All Songs, then press ＋ to keep the search as a smart playlist.";
    menu_go_to_current: "재생 중인 곡으로 이동", "Go to Current Song";
    menu_show_queue: "재생 대기열 보기/가리기", "Show/Hide Up Next";
    menu_show_info: "곡 정보 보기/가리기", "Show/Hide Song Info";
    menu_show_meter: "VU 미터 보기/가리기", "Show/Hide VU Meter";
    menu_rescan: "라이브러리 다시 스캔", "Rescan Library";
    menu_minimize: "최소화", "Minimize";
    menu_zoom: "확대/축소", "Zoom";
    menu_main_window: "OnEum 윈도우", "OnEum Window";
    menu_bring_all_to_front: "모두 앞으로 가져오기", "Bring All to Front";
    menu_fullscreen: "전체 화면", "Enter Full Screen";
    menu_user_guide: "OnEum 사용 안내", "OnEum User Guide";
    // File dialogs
    dialog_choose_folder: "음악 폴더 선택", "Choose a Music Folder";
    dialog_choose_files: "음악 파일 선택", "Choose Music Files";
    dialog_filter_audio: "음악 파일", "Audio Files";
    dialog_filter_playlist: "플레이리스트", "Playlists";
    // The phone screens
    phone_library: "보관함", "Library";
    phone_data_title: "보관함 데이터", "Library Data";
    phone_data_import_desc: "다른 기기에서 내보낸 파일을 이 보관함에 합칩니다", "Merges a file exported on another device into this library";
    phone_data_hint: "다른 기기로 옮기거나 보관해 둘 수 있습니다. 파일은 곡을 음악 폴더 아래의 폴더와 파일 이름으로 적으므로, 같은 음악을 그대로 복사해 둔 기기에서 곡을 찾습니다. 가져올 때 재생 횟수는 더하지 않고 큰 쪽을 남기며, 두 번 가져와도 달라지지 않습니다. 화면·재생 같은 설정과 고정한 항목은 들어가지 않습니다. 음악 파일은 바꾸지 않습니다.", "To take to another device, or to keep. The file names songs by their folders and file names below the music folder, so songs are found on a device that holds a copy of the same music. On import, play counts are not added up, the higher one is kept; importing twice changes nothing more. Settings such as appearance and playback, and what is pinned, are not part of it. Music files are not changed.";
    phone_songs: "곡", "Songs";
    phone_now_playing: "지금 재생", "Now Playing";
    phone_more: "더 보기", "More";
    phone_empty_title: "음악을 넣어 주세요", "Add your music";
    phone_empty_body: "파일 앱의 ‘나의 iPhone › OnEum’ 폴더에 음악을 넣으면 여기에 나타납니다. 다른 곳에 있는 음악은 아래에서 가져올 수 있습니다.",
        "Music put into “On My iPhone › OnEum” in the Files app appears here. Music kept elsewhere can be brought in below.";
    phone_import_files: "파일 가져오기", "Import Files";
    phone_import_folder: "폴더 가져오기", "Import Folder";
    phone_import_desc: "고른 파일을 OnEum의 폴더로 복사합니다.", "Copies what you choose into OnEum's folder.";
    phone_music_place: "음악이 있는 곳", "Where the music is";
    phone_music_place_desc: "파일 앱 › 나의 iPhone › OnEum", "Files › On My iPhone › OnEum";
    phone_rescan_desc: "폴더에 넣은 음악을 지금 읽습니다.", "Reads what was put into the folder now.";
    phone_search_prompt: "제목, 아티스트, 앨범으로 찾습니다.", "Find by title, artist or album.";
    phone_no_playlists: "플레이리스트가 없습니다", "No playlists";
    phone_no_playlists_hint: "곡의 ‘더 보기’에서 플레이리스트에 추가할 수 있습니다.", "Add songs from their “More” menu.";
    phone_smart: "스마트 플레이리스트", "Smart Playlists";
    phone_app_volume: "앱 볼륨", "App volume";
    phone_import_failed: "가져오지 못했습니다.", "Could not import.";
    phone_eq_note: "조절한 소리는 1초 안에 들립니다. 슬라이더를 두 번 탭하면 0 dB로 돌아갑니다. 대역을 올린 만큼 프리앰프를 내려 두면 소리가 최대 음량을 넘지 않습니다(기본 프리셋은 그렇게 맞춰져 있습니다). 그래도 넘는 순간은 잘리지 않고 피크 보호가 눌러 줍니다.",
        "Changes are heard within a second. Double-tap a slider to return it to 0 dB. Lowering the pre-amp by as much as the highest band is raised keeps the sound within full scale (the built-in presets are set that way). What still goes over is not clipped: peak protection brings it down.";
    phone_stats_note: "재생은 곡의 절반(긴 곡은 4분)을 넘게 들었을 때 셉니다. 기록은 이 휴대폰에만 있습니다. 연도(올해·작년)는 이 기기의 시간대로 가릅니다. 재생 하나하나의 기록은 최근 2만 번까지 보관하므로, 그보다 많이 들은 기간은 최근 것만 나옵니다.", "A play counts once half of a song (4 minutes of a long one) has been heard. The history stays on this phone. The years (this one and the last) are those of this device's time zone. The plays themselves are kept for the last 20,000, so a period with more than that shows only the latest of them.";
    phone_pin: "보관함에 고정", "Pin to Library";
    phone_pins_full: "고정은 12개까지 할 수 있습니다. 하나를 해제한 뒤 다시 고정하세요.", "Up to 12 things can be pinned. Unpin one, then pin again.";
    phone_sort: "정렬", "Sort";
    phone_sort_default: "기본 순서", "Default Order";
    phone_sort_added: "추가한 날", "Date Added";
    phone_select: "선택", "Select";
    phone_done: "완료", "Done";
    phone_deselect: "선택 해제", "Deselect All";
    phone_selected_songs: "선택한 곡…", "Selected Songs…";
    phone_nothing_selected: "곡을 눌러 고르세요.", "Tap songs to choose them.";
    phone_reorder: "순서 바꾸기", "Reorder";
    phone_show_total: "전체 길이 보기", "Show the whole length";
    phone_show_left: "남은 시간 보기", "Show the time left";
    phone_search_tips: "찾을 곳을 정할 수 있습니다: artist:아이유 · album:… · title:… · genre:재즈 · year:1990-1999 · format:flac\n들은 기록으로: plays:>=5 · fav:yes · added:30d · played:1y\n파일 검사 결과로: check:damaged · dr:<8\n여러 낱말을 하나로: \"let it be\"\n곡을 검색한 뒤 ＋를 누르면 그 검색이 스마트 플레이리스트로 저장됩니다.",
        "A search can be narrowed to a field: artist:beatles · album:… · title:… · genre:jazz · year:1990-1999 · format:flac\nBy what was listened to: plays:>=5 · fav:yes · added:30d · played:1y\nBy what the file check found: check:damaged · dr:<8\nSeveral words as one: \"let it be\"\nSearch for songs, then press ＋ to keep the search as a smart playlist.";
    phone_empty_body_folders: "휴대폰의 Music 폴더에 있는 음악이 여기에 나타납니다. 다른 폴더는 아래에서 추가할 수 있습니다.", "Music in the phone's Music folder appears here. Other folders can be added below.";
    phone_storage_needed: "음악 파일을 읽으려면 ‘모든 파일에 대한 접근’을 허용해야 합니다. 아래 버튼이 여는 설정 화면에서 OnEum을 허용해 주세요.", "To read music files OnEum needs “All files access”. The button below opens the settings page where it is allowed.";
    phone_storage_allow: "접근 허용", "Allow Access";
    phone_storage_row: "파일 접근", "File Access";
    phone_storage_missing: "허용되지 않아 음악을 읽을 수 없습니다.", "Not allowed: music cannot be read.";
    phone_add_folder: "폴더 추가", "Add Folder";
    phone_music_folders: "음악 폴더", "Music Folders";
    phone_folders_hint: "폴더는 있는 자리에서 읽습니다. SD 카드나 꽂은 저장 장치의 폴더도 추가할 수 있습니다. 제거해도 파일은 그대로입니다.", "Folders are read where they are. A folder on a card or a plugged-in drive can be added too. Removing one leaves its files as they are.";
    phone_link_folder: "폴더 연결", "Link Folder";
    phone_link_folder_desc: "복사하지 않고 있는 자리에서 읽습니다. iCloud Drive의 폴더나 꽂은 저장 장치의 폴더에 씁니다.", "Reads a folder where it is, without copying: one in iCloud Drive or on a drive that is plugged in.";
    phone_linked_folders: "연결한 폴더", "Linked Folders";
    phone_link_failed: "폴더를 연결하지 못했습니다.", "Could not link the folder.";
    phone_unlink: "연결 해제", "Unlink";
    phone_linked_hint: "연결한 폴더는 그 자리에 있는 동안 읽습니다. 저장 장치를 뺐다가 다시 꽂았으면 다시 읽기를 누르세요. iCloud Drive의 곡은 iPhone에 내려받은 것만 읽습니다. 연결을 해제해도 파일은 그대로입니다.", "A linked folder is read while it is where it was. After plugging a drive in again, press Rescan. Of the songs in iCloud Drive only those downloaded to the iPhone are read. Unlinking leaves the files as they are.";
    phone_search_here: "이 목록에서 검색", "Search This List";
    phone_search_here_hint: "이 목록에서 찾기", "Find in this list";
    phone_show_in_songs: "곡 목록에서 보기", "Show in Songs";
    phone_apple_import_hint: "Mac의 Apple Music에서 파일 ▸ 보관함 ▸ 보관함 내보내기로 저장한 XML 파일을 이 휴대폰으로 옮긴 뒤 고르세요. 곡은 들어 있는 폴더와 파일 이름으로 찾으므로, Mac의 음악 폴더를 그대로 복사해 넣은 곡에 적용됩니다. 재생 횟수는 더하지 않고 큰 쪽을 남깁니다. 음악 파일은 바꾸지 않습니다.",
        "Bring the XML file saved with File ▸ Library ▸ Export Library… in Apple Music on a Mac over to this phone, then choose it. Songs are found by the folders they are in and their file names, so it applies to music copied from the Mac's music folder as it is. Play counts are not added up: the larger one is kept. Music files are not changed.";
    phone_match_rate_desc: "곡마다 그 곡의 샘플레이트로 내보내 달라고 iPhone에 요청합니다. 받아 주면 변환 없이 나갑니다.",
        "Asks the iPhone to play each song at the song's own sample rate. Where it does, nothing is converted.";
    phone_quality_hint: "샘플레이트를 정하는 것은 iPhone입니다. 내장 출력은 44.1kHz와 48kHz 사이에서, USB DAC은 그 DAC이 받는 샘플레이트까지 따라옵니다. 따라오지 않으면 평소처럼 변환해 재생합니다. 샘플레이트가 다른 곡으로 넘어갈 때는 1초 안쪽으로 끊기고, 같은 샘플레이트의 곡은 끊김 없이 이어집니다. 지금 어떻게 나가고 있는지는 곡 정보의 신호 경로에 나옵니다.",
        "The iPhone decides the sample rate. The built-in output follows between 44.1 and 48 kHz, a USB DAC up to what it accepts. Where the output does not follow, the sound is converted as usual. Going on to a song of another sample rate takes a break of less than a second; songs of the same rate follow each other without one. How the sound goes out right now is shown in the signal path of Song Info.";
    phone_quality_hint_fixed: "출력의 샘플레이트는 시스템이 정합니다. 곡의 샘플레이트가 그와 다르면 변환해 재생합니다. 지금 어떻게 나가고 있는지는 곡 정보의 신호 경로에 나옵니다.",
        "The system decides the output's sample rate. A song of another sample rate is converted. How the sound goes out right now is shown in the signal path of Song Info.";
    confirm_remove_copies: "OnEum이 가져오면서 만들어 둔 사본은 함께 지워집니다.", "The copies OnEum made when it brought them in are deleted with them.";
    phone_open_files: "음악 파일 열기", "Open Music Files";
    phone_open_files_desc: "폴더를 추가하지 않고 고른 파일만 재생합니다. 그 곡은 보관함에 남습니다.",
        "Plays the files you choose without adding their folder. The songs stay in the library.";
    phone_show_folder: "보기", "Show";
    notice_cannot_show_folder: "이 폴더는 파일 앱에서 보여 줄 수 없습니다.", "The Files app cannot show this folder.";
    log_save: "로그 저장", "Save the Log";
    log_save_button: "저장…", "Save…";
    notice_log_failed: "로그를 저장하지 못했습니다.", "The log could not be saved.";
    phone_shortcuts_song: "재생 중인 곡", "The Song That Is Playing";
    phone_shortcuts_screen: "화면", "Screens";
    phone_shortcut_back: "뒤로", "Back";
    phone_shortcuts_note: "휴대폰에 꽂거나 블루투스로 연결한 키보드에서 씁니다. 글자를 입력하는 동안에는 Space가 글자 입력에 쓰입니다.",
        "For a keyboard that is plugged into the phone or paired with it. While text is being typed, Space types a space.";
    phone_no_lyrics: "가사가 없습니다", "No lyrics";
    phone_no_lyrics_hint: "파일의 태그나 같은 이름의 .lrc 파일에 든 가사를 보여 줍니다.", "Lyrics come from the file's tags or an .lrc file of the same name.";
}

impl Lang {
    pub fn strings(self) -> &'static Strings {
        match self {
            Lang::Ko => &KO,
            Lang::En => &EN,
        }
    }

    pub fn song_count(self, n: usize) -> String {
        match self {
            Lang::Ko => format!("{}곡", group_digits(n)),
            Lang::En if n == 1 => "1 song".to_string(),
            Lang::En => format!("{} songs", group_digits(n)),
        }
    }

    pub fn album_count(self, n: usize) -> String {
        match self {
            Lang::Ko => format!("앨범 {}장", group_digits(n)),
            Lang::En if n == 1 => "1 album".to_string(),
            Lang::En => format!("{} albums", group_digits(n)),
        }
    }

    pub fn scan_progress(self, found: u64) -> String {
        match self {
            Lang::Ko => format!("스캔 중 · {}곡 확인", group_digits(found as usize)),
            Lang::En => format!("Scanning · {} files checked", group_digits(found as usize)),
        }
    }

    /// Songs without leveling tags that have not been analyzed yet.
    pub fn loudness_waiting(self, n: usize) -> String {
        match self {
            Lang::Ko => format!(
                "분석할 곡 {}곡 · 곡을 끝까지 읽으므로 시간이 걸립니다.",
                group_digits(n)
            ),
            Lang::En => format!(
                "{} to analyze · every file is read to its end, so this takes a while.",
                self.song_count(n)
            ),
        }
    }

    /// Progress of the loudness analysis, or its outcome once it has ended.
    pub fn loudness_progress(self, running: bool, done: u64, total: u64, failed: u64) -> String {
        let (done, total, failed) = (done as usize, total as usize, failed as usize);
        let mut text = match (self, running) {
            (Lang::Ko, true) => format!(
                "분석 중 · {}곡 중 {}곡",
                group_digits(total),
                group_digits(done)
            ),
            (Lang::En, true) => {
                format!(
                    "Analyzing · {} of {}",
                    group_digits(done),
                    group_digits(total)
                )
            }
            (Lang::Ko, false) => {
                format!("{}곡을 분석했습니다", group_digits(done - failed.min(done)))
            }
            (Lang::En, false) => format!("Analyzed {}", self.song_count(done - failed.min(done))),
        };
        if failed > 0 {
            text.push_str(&match self {
                Lang::Ko => format!(" · 읽지 못한 곡 {}곡", group_digits(failed)),
                Lang::En => format!(" · {} could not be read", group_digits(failed)),
            });
        }
        if !running {
            text.push('.');
        }
        text
    }

    /// "1,234 plays · 56 h 7 min · 890 songs".
    pub fn stats_summary(self, plays: usize, listened_ms: u64, songs: usize) -> String {
        let minutes = listened_ms / 60_000;
        let (h, m) = (minutes / 60, minutes % 60);
        match self {
            Lang::Ko => {
                let length = if h > 0 {
                    format!("{}시간 {m}분", group_digits(h as usize))
                } else {
                    format!("{m}분")
                };
                format!(
                    "{}회 재생 · {length} · {}곡",
                    group_digits(plays),
                    group_digits(songs)
                )
            }
            Lang::En => {
                let length = if h > 0 {
                    format!("{} h {m} min", group_digits(h as usize))
                } else {
                    format!("{m} min")
                };
                format!(
                    "{} plays · {length} · {}",
                    group_digits(plays),
                    self.song_count(songs)
                )
            }
        }
    }

    /// "12회" / "12 plays".
    pub fn plays_short(self, n: usize) -> String {
        match self {
            Lang::Ko => format!("{}회", group_digits(n)),
            Lang::En if n == 1 => "1 play".to_string(),
            Lang::En => format!("{} plays", group_digits(n)),
        }
    }

    /// The library in a line: songs, total length, size, lossless share.
    pub fn library_makeup(
        self,
        songs: usize,
        total_ms: u64,
        bytes: u64,
        lossless: usize,
    ) -> String {
        let minutes = total_ms / 60_000;
        let (h, m) = (minutes / 60, minutes % 60);
        let size = if bytes >= 1 << 30 {
            format!("{:.1} GB", bytes as f64 / 1_073_741_824.0)
        } else {
            format!("{:.0} MB", bytes as f64 / 1_048_576.0)
        };
        let share = (lossless * 100).checked_div(songs).unwrap_or(0);
        match self {
            Lang::Ko => {
                let length = if h > 0 {
                    format!("{}시간 {m}분", group_digits(h as usize))
                } else {
                    format!("{m}분")
                };
                format!(
                    "{}곡 · {length} · {size} · 무손실 {share}%",
                    group_digits(songs)
                )
            }
            Lang::En => {
                let length = if h > 0 {
                    format!("{} h {m} min", group_digits(h as usize))
                } else {
                    format!("{m} min")
                };
                format!(
                    "{} · {length} · {size} · {share}% lossless",
                    self.song_count(songs)
                )
            }
        }
    }

    /// The outcome of an Apple Music import.
    pub fn apple_import_result(self, matched: usize, unmatched: usize, playlists: usize) -> String {
        match self {
            Lang::Ko => {
                let mut text = format!(
                    "Apple Music에서 {}곡의 기록을 가져왔습니다",
                    group_digits(matched)
                );
                if playlists > 0 {
                    text.push_str(&format!(" · 플레이리스트 {playlists}개"));
                }
                if unmatched > 0 {
                    text.push_str(&format!(
                        " · 라이브러리에 없는 곡 {}곡",
                        group_digits(unmatched)
                    ));
                }
                text.push('.');
                text
            }
            Lang::En => {
                let mut text = format!(
                    "Took over the history of {} from Apple Music",
                    self.song_count(matched)
                );
                if playlists > 0 {
                    text.push_str(&format!(" · {playlists} playlists"));
                }
                if unmatched > 0 {
                    text.push_str(&format!(
                        " · {} not in your library",
                        group_digits(unmatched)
                    ));
                }
                text.push('.');
                text
            }
        }
    }

    /// Files the check has yet to read.
    pub fn check_waiting(self, n: usize) -> String {
        match self {
            Lang::Ko => format!(
                "검사할 파일 {}개 · 파일을 끝까지 읽으므로 시간이 걸립니다.",
                group_digits(n)
            ),
            Lang::En => format!(
                "{} files to check · every file is read to its end, so this takes a while.",
                group_digits(n)
            ),
        }
    }

    /// Progress of the file check, or its outcome once it has ended.
    pub fn check_progress(
        self,
        running: bool,
        done: u64,
        total: u64,
        damaged: u64,
        failed: u64,
    ) -> String {
        let (done, total, damaged, failed) = (
            done as usize,
            total as usize,
            damaged as usize,
            failed as usize,
        );
        let mut text = match (self, running) {
            (Lang::Ko, true) => format!(
                "검사 중 · {}개 중 {}개",
                group_digits(total),
                group_digits(done)
            ),
            (Lang::En, true) => format!(
                "Checking · {} of {}",
                group_digits(done),
                group_digits(total)
            ),
            (Lang::Ko, false) => format!("{}개를 검사했습니다", group_digits(done)),
            (Lang::En, false) => format!("Checked {} files", group_digits(done)),
        };
        text.push_str(&match (self, damaged) {
            (Lang::Ko, 0) => " · 손상 없음".to_string(),
            (Lang::En, 0) => " · no damage found".to_string(),
            (Lang::Ko, n) => format!(" · 손상 {}개", group_digits(n)),
            (Lang::En, n) => format!(" · {} damaged", group_digits(n)),
        });
        if failed > 0 {
            text.push_str(&match self {
                Lang::Ko => format!(" · 읽지 못한 파일 {}개", group_digits(failed)),
                Lang::En => format!(" · {} could not be read", group_digits(failed)),
            });
        }
        if !running {
            text.push('.');
        }
        text
    }

    /// A correction profile in a line: how many filters, the preamp, and a warning when
    /// its boosts exceed the room the preamp makes.
    pub fn correction_summary(self, filters: usize, preamp_db: f32, peak_db: f32) -> String {
        let preamp = if preamp_db < 0.0 {
            format!("−{:.1} dB", -preamp_db)
        } else {
            format!("+{preamp_db:.1} dB")
        };
        let mut text = match self {
            Lang::Ko => format!("필터 {filters}개 · 프리앰프 {preamp}"),
            Lang::En => format!(
                "{} · preamp {preamp}",
                en_count(filters, "filter", "filters")
            ),
        };
        if peak_db > 0.05 {
            text.push_str(&match self {
                Lang::Ko => {
                    format!(" · 최대 +{peak_db:.1} dB 올림(큰 소리에서 피크 보호가 동작할 수 있음)")
                }
                Lang::En => format!(
                    " · boosts up to +{peak_db:.1} dB (peak protection may act on loud passages)"
                ),
            });
        }
        text
    }

    pub fn correction_imported(self, name: &str) -> String {
        match self {
            Lang::Ko => format!("“{name}” 보정 프로필을 불러와 적용했습니다."),
            Lang::En => format!("Imported the correction profile “{name}” and applied it."),
        }
    }

    pub fn correction_unsupported(self, kind: &str) -> String {
        match self {
            Lang::Ko => format!(
                "지원하지 않는 필터({kind})가 들어 있어 불러오지 않았습니다. 지원하는 필터: PK, LSC, HSC."
            ),
            Lang::En => format!(
                "Not imported: the file uses a filter that is not supported ({kind}). Supported: PK, LSC, HSC."
            ),
        }
    }

    pub fn correction_malformed(self, line: &str) -> String {
        let line: String = line.chars().take(80).collect();
        match self {
            Lang::Ko => format!("이 줄을 이해할 수 없어 불러오지 않았습니다: {line}"),
            Lang::En => format!("Not imported: this line could not be understood: {line}"),
        }
    }

    pub fn selected_count(self, n: usize) -> String {
        match self {
            Lang::Ko => format!("{}곡 선택됨", group_digits(n)),
            Lang::En => format!("{} selected", group_digits(n)),
        }
    }

    /// Total playing time in a compact form for list footers.
    pub fn total_time(self, ms: u64) -> String {
        let seconds = ms / 1000;
        let (h, m, sec) = (seconds / 3600, (seconds / 60) % 60, seconds % 60);
        match (self, h, m) {
            (Lang::Ko, 0, 0) => format!("{sec}초"),
            (Lang::Ko, 0, _) => format!("{m}분"),
            (Lang::Ko, _, _) => format!("{h}시간 {m}분"),
            (Lang::En, 0, 0) => format!("{sec} sec"),
            (Lang::En, 0, _) => format!("{m} min"),
            (Lang::En, _, _) => format!("{h} hr {m} min"),
        }
    }

    pub fn playlist_import_result(self, name: &str, matched: usize, missing: usize) -> String {
        match (self, missing) {
            (Lang::Ko, 0) => format!(
                "“{name}” 플레이리스트를 가져왔습니다 ({}).",
                self.song_count(matched)
            ),
            (Lang::Ko, _) => format!(
                "“{name}” 플레이리스트를 가져왔습니다 ({}, 찾지 못한 항목 {}개).",
                self.song_count(matched),
                group_digits(missing)
            ),
            (Lang::En, 0) => format!("Imported playlist “{name}” ({}).", self.song_count(matched)),
            (Lang::En, _) => format!(
                "Imported playlist “{name}” ({}, {} not found).",
                self.song_count(matched),
                en_count(missing, "entry", "entries")
            ),
        }
    }

    pub fn data_exported(self, songs: usize, playlists: usize) -> String {
        match self {
            Lang::Ko => format!(
                "라이브러리 데이터를 내보냈습니다 ({} · 플레이리스트 {playlists}개).",
                self.song_count(songs)
            ),
            Lang::En => format!(
                "Library data exported ({} · {playlists} playlists).",
                self.song_count(songs)
            ),
        }
    }

    pub fn data_import_result(
        self,
        matched: usize,
        unmatched: usize,
        playlists: usize,
        searches: usize,
    ) -> String {
        let mut text = match self {
            Lang::Ko => format!("{}곡의 기록을 가져왔습니다", group_digits(matched)),
            Lang::En => format!("Took over the history of {}", self.song_count(matched)),
        };
        if playlists > 0 {
            text.push_str(&match self {
                Lang::Ko => format!(" · 플레이리스트 {playlists}개"),
                Lang::En => format!(" · {playlists} playlists"),
            });
        }
        if searches > 0 {
            text.push_str(&match self {
                Lang::Ko => format!(" · 저장한 검색 {searches}개"),
                Lang::En => format!(" · {searches} saved searches"),
            });
        }
        if unmatched > 0 {
            text.push_str(&match self {
                Lang::Ko => format!(" · 라이브러리에 없는 곡 {}곡", group_digits(unmatched)),
                Lang::En => format!(" · {} not in your library", group_digits(unmatched)),
            });
        }
        text.push('.');
        text
    }

    pub fn correction_saved(self, name: &str) -> String {
        match self {
            Lang::Ko => format!("보정 프로필 \"{name}\"을(를) 저장했습니다."),
            Lang::En => format!("Correction profile \"{name}\" saved."),
        }
    }

    /// A playback speed as it is shown ("1.25×").
    pub fn speed_label(percent: u16) -> String {
        let text = format!("{:.2}", f32::from(percent) / 100.0);
        format!("{}×", text.trim_end_matches('0').trim_end_matches('.'))
    }

    /// The playback speed as the signal path tells of it.
    pub fn speed_detail(self, percent: u16) -> String {
        match self {
            Lang::Ko => format!("{}, 음높이 유지", Lang::speed_label(percent)),
            Lang::En => format!("{}, pitch kept", Lang::speed_label(percent)),
        }
    }

    /// The crossfade as the signal path tells of it.
    pub fn crossfade_detail(self, secs: u8) -> String {
        match self {
            Lang::Ko => format!("곡이 바뀔 때 {secs}초"),
            Lang::En => format!("{secs} s between songs"),
        }
    }

    /// The section of a song whose beginning is marked (`from`: a time as shown).
    pub fn section_begun(self, from: &str) -> String {
        match self {
            Lang::Ko => format!("구간 시작 {from}"),
            Lang::En => format!("Section from {from}"),
        }
    }

    /// The section of a song that is being repeated.
    pub fn section_repeating(self, from: &str, to: &str) -> String {
        match self {
            Lang::Ko => format!("구간 반복 {from}–{to}"),
            Lang::En => format!("Repeating {from}–{to}"),
        }
    }

    pub fn playlist_exported(self, count: usize) -> String {
        match self {
            Lang::Ko => format!("플레이리스트를 내보냈습니다 ({}).", self.song_count(count)),
            Lang::En => format!("Playlist exported ({}).", self.song_count(count)),
        }
    }

    pub fn skipped(self, title: &str, reason: &str) -> String {
        match self {
            Lang::Ko => format!("“{title}” — {reason}."),
            Lang::En => format!("“{title}” — {reason}."),
        }
    }

    pub fn folder_added(self, name: &str) -> String {
        match self {
            Lang::Ko => format!("“{name}” 폴더를 추가했습니다. 스캔을 시작합니다."),
            Lang::En => format!("Added “{name}”. Scanning has started."),
        }
    }

    pub fn scan_finished(self, total: usize, unsupported: u64, errors: u64) -> String {
        let mut text = match self {
            Lang::Ko => format!("스캔 완료 · 라이브러리 {}곡", group_digits(total)),
            Lang::En => format!("Scan finished · {} in library", self.song_count(total)),
        };
        if unsupported > 0 {
            text += &match self {
                Lang::Ko => format!(
                    " · 지원하지 않는 파일 {}개 제외",
                    group_digits(unsupported as usize)
                ),
                Lang::En => format!(
                    " · {} skipped",
                    en_count(
                        unsupported as usize,
                        "unsupported file",
                        "unsupported files"
                    )
                ),
            };
        }
        if errors > 0 {
            text += &match self {
                Lang::Ko => format!(" · 읽지 못한 파일 {}개", group_digits(errors as usize)),
                Lang::En => format!(
                    " · {}",
                    en_count(errors as usize, "unreadable file", "unreadable files")
                ),
            };
        }
        text
    }

    pub fn added_to_queue(self, n: usize) -> String {
        match self {
            Lang::Ko => format!("대기열 끝에 {}을 추가했습니다.", self.song_count(n)),
            Lang::En => format!("Added {} to the end of the queue.", self.song_count(n)),
        }
    }

    pub fn added_to_playlist(self, n: usize, name: &str) -> String {
        match self {
            Lang::Ko => format!("“{name}”에 {}을 추가했습니다.", self.song_count(n)),
            Lang::En => format!("Added {} to “{name}”.", self.song_count(n)),
        }
    }

    pub fn artist_count(self, n: usize) -> String {
        match self {
            Lang::Ko => format!("아티스트 {}명", group_digits(n)),
            Lang::En if n == 1 => "1 artist".to_string(),
            Lang::En => format!("{} artists", group_digits(n)),
        }
    }

    /// "지금: Mac mini 스피커 · 48,000 Hz" — the device playback is running on.
    pub fn output_now(self, device: &str, rate: Option<u32>) -> String {
        let rate = rate
            .map(|r| format!(" · {} Hz", group_digits(r as usize)))
            .unwrap_or_default();
        match self {
            Lang::Ko => format!("지금: {device}{rate}"),
            Lang::En => format!("Now: {device}{rate}"),
        }
    }

    /// How long ago a folder was last scanned.
    pub fn scanned_ago(self, seconds: i64) -> String {
        let seconds = seconds.max(0);
        let (n, ko, en) = match seconds {
            0..=59 => {
                return match self {
                    Lang::Ko => "방금 스캔함".to_string(),
                    Lang::En => "Scanned just now".to_string(),
                };
            }
            60..=3599 => (seconds / 60, "분", "min"),
            3600..=86_399 => (seconds / 3600, "시간", "hr"),
            _ => (seconds / 86_400, "일", "d"),
        };
        match self {
            Lang::Ko => format!("{n}{ko} 전 스캔"),
            Lang::En => format!("Scanned {n} {en} ago"),
        }
    }

    /// How long ago something happened, to the day: "오늘", "3일 전", "2주 전", "5달 전".
    pub fn days_ago(self, seconds: i64) -> String {
        let days = seconds.max(0) / 86_400;
        let (n, ko, en) = match days {
            0 => {
                return match self {
                    Lang::Ko => "오늘".to_string(),
                    Lang::En => "Today".to_string(),
                };
            }
            1..=13 => (days, "일", "d"),
            14..=59 => (days / 7, "주", "wk"),
            60..=729 => (days / 30, "달", "mo"),
            _ => (days / 365, "년", "yr"),
        };
        match self {
            Lang::Ko => format!("{n}{ko} 전"),
            Lang::En => format!("{n} {en} ago"),
        }
    }

    /// "3 (CD 2)": a track number with its disc, for multi-disc albums.
    pub fn track_on_disc(self, track: u32, disc: u32) -> String {
        match self {
            Lang::Ko => format!("{track} (디스크 {disc})"),
            Lang::En => format!("{track} (Disc {disc})"),
        }
    }

    pub fn artist_summary(self, albums: usize, songs: usize) -> String {
        format!("{} · {}", self.album_count(albums), self.song_count(songs))
    }
}

/// English count with the right noun form: "1 entry", "2,000 entries".
fn en_count(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{} {many}", group_digits(n))
    }
}

/// 12345 → "12,345".
pub fn group_digits(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// 83_000 ms → "1:23"; 3_723_000 ms → "1:02:03".
pub fn format_time(ms: u64) -> String {
    let total = ms / 1000;
    let (h, m, s) = (total / 3600, (total / 60) % 60, total % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_resolution() {
        assert_eq!(LanguageChoice::System.resolve(true), Lang::Ko);
        assert_eq!(LanguageChoice::System.resolve(false), Lang::En);
        assert_eq!(LanguageChoice::English.resolve(true), Lang::En);
        assert_eq!(LanguageChoice::Korean.resolve(false), Lang::Ko);
    }

    #[test]
    fn formatting_helpers() {
        assert_eq!(group_digits(0), "0");
        assert_eq!(group_digits(999), "999");
        assert_eq!(group_digits(1000), "1,000");
        assert_eq!(group_digits(50_000), "50,000");
        assert_eq!(group_digits(1_234_567), "1,234,567");
        assert_eq!(format_time(0), "0:00");
        assert_eq!(format_time(83_000), "1:23");
        assert_eq!(format_time(3_723_000), "1:02:03");
        assert_eq!(Lang::En.song_count(1), "1 song");
        assert_eq!(Lang::Ko.song_count(1200), "1,200곡");
        assert_eq!(Lang::Ko.total_time(3_900_000), "1시간 5분");
        assert_eq!(Lang::Ko.total_time(54_000), "54초");
        assert_eq!(Lang::En.total_time(600_000), "10 min");
        assert_eq!(
            Lang::En.playlist_import_result("Mix", 1, 1),
            "Imported playlist “Mix” (1 song, 1 entry not found)."
        );
        assert_eq!(
            Lang::En.scan_finished(1, 1, 2000),
            "Scan finished · 1 song in library · 1 unsupported file skipped · 2,000 unreadable files"
        );
        assert_eq!(Lang::En.playlist_exported(1), "Playlist exported (1 song).");
        assert_eq!(Lang::Ko.track_on_disc(3, 2), "3 (디스크 2)");
        assert_eq!(Lang::Ko.days_ago(3600), "오늘");
        assert_eq!(Lang::Ko.days_ago(3 * 86_400), "3일 전");
        assert_eq!(Lang::Ko.days_ago(20 * 86_400), "2주 전");
        assert_eq!(Lang::En.days_ago(100 * 86_400), "3 mo ago");
        assert_eq!(Lang::En.days_ago(800 * 86_400), "2 yr ago");
        assert_eq!(Lang::Ko.scanned_ago(30), "방금 스캔함");
        assert_eq!(Lang::Ko.scanned_ago(125), "2분 전 스캔");
        assert_eq!(Lang::En.scanned_ago(7300), "Scanned 2 hr ago");
        assert_eq!(Lang::En.scanned_ago(200_000), "Scanned 2 d ago");
        assert_eq!(
            Lang::Ko.output_now("스피커", Some(48_000)),
            "지금: 스피커 · 48,000 Hz"
        );
        assert_eq!(Lang::En.artist_count(1), "1 artist");
    }

    #[test]
    fn no_empty_strings() {
        // Spot-check that both tables are filled (the macro guarantees they have the same keys).
        for s in [&KO, &EN] {
            assert!(
                !s.add_music_folder.is_empty()
                    && !s.notice_device_lost.is_empty()
                    && !s.menu_quit.is_empty()
            );
        }
    }

    /// With the language set to English, every text of the app is English: the table
    /// has no Korean on its English side (the language's own name is not in it: the
    /// settings show "한국어" as it is), and neither do the sentences built from it.
    #[test]
    fn the_english_text_is_english() {
        let korean = |text: &str| {
            text.chars()
                .any(|c| matches!(c, '\u{AC00}'..='\u{D7A3}' | '\u{3131}'..='\u{318E}'))
        };
        let all = EN.all();
        assert_eq!(all.len(), KO.all().len());
        assert!(all.len() > 400);
        for (name, text) in all {
            assert!(!korean(text), "{name}: {text}");
            assert!(!text.is_empty(), "{name}");
        }
        let en = Lang::En;
        for text in [
            en.song_count(3),
            en.album_count(2),
            en.artist_count(1),
            en.scan_progress(1_234),
            en.loudness_waiting(10),
            en.correction_imported("HD 800 S"),
            en.correction_summary(10, -6.2, 1.5),
            en.correction_unsupported("LP"),
            en.correction_malformed("Filter 1: ON"),
            en.selected_count(4),
            en.playlist_exported(5),
            en.folder_added("Music"),
            en.scan_finished(20, 1, 2),
            en.added_to_queue(2),
            en.added_to_playlist(2, "Mix"),
        ] {
            assert!(!korean(&text), "{text}");
        }
    }
}
