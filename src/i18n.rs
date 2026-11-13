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
    show_in_finder: if cfg!(windows) { "탐색기에서 보기" } else { "Finder에서 보기" },
        if cfg!(windows) { "Show in Explorer" } else { "Show in Finder" };
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
    rg_hint: "파일에 이미 들어 있는 ReplayGain 태그만 사용합니다. 태그가 없는 곡은 보정하지 않습니다.",
        "Uses ReplayGain tags already present in your files. Songs without tags are not adjusted.";
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
    confirm_remove_tracks_body: "라이브러리와 플레이리스트에서만 빠집니다. 원본 파일은 삭제되지 않습니다.",
        "They are only removed from the library and playlists. The original files are not deleted.";
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
    info_proc_limiter: "피크 보호", "Peak protection";
    info_proc_eq: "이퀄라이저", "Equalizer";
    info_signal_path: "신호 경로", "Signal Path";
    info_lyrics: "가사", "Lyrics";
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
    // Radio
    radio: "라디오", "Radio";
    radio_row: "대기열이 끝나면 이어서 재생", "Keep Playing When the Queue Ends";
    radio_desc: "마지막 곡과 어울리는 곡을 라이브러리에서 골라 대기열에 더합니다.", "Adds songs from your library that go with the last one.";
    radio_hint: "장르·아티스트·연도가 가까운 곡을 고르고, 방금 들은 곡과 같은 아티스트의 연속은 피합니다. 인터넷을 쓰지 않으며, 반복 재생이 켜져 있으면 동작하지 않습니다.",
        "Picks songs close in genre, artist and year, and avoids what was just heard and long runs of one artist. Nothing goes online. Does nothing while Repeat is on.";
    // Equalizer
    equalizer: "이퀄라이저", "Equalizer";
    menu_equalizer: "이퀄라이저…", "Equalizer…";
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
}
