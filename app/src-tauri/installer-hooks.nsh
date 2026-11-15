; Blackbox installer hooks.
;
; The Tauri uninstaller already removes the "Blackbox" autostart entry.
; Its "delete app data" checkbox only covers folders named after the bundle
; identifier; Blackbox keeps its data in %APPDATA%\Blackbox and
; %LOCALAPPDATA%\Blackbox (database, config snapshots, logs), so those are
; removed here under the same conditions: checkbox ticked, not an update.

!macro NSIS_HOOK_POSTUNINSTALL
  ${If} $DeleteAppDataCheckboxState = 1
  ${AndIf} $UpdateMode <> 1
    SetShellVarContext current
    RmDir /r "$APPDATA\Blackbox"
    RmDir /r "$LOCALAPPDATA\Blackbox"
  ${EndIf}
!macroend
