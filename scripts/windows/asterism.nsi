; The Windows installer: Asterism for the current user only (no administrator rights), a Start menu shortcut and an
; uninstaller listed in Settings > Apps. Built in CI with:
;   makensis /DVERSION=0.1.0 /DEXE=path\to\asterism.exe /DOUTFILE=Asterism-0.1.0-windows-x64-setup.exe asterism.nsi
; Run with /S it installs silently (the app's own updates do that), then starts Asterism again.
Unicode true
!include "FileFunc.nsh"

!define NAME "Asterism"
!define KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\${NAME}"
!ifndef VERSION
  !define VERSION "0.0.0"
!endif

Name "${NAME}"
OutFile "${OUTFILE}"
InstallDir "$LOCALAPPDATA\Programs\${NAME}"
RequestExecutionLevel user
SetCompressor /SOLID lzma
Icon "..\..\crates\app\assets\icon.ico"
UninstallIcon "..\..\crates\app\assets\icon.ico"
VIProductVersion "${VERSION}.0"
VIAddVersionKey "ProductName" "${NAME}"
VIAddVersionKey "FileDescription" "${NAME} installer"
VIAddVersionKey "ProductVersion" "${VERSION}"
VIAddVersionKey "FileVersion" "${VERSION}"
VIAddVersionKey "LegalCopyright" "MIT License"

Page directory
Page instfiles
UninstPage uninstConfirm
UninstPage instfiles

Section
  SetOutPath "$INSTDIR"
  ; An update quits the app before running this; still, give its process a moment to let go of the exe.
  Sleep 500
  File "/oname=asterism.exe" "${EXE}"
  File "/oname=icon.ico" "..\..\crates\app\assets\icon.ico"
  WriteUninstaller "$INSTDIR\uninstall.exe"
  CreateShortcut "$SMPROGRAMS\${NAME}.lnk" "$INSTDIR\asterism.exe" "" "$INSTDIR\icon.ico"
  WriteRegStr HKCU "${KEY}" "DisplayName" "${NAME}"
  WriteRegStr HKCU "${KEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKCU "${KEY}" "Publisher" "flav-code"
  WriteRegStr HKCU "${KEY}" "DisplayIcon" "$INSTDIR\icon.ico"
  WriteRegStr HKCU "${KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "${KEY}" "UninstallString" '"$INSTDIR\uninstall.exe"'
  WriteRegStr HKCU "${KEY}" "QuietUninstallString" '"$INSTDIR\uninstall.exe" /S'
  WriteRegDWORD HKCU "${KEY}" "NoModify" 1
  WriteRegDWORD HKCU "${KEY}" "NoRepair" 1
  ${GetSize} "$INSTDIR" "/S=0K" $0 $1 $2
  WriteRegDWORD HKCU "${KEY}" "EstimatedSize" "$0"
SectionEnd

; Start Asterism once installed (after a silent update too).
Function .onInstSuccess
  Exec '"$INSTDIR\asterism.exe"'
FunctionEnd

Section "Uninstall"
  Delete "$INSTDIR\asterism.exe"
  Delete "$INSTDIR\icon.ico"
  Delete "$INSTDIR\uninstall.exe"
  RMDir "$INSTDIR"
  Delete "$SMPROGRAMS\${NAME}.lnk"
  DeleteRegKey HKCU "${KEY}"
SectionEnd
