## What Is Different on Windows
- **Installing**: run `OnEum-…-setup.exe` and OnEum appears in the Start menu (no administrator rights are needed). To remove it, use **Settings → Apps → Installed apps** in Windows. Removing asks whether the library and the settings are to go as well; your music files are never touched. To use OnEum without installing it, unzip the zip file and run `OnEum.exe`.
- The menu bar is at the top of the window. Shortcuts use the Ctrl key.
- Closing the window quits the program and stops playback. What this guide says about the Dock does not apply to Windows.
- Drop music files onto `OnEum.exe`, or open them with "Open with", to play them.
- The media keys of the keyboard and the media controls of Windows (shown when you change the volume, and in the taskbar) play, pause, skip and move within the song. Opening music files with OnEum while it is open already plays them in the open window instead of starting another one.
- In Output Quality, **Match the Song's Sample Rate** and **Exclusive Access** work only on macOS. On Windows, songs are converted to the sample rate the system has set. **Memory Playback** and the VU meter work the same.
- Your data is in `%APPDATA%\OnEum` (library and settings) and `%LOCALAPPDATA%\OnEum` (image cache and logs).
- If an error window appears at start-up, press Ctrl+C in it to copy its text. If it says no graphics device could be set up, update the graphics driver or, in a virtual machine, turn on 3D acceleration, then start OnEum again. The details are in `%LOCALAPPDATA%\OnEum\Logs\oneum.log`.
- The Windows edition is built from the same code as the Mac edition, but this version has not yet been checked on real Windows.
