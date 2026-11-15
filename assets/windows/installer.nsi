; The setup program of OnEum for Windows (NSIS 3, https://nsis.sourceforge.io).
; Built by scripts/package-windows.sh, which passes:
;   VERSION   the version, as in Cargo.toml ("0.1.0")
;   STAGE     the folder that holds what is installed (OnEum.exe, the license, the notices)
;   OUTFILE   the setup program to write
;
; It installs for the person who runs it, without asking for an administrator:
; the program goes into %LOCALAPPDATA%\Programs\OnEum, a shortcut into the Start menu,
; and an entry into "Installed apps" through which it is removed again. The library and
; the settings (%APPDATA%\OnEum, %LOCALAPPDATA%\OnEum) are not part of the program:
; removing OnEum asks whether they are to go as well, and keeps them unless told to.

Unicode true
SetCompressor /SOLID lzma
RequestExecutionLevel user

!include "MUI2.nsh"
!include "x64.nsh"
!include "WinVer.nsh"
!include "FileFunc.nsh"

!ifndef VERSION
  !error "VERSION is not defined (see scripts/package-windows.sh)"
!endif
!ifndef STAGE
  !error "STAGE is not defined (see scripts/package-windows.sh)"
!endif
!ifndef OUTFILE
  !error "OUTFILE is not defined (see scripts/package-windows.sh)"
!endif

!define APP "OnEum"
!define EXE "OnEum.exe"
!define UNINSTALL_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APP}"
!define APP_KEY "Software\${APP}"

Name "${APP}"
OutFile "${OUTFILE}"
InstallDir "$LOCALAPPDATA\Programs\${APP}"
; An earlier installation says where it went: an update goes to the same place.
InstallDirRegKey HKCU "${APP_KEY}" "InstallDir"
BrandingText "${APP} ${VERSION}"
ShowInstDetails nevershow
ShowUninstDetails nevershow

VIProductVersion "${VERSION}.0"
VIAddVersionKey "ProductName" "${APP}"
VIAddVersionKey "FileDescription" "${APP} Setup"
VIAddVersionKey "FileVersion" "${VERSION}"
VIAddVersionKey "ProductVersion" "${VERSION}"
VIAddVersionKey "LegalCopyright" "MIT License"

; ----- pages ---------------------------------------------------------------------------

!define MUI_ICON "oneum.ico"
!define MUI_UNICON "oneum.ico"
!define MUI_ABORTWARNING

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_LICENSE "${STAGE}/LICENSE.txt"
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
; The last page: start OnEum, and (not ticked) a shortcut on the desktop.
!define MUI_FINISHPAGE_RUN "$INSTDIR\${EXE}"
!define MUI_FINISHPAGE_SHOWREADME ""
!define MUI_FINISHPAGE_SHOWREADME_NOTCHECKED
!define MUI_FINISHPAGE_SHOWREADME_TEXT "$(DesktopShortcut)"
!define MUI_FINISHPAGE_SHOWREADME_FUNCTION MakeDesktopShortcut
!insertmacro MUI_PAGE_FINISH

!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES

; The language is chosen by the language Windows is set to (English if it is neither).
!insertmacro MUI_LANGUAGE "English"
!insertmacro MUI_LANGUAGE "Korean"

LangString DesktopShortcut ${LANG_ENGLISH} "Create a shortcut on the desktop"
LangString DesktopShortcut ${LANG_KOREAN} "바탕 화면에 바로 가기 만들기"
LangString Needs64Bit ${LANG_ENGLISH} "OnEum needs a 64-bit version of Windows."
LangString Needs64Bit ${LANG_KOREAN} "OnEum은 64비트 Windows에서만 실행됩니다."
LangString NeedsWindows10 ${LANG_ENGLISH} "OnEum needs Windows 10 or later."
LangString NeedsWindows10 ${LANG_KOREAN} "OnEum은 Windows 10 이상에서 실행됩니다."
LangString StillRunning ${LANG_ENGLISH} "OnEum is running. Close it, then choose Retry."
LangString StillRunning ${LANG_KOREAN} "OnEum이 실행 중입니다. 닫은 뒤 다시 시도를 누르세요."
LangString RemoveData ${LANG_ENGLISH} "Also remove the library and the settings of OnEum (favorites, play counts, playlists)?$\r$\n$\r$\nYour music files are never touched."
LangString RemoveData ${LANG_KOREAN} "OnEum의 라이브러리와 설정(즐겨찾기, 재생 횟수, 플레이리스트)도 지울까요?$\r$\n$\r$\n음악 파일은 건드리지 않습니다."

; ----- installing ----------------------------------------------------------------------

Function .onInit
  ${IfNot} ${RunningX64}
    MessageBox MB_OK|MB_ICONSTOP "$(Needs64Bit)"
    Abort
  ${EndIf}
  ${IfNot} ${AtLeastWin10}
    MessageBox MB_OK|MB_ICONSTOP "$(NeedsWindows10)"
    Abort
  ${EndIf}
FunctionEnd

; The program cannot be replaced (or removed) while it runs: Windows keeps its file
; open. Asks to close it until the file can be opened for writing.
!macro WaitUntilClosed
  ${Do}
    ${IfNot} ${FileExists} "$INSTDIR\${EXE}"
      ${ExitDo}
    ${EndIf}
    ClearErrors
    FileOpen $0 "$INSTDIR\${EXE}" a
    ${IfNot} ${Errors}
      FileClose $0
      ${ExitDo}
    ${EndIf}
    MessageBox MB_RETRYCANCEL|MB_ICONEXCLAMATION "$(StillRunning)" IDRETRY +2
    Abort
  ${Loop}
!macroend

Section "OnEum" Main
  SectionIn RO
  !insertmacro WaitUntilClosed
  SetOutPath "$INSTDIR"
  File "${STAGE}/${EXE}"
  File "${STAGE}/LICENSE.txt"
  File "${STAGE}/THIRD_PARTY_NOTICES.md"
  File "${STAGE}/THIRD_PARTY_LICENSES.txt"
  WriteUninstaller "$INSTDIR\Uninstall.exe"

  CreateShortcut "$SMPROGRAMS\${APP}.lnk" "$INSTDIR\${EXE}"

  WriteRegStr HKCU "${APP_KEY}" "InstallDir" "$INSTDIR"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayName" "${APP}"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "Publisher" "${APP}"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayIcon" "$INSTDIR\${EXE}"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "UninstallString" '"$INSTDIR\Uninstall.exe"'
  WriteRegStr HKCU "${UNINSTALL_KEY}" "QuietUninstallString" '"$INSTDIR\Uninstall.exe" /S'
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "NoModify" 1
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "NoRepair" 1
  ${GetSize} "$INSTDIR" "/S=0K" $0 $1 $2
  IntFmt $0 "0x%08X" $0
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "EstimatedSize" "$0"
SectionEnd

Function MakeDesktopShortcut
  CreateShortcut "$DESKTOP\${APP}.lnk" "$INSTDIR\${EXE}"
FunctionEnd

; ----- removing ------------------------------------------------------------------------

Section "Uninstall"
  !insertmacro WaitUntilClosed
  Delete "$INSTDIR\${EXE}"
  Delete "$INSTDIR\LICENSE.txt"
  Delete "$INSTDIR\THIRD_PARTY_NOTICES.md"
  Delete "$INSTDIR\THIRD_PARTY_LICENSES.txt"
  Delete "$INSTDIR\Uninstall.exe"
  ; Only if nothing else was put into it.
  RMDir "$INSTDIR"

  Delete "$SMPROGRAMS\${APP}.lnk"
  Delete "$DESKTOP\${APP}.lnk"
  DeleteRegKey HKCU "${UNINSTALL_KEY}"
  DeleteRegKey HKCU "${APP_KEY}"

  ; What the user made with the program stays unless they say otherwise (never when
  ; removed without questions, as an update does).
  ${IfNot} ${Silent}
    MessageBox MB_YESNO|MB_ICONQUESTION|MB_DEFBUTTON2 "$(RemoveData)" IDNO keep
      RMDir /r "$APPDATA\${APP}"
      RMDir /r "$LOCALAPPDATA\${APP}"
    keep:
  ${EndIf}
SectionEnd
