; installer.nsi
;
; NSIS 3 installer for shuvarie - Modern UI 2 with a mixed per-user /
; per-machine installation mode (MultiUser.nsh).
;
; Build (from this directory, after building the release binary):
;   makensis /DVERSION=<version> /DBINARY="..\target\release\shuvarie.exe" installer.nsi
;
; Silent install:
;   shuvarie-setup.exe /S [/AllUsers | /CurrentUser]
; A silent install always adds shuvarie to PATH.

; ---------------------------------------------------------------------------
; Compile-time configuration (overridable with /D switches)
; ---------------------------------------------------------------------------

!define /IfNDef VERSION "0.2.0"
!define /IfNDef APP_NAME "shuvarie"
!define /IfNDef EXE_FILE_NAME "shuvarie.exe"
!define /IfNDef PUBLISHER "Charles Dong"
!define /IfNDef APP_URL "https://github.com/shuvarie/shuvarie"
!define /IfNDef SOURCE_ROOT ".."
!define /IfNDef BINARY "${SOURCE_ROOT}/target/release/shuvarie.exe"
!define /IfNDef OUT_FILE "${APP_NAME}-setup-${VERSION}.exe"

; ---------------------------------------------------------------------------
; MultiUser (mixed-mode installer: per-machine or per-user, chosen on a page)
; ---------------------------------------------------------------------------

!define MULTIUSER_EXECUTIONLEVEL Highest

!define MULTIUSER_MUI
!define MULTIUSER_INSTALLMODE_COMMANDLINE ; /AllUsers and /CurrentUser switches
!define MULTIUSER_INSTALLMODE_INSTDIR "${APP_NAME}"
!define MULTIUSER_USE_PROGRAMFILES64

; The ARP (Add/Remove Programs) uninstall entry remembers the previous mode and
; install directory, so upgrades and uninstallers resume in the right place.
!define MULTIUSER_INSTALLMODE_DEFAULT_REGISTRY_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APP_NAME}"
!define MULTIUSER_INSTALLMODE_DEFAULT_REGISTRY_VALUENAME "UninstallString"
!define MULTIUSER_INSTALLMODE_INSTDIR_REGISTRY_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APP_NAME}"
!define MULTIUSER_INSTALLMODE_INSTDIR_REGISTRY_VALUENAME "InstallLocation"

!include "MultiUser.nsh" ; also pulls in MUI2 via MULTIUSER_MUI
!include "MUI2.nsh"
!include "LogicLib.nsh"
!include "WordFunc.nsh"
!include "x64.nsh"

; ---------------------------------------------------------------------------
; General installer attributes
; ---------------------------------------------------------------------------

Name "${APP_NAME}"
OutFile "${OUT_FILE}"

ManifestDPIAware true
SetCompressor /SOLID lzma
ShowInstDetails show
ShowUninstDetails show

; ---------------------------------------------------------------------------
; Pages
; ---------------------------------------------------------------------------

!define MUI_ABORTWARNING

; 1. Welcome
!define MUI_WELCOMEPAGE_TITLE "Welcome to the ${APP_NAME} ${VERSION} Setup Wizard"
!define MUI_WELCOMEPAGE_TEXT "This wizard will install ${APP_NAME} ${VERSION} on your computer.$\r$\n$\r$\n${APP_NAME} is a blazingly fast AI coding TUI for chivalrous people.$\r$\n$\r$\nIt is recommended that you close all other applications before starting Setup.$\r$\n$\r$\n$_CLICK"
!insertmacro MUI_PAGE_WELCOME

; 2. MIT license - "Next" stays disabled until the box is checked
!define MUI_LICENSEPAGE_CHECKBOX
!define MUI_LICENSEPAGE_CHECKBOX_TEXT "I &accept the terms in the License Agreement"
!insertmacro MUI_PAGE_LICENSE "${SOURCE_ROOT}/LICENSE"

; 3. Install for anyone using this computer / just for me
!insertmacro MULTIUSER_PAGE_INSTALLMODE

; 4. Installation folder
!insertmacro MUI_PAGE_DIRECTORY

; 5. Additional tasks: add to PATH (custom nsDialogs page, see TasksPageCreate)
Page custom TasksPageCreate TasksPageLeave

; 6. Install
!insertmacro MUI_PAGE_INSTFILES

; 7. Finish
!define MUI_FINISHPAGE_RUN "$INSTDIR\${EXE_FILE_NAME}"
!define MUI_FINISHPAGE_RUN_TEXT "Launch ${APP_NAME} now"
!define MUI_FINISHPAGE_LINK "Visit the ${APP_NAME} repository"
!define MUI_FINISHPAGE_LINK_LOCATION "${APP_URL}"
!insertmacro MUI_PAGE_FINISH

; Uninstaller pages
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES

!insertmacro MUI_LANGUAGE "English"

; ---------------------------------------------------------------------------
; Version information
; ---------------------------------------------------------------------------

VIProductVersion "${VERSION}.0"
VIAddVersionKey /LANG=1033 "ProductName" "${APP_NAME}"
VIAddVersionKey /LANG=1033 "ProductVersion" "${VERSION}"
VIAddVersionKey /LANG=1033 "CompanyName" "${PUBLISHER}"
VIAddVersionKey /LANG=1033 "FileDescription" "${APP_NAME} ${VERSION} Setup"
VIAddVersionKey /LANG=1033 "FileVersion" "${VERSION}.0"
VIAddVersionKey /LANG=1033 "LegalCopyright" "MIT License, Copyright (c) 2026 ${PUBLISHER}"
VIAddVersionKey /LANG=1033 "OriginalFilename" "${OUT_FILE}"

; ---------------------------------------------------------------------------
; Variables
; ---------------------------------------------------------------------------

Var Dialog
Var PathCheckbox
Var AddToPath ; "1" when the user wants PATH integration, "0" otherwise

; Scratch variables for the PATH helpers (shared by installer / uninstaller)
Var PathKey
Var PathOld
Var PathNew
Var PathLen

; Directory containing the running uninstaller ($INSTDIR in uninstaller code
; is reassigned by MultiUser, so keep our own copy)
Var UninstallSourceDir

; NSIS strings are limited to NSIS_MAX_STRLEN characters; refuse to edit PATH
; values that would not survive a read/modify/write cycle losslessly.
!ifdef NSIS_MAX_STRLEN
  !define /math PATH_LEN_LIMIT ${NSIS_MAX_STRLEN} - 256
!else
  !define PATH_LEN_LIMIT 7936
!endif

!define SMTO_ABORTIFHUNG 0x0002

; Broadcast WM_SETTINGCHANGE so running processes pick up the new PATH.
!macro BroadcastEnvironmentChange
  Push $0
  Push $1
  System::Call 'user32::SendMessageTimeout(p ${HWND_BROADCAST}, i ${WM_SETTINGCHANGE}, p 0, t "Environment", i ${SMTO_ABORTIFHUNG}, i 10000, *p.r0)i.r1'
  Pop $1
  Pop $0
!macroend

; ---------------------------------------------------------------------------
; Installer callbacks
; ---------------------------------------------------------------------------

Function .onInit
  !insertmacro MULTIUSER_INIT

  ; shuvarie is a terminal application and needs a VT-capable console host.
  ${IfNot} ${AtLeastWin10}
    MessageBox MB_OK|MB_ICONSTOP "${APP_NAME} requires Windows 10 or later."
    Quit
  ${EndIf}

  ; Silent installs never see the task page - default to adding PATH.
  ${If} ${Silent}
    ${If} $AddToPath == ""
      StrCpy $AddToPath "1"
    ${EndIf}
  ${EndIf}
FunctionEnd

; ---------------------------------------------------------------------------
; "Additional tasks" page (Add to PATH)
; ---------------------------------------------------------------------------

Function TasksPageCreate
  !insertmacro MUI_HEADER_TEXT "Select Additional Tasks" "Choose which additional tasks Setup should perform."

  nsDialogs::Create 1018
  Pop $Dialog
  ${If} $Dialog == error
    Abort
  ${EndIf}

  ${NSD_CreateLabel} 0 0 100% 24u "Setup can make ${APP_NAME} available from any terminal (Command Prompt, PowerShell, ...) by adding its installation folder to the PATH environment variable."
  Pop $0

  ${NSD_CreateCheckbox} 0 35u 100% 13u "&Add ${APP_NAME} to the PATH environment variable (recommended)"
  Pop $PathCheckbox

  ; Restore the state when the user navigates back to this page
  ${If} $AddToPath == "0"
    ${NSD_SetState} $PathCheckbox ${BST_UNCHECKED}
  ${Else}
    ${NSD_SetState} $PathCheckbox ${BST_CHECKED}
  ${EndIf}

  nsDialogs::Show
FunctionEnd

Function TasksPageLeave
  ${NSD_GetState} $PathCheckbox $0
  ${If} $0 == ${BST_CHECKED}
    StrCpy $AddToPath "1"
  ${Else}
    StrCpy $AddToPath "0"
  ${EndIf}
FunctionEnd

; ---------------------------------------------------------------------------
; PATH helpers
; ---------------------------------------------------------------------------

; Adds $INSTDIR to PATH (deduplicated, case-insensitive) in the registry
; location matching the current installation mode.
Function AddToPath
  ${If} $MultiUser.InstallMode == "AllUsers" ; SHCTX -> HKLM / HKCU accordingly
    StrCpy $PathKey "SYSTEM\CurrentControlSet\Control\Session Manager\Environment"
  ${Else}
    StrCpy $PathKey "Environment"
  ${EndIf}

  ClearErrors
  ReadRegStr $PathOld SHCTX "$PathKey" "PATH" ; REG_EXPAND_SZ is read unexpanded
  ${If} ${Errors}
    StrCpy $PathOld "" ; value not set yet
  ${EndIf}
  ClearErrors

  StrLen $PathLen "$PathOld"
  ${If} $PathLen >= ${PATH_LEN_LIMIT}
    DetailPrint "PATH is too long to edit safely; add $\"$INSTDIR$\" manually."
    ${IfNot} ${Silent}
      MessageBox MB_OK|MB_ICONEXCLAMATION "The PATH environment variable is too long for Setup to edit safely.$\r$\n$\r$\nPlease add $\"$INSTDIR$\" to your PATH manually."
    ${EndIf}
    Return
  ${EndIf}

  ; "E+" = append unless already present (case-insensitive)
  ${WordAdd} "$PathOld" ";" "E+$INSTDIR" $PathNew
  ${If} $PathNew != $PathOld
    WriteRegExpandStr SHCTX "$PathKey" "PATH" "$PathNew"
    !insertmacro BroadcastEnvironmentChange
    DetailPrint 'Added $\"$INSTDIR$\" to the PATH environment variable'
  ${EndIf}
FunctionEnd

; Removes $INSTDIR from PATH (uninstaller counterpart of AddToPath).
Function un.RemoveFromPath
  ${If} $MultiUser.InstallMode == "AllUsers" ; SHCTX -> HKLM / HKCU accordingly
    StrCpy $PathKey "SYSTEM\CurrentControlSet\Control\Session Manager\Environment"
  ${Else}
    StrCpy $PathKey "Environment"
  ${EndIf}

  ClearErrors
  ReadRegStr $PathOld SHCTX "$PathKey" "PATH"
  ${If} ${Errors}
    StrCpy $PathOld ""
  ${EndIf}
  ClearErrors

  StrLen $PathLen "$PathOld"
  ${If} $PathLen >= ${PATH_LEN_LIMIT}
    DetailPrint "PATH is too long to edit safely; remove $\"$INSTDIR$\" manually."
    Return
  ${EndIf}

  ; "E-" = remove all occurrences (case-insensitive)
  ${un.WordAdd} "$PathOld" ";" "E-$INSTDIR" $PathNew
  ${If} $PathNew != $PathOld
    WriteRegExpandStr SHCTX "$PathKey" "PATH" "$PathNew"
    !insertmacro BroadcastEnvironmentChange
    DetailPrint 'Removed $\"$INSTDIR$\" from the PATH environment variable'
  ${EndIf}
FunctionEnd

; ---------------------------------------------------------------------------
; Add/Remove Programs entries
; ---------------------------------------------------------------------------

!macro WriteARPEntries ROOT
  WriteRegStr "${ROOT}" "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APP_NAME}" "DisplayName" "${APP_NAME} ${VERSION}"
  WriteRegStr "${ROOT}" "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APP_NAME}" "DisplayVersion" "${VERSION}"
  WriteRegStr "${ROOT}" "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APP_NAME}" "Publisher" "${PUBLISHER}"
  WriteRegStr "${ROOT}" "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APP_NAME}" "DisplayIcon" "$INSTDIR\${EXE_FILE_NAME}"
  WriteRegStr "${ROOT}" "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APP_NAME}" "InstallLocation" "$INSTDIR"
  WriteRegStr "${ROOT}" "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APP_NAME}" "UninstallString" "$\"$INSTDIR\Uninstall.exe$\""
  WriteRegStr "${ROOT}" "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APP_NAME}" "QuietUninstallString" "$\"$INSTDIR\Uninstall.exe$\" /S"
  WriteRegStr "${ROOT}" "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APP_NAME}" "URLInfoAbout" "${APP_URL}"
  WriteRegStr "${ROOT}" "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APP_NAME}" "HelpLink" "${APP_URL}"
  WriteRegDWORD "${ROOT}" "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APP_NAME}" "NoModify" 1
  WriteRegDWORD "${ROOT}" "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APP_NAME}" "NoRepair" 1
!macroend

; ---------------------------------------------------------------------------
; Install section
; ---------------------------------------------------------------------------

Section -Install
  SetOutPath "$INSTDIR"
  File /oname=${EXE_FILE_NAME} "${BINARY}"
  File "${SOURCE_ROOT}/LICENSE"
  File "${SOURCE_ROOT}/README.md"
  WriteUninstaller "$INSTDIR\Uninstall.exe"

  ; Remember the installation mode next to the uninstaller so that the right
  ; mode is restored even when both a per-machine and a per-user copy exist.
  FileOpen $0 "$INSTDIR\install-mode" w
  FileWrite $0 "$MultiUser.InstallMode"
  FileClose $0

  ; PATH integration (skipped when unchecked on the tasks page)
  ${If} $AddToPath == "1"
    Call AddToPath
  ${EndIf}

  ; Add/Remove Programs entry (HKLM for per-machine, HKCU for per-user)
  ${If} $MultiUser.InstallMode == "AllUsers"
    ${If} ${RunningX64}
      SetRegView 64 ; the app is 64-bit: use the native registry view
    ${EndIf}
    !insertmacro WriteARPEntries HKLM
    SetRegView lastused
  ${Else}
    !insertmacro WriteARPEntries HKCU
  ${EndIf}
SectionEnd

; ---------------------------------------------------------------------------
; Uninstaller
; ---------------------------------------------------------------------------

Function un.onInit
  ; In uninstaller code $INSTDIR initially holds the uninstaller's directory.
  StrCpy $UninstallSourceDir "$INSTDIR"

  !insertmacro MULTIUSER_UNINIT

  ; MultiUser derives the mode from the ARP registry keys. When a per-machine
  ; AND a per-user copy are installed, it guesses per-machine - the marker
  ; file written next to this uninstaller knows the truth.
  ClearErrors
  FileOpen $0 "$UninstallSourceDir\install-mode" r
  ${IfNot} ${Errors}
    FileRead $0 $1
    FileClose $0
    ${If} $1 == "CurrentUser"
      Call un.MultiUser.InstallMode.CurrentUser
    ${ElseIf} $1 == "AllUsers"
      Call un.MultiUser.InstallMode.AllUsers
    ${EndIf}
  ${EndIf}
  ClearErrors
FunctionEnd

Section "Uninstall"
  Call un.RemoveFromPath

  Delete "$INSTDIR\${EXE_FILE_NAME}"
  Delete "$INSTDIR\LICENSE"
  Delete "$INSTDIR\README.md"
  Delete "$INSTDIR\install-mode"
  Delete "$INSTDIR\Uninstall.exe"
  RMDir "$INSTDIR"

  ${If} $MultiUser.InstallMode == "AllUsers"
    ${If} ${RunningX64}
      SetRegView 64
    ${EndIf}
    DeleteRegKey HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APP_NAME}"
    SetRegView lastused
  ${Else}
    DeleteRegKey HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APP_NAME}"
  ${EndIf}
SectionEnd
