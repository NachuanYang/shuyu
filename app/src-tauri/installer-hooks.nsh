!macro NSIS_HOOK_POSTINSTALL
  ; Use a dedicated ICO path so Windows Search does not keep the old EXE icon cache entry.
  !if "${STARTMENUFOLDER}" != ""
    IfFileExists "$SMPROGRAMS\$AppStartMenuFolder\${PRODUCTNAME}.lnk" 0 ShuyuIconShortcutRefreshDone
    Delete "$SMPROGRAMS\$AppStartMenuFolder\${PRODUCTNAME}.lnk"
    CreateShortcut "$SMPROGRAMS\$AppStartMenuFolder\${PRODUCTNAME}.lnk" "$INSTDIR\${MAINBINARYNAME}.exe" "" "$INSTDIR\shuyu-icon-book.ico" 0
    !insertmacro SetLnkAppUserModelId "$SMPROGRAMS\$AppStartMenuFolder\${PRODUCTNAME}.lnk"
    System::Call 'shell32::SHChangeNotify(i 0x00002000, i 0x00000005, w "$SMPROGRAMS\$AppStartMenuFolder\${PRODUCTNAME}.lnk", p 0)'
  !else
    IfFileExists "$SMPROGRAMS\${PRODUCTNAME}.lnk" 0 ShuyuIconShortcutRefreshDone
    Delete "$SMPROGRAMS\${PRODUCTNAME}.lnk"
    CreateShortcut "$SMPROGRAMS\${PRODUCTNAME}.lnk" "$INSTDIR\${MAINBINARYNAME}.exe" "" "$INSTDIR\shuyu-icon-book.ico" 0
    !insertmacro SetLnkAppUserModelId "$SMPROGRAMS\${PRODUCTNAME}.lnk"
    System::Call 'shell32::SHChangeNotify(i 0x00002000, i 0x00000005, w "$SMPROGRAMS\${PRODUCTNAME}.lnk", p 0)'
  !endif
ShuyuIconShortcutRefreshDone:
  ; Windows Search keeps an app-ID keyed icon cache separate from the Shell cache.
  ; Remove only this application's stale bitmap so Search reloads the refreshed shortcut icon.
  Push $R0
  Push $R1
  Push $R2
  StrCpy $R0 "$LOCALAPPDATA\Packages\Microsoft.Windows.Search_cw5n1h2txyewy\LocalState\AppIconCache"
  IfFileExists "$R0\." 0 ShuyuSearchIconCacheDone
  FindFirst $R1 $R2 "$R0\*"
ShuyuSearchIconCacheLoop:
  StrCmp $R2 "" ShuyuSearchIconCacheClose
  StrCmp $R2 "." ShuyuSearchIconCacheNext
  StrCmp $R2 ".." ShuyuSearchIconCacheNext
  Delete "$R0\$R2\com_caohua_shuyu"
ShuyuSearchIconCacheNext:
  FindNext $R1 $R2
  Goto ShuyuSearchIconCacheLoop
ShuyuSearchIconCacheClose:
  FindClose $R1
ShuyuSearchIconCacheDone:
  Pop $R2
  Pop $R1
  Pop $R0
  ; Make Windows app listings use the same standalone icon file.
  WriteRegStr SHCTX "${UNINSTKEY}" "DisplayIcon" "$INSTDIR\shuyu-icon-book.ico"
  System::Call 'shell32::SHChangeNotify(i 0x00002000, i 0x00000005, w "$INSTDIR\shuyu.exe", p 0)'
  System::Call 'shell32::SHChangeNotify(i 0x00002000, i 0x00000005, w "$INSTDIR\shuyu-icon-book.ico", p 0)'
  System::Call 'shell32::SHChangeNotify(i 0x08000000, i 0, p 0, p 0)'
!macroend
