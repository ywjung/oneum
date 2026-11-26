//! What a menu command asks for, whichever menu it comes from: the system menu bar on
//! macOS or the menu bar drawn inside the window elsewhere.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MenuAction {
    Settings,
    OpenFiles,
    AddFolder,
    NewPlaylist,
    /// Open the editor for a new smart playlist.
    NewSmartPlaylist,
    ImportPlaylist,
    Find,
    PlayPause,
    Next,
    Previous,
    VolumeUp,
    VolumeDown,
    ToggleShuffle,
    CycleRepeat,
    StopAfterCurrent,
    /// The next step of marking a section of the current song to repeat (A–B).
    RepeatSection,
    /// Sleep timer in minutes; 0 switches it off.
    SleepTimer(u16),
    SeekForward,
    SeekBack,
    GoToCurrent,
    ToggleQueue,
    ToggleInfo,
    ToggleMeter,
    /// The window as small as the player bar, and the full window again.
    MiniPlayer,
    Equalizer,
    Rescan,
    ShowMainWindow,
    UserGuide,
    ToggleMute,
    /// These act on the playlist that is open in the window.
    PlaylistRename,
    PlaylistExport,
    PlaylistDelete,
    /// The "Song" menu: these act on the rows selected in the window.
    SongPlay,
    SongPlayNext,
    SongAddToQueue,
    SongToggleFavorite,
    SongShowInFinder,
    SongInfo,
}
