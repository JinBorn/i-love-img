Unicode true
!include "MUI2.nsh"

!ifndef STAGE_DIR
  !error "STAGE_DIR is required"
!endif
!ifndef OUTPUT_FILE
  !error "OUTPUT_FILE is required"
!endif
!ifndef BUILD_VERSION
  !error "BUILD_VERSION is required"
!endif
!ifndef UNINSTALL_MANIFEST
  !error "UNINSTALL_MANIFEST is required"
!endif

Name "Local Image Tools"
OutFile "${OUTPUT_FILE}"
InstallDir "$LOCALAPPDATA\Programs\Local Image Tools"
InstallDirRegKey HKCU "Software\LocalImageTools" "InstallDir"
RequestExecutionLevel user
SetCompressor zlib
!ifdef APP_ICON
  Icon "${APP_ICON}"
  UninstallIcon "${APP_ICON}"
!endif

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_LICENSE "${STAGE_DIR}\LICENSE"
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"
!insertmacro MUI_LANGUAGE "SimpChinese"

Section "Local Image Tools" Main
  SetShellVarContext current
  SetOutPath "$INSTDIR"
  File /r "${STAGE_DIR}\*"
  WriteUninstaller "$INSTDIR\Uninstall.exe"
  CreateDirectory "$SMPROGRAMS\Local Image Tools"
  CreateShortcut "$SMPROGRAMS\Local Image Tools\Local Image Tools.lnk" "$INSTDIR\local-image-tools.exe"
  CreateShortcut "$SMPROGRAMS\Local Image Tools\Uninstall.lnk" "$INSTDIR\Uninstall.exe"
  WriteRegStr HKCU "Software\LocalImageTools" "InstallDir" "$INSTDIR"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\LocalImageTools" "DisplayName" "Local Image Tools"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\LocalImageTools" "DisplayVersion" "${BUILD_VERSION}"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\LocalImageTools" "DisplayIcon" "$INSTDIR\local-image-tools.exe"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\LocalImageTools" "UninstallString" '$\"$INSTDIR\Uninstall.exe$\"'
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\LocalImageTools" "QuietUninstallString" '$\"$INSTDIR\Uninstall.exe$\" /S'
  WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\LocalImageTools" "NoModify" 1
  WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\LocalImageTools" "NoRepair" 1
SectionEnd

Section "Uninstall"
  SetShellVarContext current
  !include "${UNINSTALL_MANIFEST}"
  Delete "$INSTDIR\Uninstall.exe"
  RMDir "$INSTDIR"
  Delete "$SMPROGRAMS\Local Image Tools\Local Image Tools.lnk"
  Delete "$SMPROGRAMS\Local Image Tools\Uninstall.lnk"
  RMDir "$SMPROGRAMS\Local Image Tools"
  DeleteRegKey HKCU "Software\LocalImageTools"
  DeleteRegKey HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\LocalImageTools"
SectionEnd
