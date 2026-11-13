//! What a menu command asks for, whichever menu it comes from: the system menu bar on
//! macOS or the menu bar drawn inside the window elsewhere.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MenuAction {
    Settings,
    OpenFiles,
    AddFolder,
    NewPlaylist,
    ImportPlaylist,
    Find,
    PlayPause,
    Next,
    Previous,
    VolumeUp,
    VolumeDown,
    ToggleShuffle,
    CycleRepeat,
    GoToCurrent,
    ToggleQueue,
    ToggleInfo,
    ToggleMeter,
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
