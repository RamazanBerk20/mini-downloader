Unicode true
RequestExecutionLevel user
SetCompressor /SOLID lzma

!ifndef VERSION
  !error "VERSION is required"
!endif
!ifndef STAGE_DIR
  !error "STAGE_DIR is required"
!endif
!ifndef OUT_DIR
  !error "OUT_DIR is required"
!endif
!ifndef PROJECT_DIR
  !error "PROJECT_DIR is required"
!endif

!include "MUI2.nsh"

Name "Mini Downloader"
OutFile "${OUT_DIR}\Mini.Downloader_${VERSION}_x64-setup.exe"
InstallDir "$LOCALAPPDATA\Programs\Mini Downloader"
InstallDirRegKey HKCU "Software\Mini Downloader" "InstallDir"

!define MUI_ABORTWARNING
!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"

Section "Mini Downloader" SecMain
  IfFileExists "$INSTDIR\mini-downloader.exe" 0 +3
  ExecWait '"$INSTDIR\mini-downloader.exe" --quit'
  Sleep 500
  SetOutPath "$INSTDIR"
  File "${STAGE_DIR}\mini-downloader.exe"
  File "${STAGE_DIR}\minidl-native-host.exe"
  File "${STAGE_DIR}\aria2c.exe"
  File "${STAGE_DIR}\yt-dlp.exe"
  File "/oname=LICENSE.txt" "${PROJECT_DIR}\LICENSE"
  File "/oname=PRIVACY.txt" "${PROJECT_DIR}\PRIVACY.md"
  File "/oname=SECURITY.txt" "${PROJECT_DIR}\SECURITY.md"
  File "/oname=THIRD_PARTY_NOTICES.txt" "${PROJECT_DIR}\THIRD_PARTY_NOTICES.md"
  File "${PROJECT_DIR}\THIRD_PARTY_LICENSES.txt"
  File "${STAGE_DIR}\LICENSE.aria2.txt"
  File "${STAGE_DIR}\NOTICE.aria2-openssl.txt"
  File "${STAGE_DIR}\LICENSE.yt-dlp.txt"

  WriteRegStr HKCU "Software\Mini Downloader" "InstallDir" "$INSTDIR"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Mini Downloader" "DisplayName" "Mini Downloader"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Mini Downloader" "DisplayVersion" "${VERSION}"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Mini Downloader" "Publisher" "RamazanBerk20"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Mini Downloader" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Mini Downloader" "UninstallString" '"$INSTDIR\Uninstall.exe"'
  WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Mini Downloader" "NoModify" 1
  WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Mini Downloader" "NoRepair" 1

  WriteRegStr HKCU "Software\Classes\minidownloader" "" "URL:Mini Downloader Protocol"
  WriteRegStr HKCU "Software\Classes\minidownloader" "URL Protocol" ""
  WriteRegStr HKCU "Software\Classes\minidownloader\shell\open\command" "" '"$INSTDIR\mini-downloader.exe" "%1"'

  CreateDirectory "$SMPROGRAMS\Mini Downloader"
  CreateShortcut "$SMPROGRAMS\Mini Downloader\Mini Downloader.lnk" "$INSTDIR\mini-downloader.exe"
  CreateShortcut "$DESKTOP\Mini Downloader.lnk" "$INSTDIR\mini-downloader.exe"
  WriteUninstaller "$INSTDIR\Uninstall.exe"
SectionEnd

Section "Uninstall"
  ExecWait '"$INSTDIR\mini-downloader.exe" --quit'
  Sleep 500
  Delete "$DESKTOP\Mini Downloader.lnk"
  Delete "$SMPROGRAMS\Mini Downloader\Mini Downloader.lnk"
  RMDir "$SMPROGRAMS\Mini Downloader"
  Delete "$INSTDIR\mini-downloader.exe"
  Delete "$INSTDIR\minidl-native-host.exe"
  Delete "$INSTDIR\aria2c.exe"
  Delete "$INSTDIR\yt-dlp.exe"
  Delete "$INSTDIR\LICENSE.txt"
  Delete "$INSTDIR\PRIVACY.txt"
  Delete "$INSTDIR\SECURITY.txt"
  Delete "$INSTDIR\THIRD_PARTY_NOTICES.txt"
  Delete "$INSTDIR\THIRD_PARTY_LICENSES.txt"
  Delete "$INSTDIR\LICENSE.aria2.txt"
  Delete "$INSTDIR\NOTICE.aria2-openssl.txt"
  Delete "$INSTDIR\LICENSE.yt-dlp.txt"
  Delete "$INSTDIR\Uninstall.exe"
  RMDir "$INSTDIR"
  DeleteRegKey HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Mini Downloader"
  DeleteRegKey HKCU "Software\Mini Downloader"
  DeleteRegKey HKCU "Software\Classes\minidownloader"
  ; The app also registers itself for magnet: links. Remove that handler only
  ; while it still points at this install, never another client's.
  ReadRegStr $0 HKCU "Software\Classes\magnet\shell\open\command" ""
  StrCmp $0 '"$INSTDIR\mini-downloader.exe" "%1"' 0 +2
  DeleteRegKey HKCU "Software\Classes\magnet"
  DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "MiniDownloader"
  DeleteRegKey HKCU "Software\Mozilla\NativeMessagingHosts\com.minidownloader.host"
  DeleteRegKey HKCU "Software\Google\Chrome\NativeMessagingHosts\com.minidownloader.host"
  DeleteRegKey HKCU "Software\Chromium\NativeMessagingHosts\com.minidownloader.host"
  DeleteRegKey HKCU "Software\Microsoft\Edge\NativeMessagingHosts\com.minidownloader.host"
  DeleteRegKey HKCU "Software\BraveSoftware\Brave-Browser\NativeMessagingHosts\com.minidownloader.host"
  DeleteRegKey HKCU "Software\Vivaldi\NativeMessagingHosts\com.minidownloader.host"
  Delete "$APPDATA\minidownloader\native-messaging\com.minidownloader.host.json"
  Delete "$APPDATA\minidownloader\native-messaging\chromium\com.minidownloader.host.json"
  RMDir "$APPDATA\minidownloader\native-messaging\chromium"
  RMDir "$APPDATA\minidownloader\native-messaging"
SectionEnd
