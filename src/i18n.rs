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
    ($( $name:ident : $ko:literal, $en:literal; )*) => {
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
    recently_played: "최근 재생", "Recently Played";
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
    seek_bar: "재생 위치", "Playback position";
    nothing_playing: "재생 중인 곡 없음", "Not Playing";
    // Context menu
    play_now: "재생", "Play";
    play_next: "다음에 재생", "Play Next";
    add_to_queue: "대기열 끝에 추가", "Add to Queue";
    add_to_playlist: "플레이리스트에 추가", "Add to Playlist";
    add_favorite: "즐겨찾기에 추가", "Add to Favorites";
    remove_favorite: "즐겨찾기에서 제거", "Remove from Favorites";
    show_in_finder: "Finder에서 보기", "Show in Finder";
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
    about_body: "로컬 음악 파일을 위한 오프라인 플레이어.\n계정, 서버, 사용량 수집이 없습니다.",
        "An offline player for your local music files.\nNo accounts, no servers, no analytics.";
    version: "버전", "Version";
    licenses: "오픈 소스 라이선스", "Open Source Licenses";
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
    info_output: "출력", "Output";
    info_resampled: "리샘플링됨", "resampled";
    info_missing: "파일을 찾을 수 없습니다", "File not found";
    // Notices
    notice_device_lost: "출력 장치 연결이 끊겨 일시정지했습니다.", "The output device was disconnected. Playback is paused.";
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
            (Lang::Ko, 0) => format!("“{name}” 플레이리스트를 가져왔습니다 ({matched}곡)."),
            (Lang::Ko, _) => format!(
                "“{name}” 플레이리스트를 가져왔습니다 ({matched}곡, 찾지 못한 항목 {missing}개)."
            ),
            (Lang::En, 0) => format!("Imported playlist “{name}” ({matched} songs)."),
            (Lang::En, _) => format!(
                "Imported playlist “{name}” ({matched} songs, {missing} entries not found)."
            ),
        }
    }

    pub fn playlist_exported(self, count: usize) -> String {
        match self {
            Lang::Ko => format!("플레이리스트를 내보냈습니다 ({count}곡)."),
            Lang::En => format!("Playlist exported ({count} songs)."),
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
            Lang::En => format!("Scan finished · {} songs in library", group_digits(total)),
        };
        if unsupported > 0 {
            text += &match self {
                Lang::Ko => format!(" · 지원하지 않는 파일 {unsupported}개 제외"),
                Lang::En => format!(" · {unsupported} unsupported files skipped"),
            };
        }
        if errors > 0 {
            text += &match self {
                Lang::Ko => format!(" · 읽지 못한 파일 {errors}개"),
                Lang::En => format!(" · {errors} unreadable files"),
            };
        }
        text
    }

    pub fn artist_summary(self, albums: usize, songs: usize) -> String {
        format!("{} · {}", self.album_count(albums), self.song_count(songs))
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
