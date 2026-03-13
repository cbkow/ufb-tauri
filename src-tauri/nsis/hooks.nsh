!macro NSIS_HOOK_POSTINSTALL
  ; ── FFmpeg binaries + runtime DLLs (next to main exe) ──
  SetOutPath $INSTDIR
  File /nonfatal /oname=ffmpeg.exe "${MAINBINARYSRCPATH}\..\ffmpeg.exe"
  File /nonfatal /oname=ffprobe.exe "${MAINBINARYSRCPATH}\..\ffprobe.exe"
  File /nonfatal /oname=avcodec-62.dll "${MAINBINARYSRCPATH}\..\avcodec-62.dll"
  File /nonfatal /oname=avdevice-62.dll "${MAINBINARYSRCPATH}\..\avdevice-62.dll"
  File /nonfatal /oname=avfilter-11.dll "${MAINBINARYSRCPATH}\..\avfilter-11.dll"
  File /nonfatal /oname=avformat-62.dll "${MAINBINARYSRCPATH}\..\avformat-62.dll"
  File /nonfatal /oname=avutil-60.dll "${MAINBINARYSRCPATH}\..\avutil-60.dll"
  File /nonfatal /oname=swresample-6.dll "${MAINBINARYSRCPATH}\..\swresample-6.dll"
  File /nonfatal /oname=swscale-9.dll "${MAINBINARYSRCPATH}\..\swscale-9.dll"

  ; ── ufb:// protocol handler (opens in app) ──
  WriteRegStr HKCR "ufb" "" "URL:UFB Protocol"
  WriteRegStr HKCR "ufb" "URL Protocol" ""
  WriteRegStr HKCR "ufb\shell\open\command" "" \
    '"$INSTDIR\${MAINBINARYNAME}.exe" "%1"'

  ; ── union:// protocol handler (opens in Explorer, not in app) ──
  WriteRegStr HKCR "union" "" "URL:Union Protocol"
  WriteRegStr HKCR "union" "URL Protocol" ""
  WriteRegStr HKCR "union\shell\open\command" "" \
    '"powershell.exe" -NoProfile -ExecutionPolicy Bypass -File "$INSTDIR\assets\scripts\open_union_link.ps1" "%1"'

  ; ── Firewall rules for Mesh Sync ──
  nsExec::ExecToLog 'netsh advfirewall firewall add rule name="UFB Mesh Sync (TCP)" dir=in action=allow protocol=TCP localport=49200 program="$INSTDIR\${MAINBINARYNAME}.exe"'
  nsExec::ExecToLog 'netsh advfirewall firewall add rule name="UFB Mesh Sync (UDP)" dir=in action=allow protocol=UDP localport=4244 program="$INSTDIR\${MAINBINARYNAME}.exe"'

  ; ── Nilesoft Shell integration ──
  nsExec::ExecToLog 'powershell.exe -NoProfile -ExecutionPolicy Bypass -File "$INSTDIR\assets\scripts\install_nilesoft.ps1" -InstDir "$INSTDIR" -ExeName "${MAINBINARYNAME}.exe"'
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  DeleteRegKey HKCR "ufb"
  DeleteRegKey HKCR "union"
  nsExec::ExecToLog 'netsh advfirewall firewall delete rule name="UFB Mesh Sync (TCP)"'
  nsExec::ExecToLog 'netsh advfirewall firewall delete rule name="UFB Mesh Sync (UDP)"'

  ; ── FFmpeg cleanup ──
  Delete "$INSTDIR\ffmpeg.exe"
  Delete "$INSTDIR\ffprobe.exe"
  Delete "$INSTDIR\avcodec-62.dll"
  Delete "$INSTDIR\avdevice-62.dll"
  Delete "$INSTDIR\avfilter-11.dll"
  Delete "$INSTDIR\avformat-62.dll"
  Delete "$INSTDIR\avutil-60.dll"
  Delete "$INSTDIR\swresample-6.dll"
  Delete "$INSTDIR\swscale-9.dll"

  ; ── Nilesoft Shell cleanup ──
  nsExec::ExecToLog 'powershell.exe -NoProfile -ExecutionPolicy Bypass -File "$INSTDIR\assets\scripts\uninstall_nilesoft.ps1"'

  ; ── Prompt to delete user data ──
  MessageBox MB_YESNO "Delete user data (settings, database) from $LOCALAPPDATA\ufb?$\n$\nChoose No to keep data for future installations (recommended)." IDYES delete_userdata IDNO skip_userdata
  delete_userdata:
    RMDir /r "$LOCALAPPDATA\ufb"
  skip_userdata:
!macroend
