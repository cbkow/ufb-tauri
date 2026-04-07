; Union File Browser (Tauri) Installer Script for Inno Setup 6
; https://jrsoftware.org/isinfo.php
;
; Build the app first:  cargo tauri build
; Then compile this with Inno Setup 6.

#define MyAppName "Union File Browser"
#define MyAppVersion "0.2.5"
#define MyAppPublisher "cbkow"
#define MyAppURL "https://github.com/cbkow/ufb"
#define MyAppExeName "ufb-tauri.exe"
#define AgentExeName "mediamount-agent.exe"

; Paths relative to this .iss file
#define SrcTauri "..\src-tauri"
#define ReleaseDir SrcTauri + "\target\release"
#define AgentReleaseDir "..\mediamount-agent\target\release"

[Setup]
AppId={{B3C9D5E7-4F8A-6B2C-9D1E-7A3F5C8E2D4B}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppVerName={#MyAppName} {#MyAppVersion}
AppPublisher={#MyAppPublisher}
AppPublisherURL={#MyAppURL}
AppSupportURL={#MyAppURL}
AppUpdatesURL={#MyAppURL}
AppCopyright=Copyright (C) 2025 {#MyAppPublisher}

DefaultDirName={autopf}\Union File Browser
DefaultGroupName={#MyAppName}
DisableProgramGroupPage=yes

LicenseFile=..\LICENSE
InfoBeforeFile=..\LICENSES\THIRD_PARTY_NOTICES.txt

OutputDir=.
OutputBaseFilename=ufb-tauri-setup-{#MyAppVersion}
Compression=lzma2/max
SolidCompression=yes

SetupIconFile={#SrcTauri}\icons\icon.ico
UninstallDisplayIcon={app}\{#MyAppExeName}
WizardStyle=modern

ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
PrivilegesRequired=admin
MinVersion=10.0.17763

UninstallDisplayName={#MyAppName}
UninstallFilesDir={app}\uninstall

AllowNoIcons=yes
DisableWelcomePage=no

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Types]
Name: "full"; Description: "Full installation (recommended)"
Name: "custom"; Description: "Custom installation"; Flags: iscustom

[Components]
Name: "core"; Description: "Core application files"; Types: full custom; Flags: fixed
Name: "mediamount"; Description: "MediaMount Agent (SMB mount manager)"; Types: full
Name: "uri_protocol"; Description: "Register ufb:/// URI protocol for project links"; Types: full
Name: "union_protocol"; Description: "Register union:/// URI protocol for Union links"; Types: full
Name: "firewall"; Description: "Add Windows Firewall rules for Mesh Sync (TCP 49200, UDP 4244)"; Types: full
Name: "nilesoft"; Description: "Install Nilesoft Shell context menu integration (requires Nilesoft Shell)"; Types: full
Name: "shortcuts"; Description: "Create shortcuts"
Name: "shortcuts\desktop"; Description: "Create desktop shortcut"; Types: full
Name: "shortcuts\startmenu"; Description: "Create Start Menu shortcuts"; Types: full; Flags: fixed

[Tasks]
Name: "mediamount_autostart"; Description: "Start MediaMount Agent at login"; GroupDescription: "MediaMount:"; Components: mediamount
Name: "cleansettings"; Description: "Remove user preferences (%LOCALAPPDATA%\ufb\settings.json) - NOT RECOMMENDED"; GroupDescription: "User data cleanup:"; Flags: unchecked
Name: "cleandb"; Description: "Remove database (%LOCALAPPDATA%\ufb\ufb_v2.db) - NOT RECOMMENDED"; GroupDescription: "User data cleanup:"; Flags: unchecked
Name: "cleanall"; Description: "Remove ALL user data and preferences (%LOCALAPPDATA%\ufb\) - NOT RECOMMENDED"; GroupDescription: "User data cleanup:"; Flags: unchecked
Name: "restartexplorer"; Description: "Restart Windows Explorer (refreshes icons, URI protocols, and shell integrations)"; GroupDescription: "Post-installation:"; Flags: unchecked
Name: "launchafter"; Description: "Launch {#MyAppName} after installation"; GroupDescription: "Post-installation:"; Flags: unchecked

[Files]
; Main executable
Source: "{#ReleaseDir}\{#MyAppExeName}"; DestDir: "{app}"; Flags: ignoreversion; Components: core

; FFmpeg binaries
Source: "{#ReleaseDir}\ffmpeg.exe"; DestDir: "{app}"; Flags: ignoreversion skipifsourcedoesntexist; Components: core
Source: "{#ReleaseDir}\ffprobe.exe"; DestDir: "{app}"; Flags: ignoreversion skipifsourcedoesntexist; Components: core

; ExifTool
Source: "{#ReleaseDir}\exiftool.exe"; DestDir: "{app}"; Flags: ignoreversion skipifsourcedoesntexist; Components: core
Source: "{#ReleaseDir}\exiftool_files\*"; DestDir: "{app}\exiftool_files"; Flags: ignoreversion recursesubdirs createallsubdirs skipifsourcedoesntexist; Components: core

; Runtime DLLs
Source: "{#ReleaseDir}\*.dll"; DestDir: "{app}"; Flags: ignoreversion skipifsourcedoesntexist; Components: core

; MediaMount Agent
Source: "{#AgentReleaseDir}\{#AgentExeName}"; DestDir: "{app}"; Flags: ignoreversion; Components: mediamount

; Assets - scripts
Source: "{#SrcTauri}\assets\scripts\*"; DestDir: "{app}\assets\scripts"; Flags: ignoreversion recursesubdirs; Components: core

; Assets - project templates
Source: "{#SrcTauri}\assets\projectTemplate\*"; DestDir: "{app}\assets\projectTemplate"; Flags: ignoreversion recursesubdirs createallsubdirs; Components: core

; Assets - shell integration files (deployed to app dir, patched and copied to Nilesoft by Pascal script)
Source: "{#SrcTauri}\assets\shell\shell.nss"; DestDir: "{app}\assets\shell"; Flags: ignoreversion; Components: core
Source: "{#SrcTauri}\assets\shell\import\*"; DestDir: "{app}\assets\shell\import"; Flags: ignoreversion; Components: core
Source: "{#SrcTauri}\assets\shell\regEntry\*"; DestDir: "{app}\assets\shell\regEntry"; Flags: ignoreversion skipifsourcedoesntexist; Components: core

; Icons
Source: "{#SrcTauri}\icons\32x32.png"; DestDir: "{app}\icons"; Flags: ignoreversion; Components: core
Source: "{#SrcTauri}\icons\icon.ico"; DestDir: "{app}\icons"; Flags: ignoreversion; Components: core

; Documentation
Source: "..\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"; Flags: ignoreversion; Components: core
Source: "..\LICENSES\*"; DestDir: "{app}\LICENSES"; Flags: ignoreversion recursesubdirs createallsubdirs; Components: core

[Icons]
Name: "{group}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"; Components: shortcuts\startmenu
Name: "{group}\Uninstall {#MyAppName}"; Filename: "{uninstallexe}"; Components: shortcuts\startmenu
Name: "{autodesktop}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"; Components: shortcuts\desktop

[Registry]
; ufb:/// protocol
Root: HKCR; Subkey: "ufb"; ValueType: string; ValueName: ""; ValueData: "URL:Union File Browser Protocol"; Flags: uninsdeletekey; Components: uri_protocol
Root: HKCR; Subkey: "ufb"; ValueType: string; ValueName: "URL Protocol"; ValueData: ""; Components: uri_protocol
Root: HKCR; Subkey: "ufb\DefaultIcon"; ValueType: string; ValueName: ""; ValueData: "{app}\icons\icon.ico,0"; Components: uri_protocol
Root: HKCR; Subkey: "ufb\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\{#MyAppExeName}"" ""%1"""; Components: uri_protocol

; union:/// protocol
Root: HKCR; Subkey: "union"; ValueType: string; ValueName: ""; ValueData: "URL:Union Protocol"; Flags: uninsdeletekey; Components: union_protocol
Root: HKCR; Subkey: "union"; ValueType: string; ValueName: "URL Protocol"; ValueData: ""; Components: union_protocol
Root: HKCR; Subkey: "union\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """powershell.exe"" -NoProfile -ExecutionPolicy Bypass -File ""{app}\assets\scripts\open_union_link.ps1"" ""%1"""; Components: union_protocol

; App User Model ID (Windows 11 taskbar)
Root: HKLM; Subkey: "Software\Classes\Applications\{#MyAppExeName}"; ValueType: string; ValueName: "AppUserModelID"; ValueData: "com.unionfiles.ufb"; Flags: uninsdeletekey

; MediaMount Agent auto-start at login
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "MediaMountAgent"; ValueData: """{app}\{#AgentExeName}"""; Flags: uninsdeletevalue; Components: mediamount; Tasks: mediamount_autostart

[Code]
var
  DataCleanupPage: TInputOptionWizardPage;

procedure InitializeWizard();
begin
  DataCleanupPage := CreateInputOptionPage(wpSelectTasks,
    'User Data Cleanup - WARNING',
    'Carefully review these options before proceeding',
    'The options below will DELETE your user data. This is usually NOT what you want unless you are completely removing the app from your system.',
    False, False);
  DataCleanupPage.Add('I understand that checking data cleanup tasks will DELETE my settings and/or database');
end;

function ShouldSkipPage(PageID: Integer): Boolean;
begin
  if PageID = DataCleanupPage.ID then
    Result := not (WizardIsTaskSelected('cleansettings') or
                   WizardIsTaskSelected('cleandb') or
                   WizardIsTaskSelected('cleanall'))
  else
    Result := False;
end;

procedure RestartExplorer();
var
  ResultCode: Integer;
begin
  Log('Restarting Windows Explorer...');
  Exec('taskkill.exe', '/f /im explorer.exe', '', SW_HIDE, ewWaitUntilTerminated, ResultCode);
  Sleep(500);
  Exec(ExpandConstant('{win}\explorer.exe'), '', '', SW_SHOWNORMAL, ewNoWait, ResultCode);
end;

// Replace placeholder strings in a file
procedure PatchFile(const FileName, InstDir, ExeName: String);
var
  RawContent: AnsiString;
  Content: String;
  PlaceholderDir, PlaceholderExe: String;
begin
  // Build placeholder strings without triggering Inno's brace parser
  PlaceholderDir := Chr(123) + Chr(123) + 'INSTDIR' + Chr(125) + Chr(125);
  PlaceholderExe := Chr(123) + Chr(123) + 'EXENAME' + Chr(125) + Chr(125);
  if LoadStringFromFile(FileName, RawContent) then
  begin
    Content := String(RawContent);
    StringChangeEx(Content, PlaceholderDir, InstDir, True);
    StringChangeEx(Content, PlaceholderExe, ExeName, True);
    SaveStringToFile(FileName, AnsiString(Content), False);
  end;
end;

// Deploy Nilesoft Shell integration files
procedure DeployNilesoftShell(const InstDir: String);
var
  NilesoftDir, ImportsDir, SrcDir: String;
  FindRec: TFindRec;
  DestFile: String;
begin
  NilesoftDir := ExpandConstant('{commonpf}\Nilesoft Shell');
  ImportsDir := NilesoftDir + '\imports';
  SrcDir := InstDir + '\assets\shell\import';

  if not FileExists(NilesoftDir + '\shell.nss') then
  begin
    Log('Nilesoft Shell not found, skipping');
    Exit;
  end;

  // Backup originals
  if FileExists(NilesoftDir + '\shell.nss') and not FileExists(NilesoftDir + '\shell.nss.bak') then
    FileCopy(NilesoftDir + '\shell.nss', NilesoftDir + '\shell.nss.bak', False);
  if FileExists(ImportsDir + '\modify.nss') and not FileExists(ImportsDir + '\modify.nss.bak') then
    FileCopy(ImportsDir + '\modify.nss', ImportsDir + '\modify.nss.bak', False);

  // Copy our shell.nss
  FileCopy(InstDir + '\assets\shell\shell.nss', NilesoftDir + '\shell.nss', False);

  // Create imports dir if missing
  if not DirExists(ImportsDir) then
    ForceDirectories(ImportsDir);

  // Copy and patch each .nss import file
  if FindFirst(SrcDir + '\*.nss', FindRec) then
  begin
    try
      repeat
        DestFile := ImportsDir + '\' + FindRec.Name;
        FileCopy(SrcDir + '\' + FindRec.Name, DestFile, False);
        PatchFile(DestFile, InstDir, '{#MyAppExeName}');
        Log('Deployed: ' + FindRec.Name);
      until not FindNext(FindRec);
    finally
      FindClose(FindRec);
    end;
  end;

  Log('Nilesoft Shell integration complete');
end;

procedure CurStepChanged(CurStep: TSetupStep);
var
  ResultCode: Integer;
  AppDir, LocalAppData, SettingsFile, DbFile: String;
begin
  if CurStep = ssInstall then
  begin
    // Stop running processes before overwriting binaries
    Exec('taskkill.exe', '/f /im {#MyAppExeName}', '', SW_HIDE, ewWaitUntilTerminated, ResultCode);
    Exec('taskkill.exe', '/f /im {#AgentExeName}', '', SW_HIDE, ewWaitUntilTerminated, ResultCode);
    Sleep(500);

    // Clean old program files
    AppDir := ExpandConstant('{app}');
    if DirExists(AppDir) then
      DelTree(AppDir, True, True, True);

    LocalAppData := ExpandConstant('{localappdata}\ufb');

    if WizardIsTaskSelected('cleanall') then
    begin
      if DirExists(LocalAppData) then
        DelTree(LocalAppData, True, True, True);
    end
    else
    begin
      if WizardIsTaskSelected('cleansettings') then
      begin
        SettingsFile := LocalAppData + '\settings.json';
        if FileExists(SettingsFile) then
          DeleteFile(SettingsFile);
      end;
      if WizardIsTaskSelected('cleandb') then
      begin
        // Current database
        DbFile := LocalAppData + '\ufb_v2.db';
        if FileExists(DbFile) then DeleteFile(DbFile);
        if FileExists(DbFile + '-wal') then DeleteFile(DbFile + '-wal');
        if FileExists(DbFile + '-shm') then DeleteFile(DbFile + '-shm');
        // Legacy database
        DbFile := LocalAppData + '\ufb.db';
        if FileExists(DbFile) then DeleteFile(DbFile);
        if FileExists(DbFile + '-wal') then DeleteFile(DbFile + '-wal');
        if FileExists(DbFile + '-shm') then DeleteFile(DbFile + '-shm');
      end;
    end;
  end;

  if CurStep = ssPostInstall then
  begin
    if WizardIsComponentSelected('nilesoft') then
      DeployNilesoftShell(ExpandConstant('{app}'));

    if WizardIsComponentSelected('firewall') then
    begin
      Exec('netsh.exe', 'advfirewall firewall add rule name="UFB Mesh Sync (TCP)" dir=in action=allow protocol=TCP localport=49200 program="' + ExpandConstant('{app}\{#MyAppExeName}') + '"', '', SW_HIDE, ewWaitUntilTerminated, ResultCode);
      Exec('netsh.exe', 'advfirewall firewall add rule name="UFB Mesh Sync (UDP)" dir=in action=allow protocol=UDP localport=4244 program="' + ExpandConstant('{app}\{#MyAppExeName}') + '"', '', SW_HIDE, ewWaitUntilTerminated, ResultCode);
    end;

    if WizardIsTaskSelected('restartexplorer') then
      RestartExplorer();
  end;
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  ResultCode: Integer;
  UserDataDir, NilesoftDir: String;
  Response: Integer;
begin
  if CurUninstallStep = usUninstall then
  begin
    // Stop the MediaMount agent if running
    Exec('taskkill.exe', '/f /im {#AgentExeName}', '', SW_HIDE, ewWaitUntilTerminated, ResultCode);
  end;

  if CurUninstallStep = usPostUninstall then
  begin

    // Remove MediaMount auto-start
    RegDeleteValue(HKCU, 'Software\Microsoft\Windows\CurrentVersion\Run', 'MediaMountAgent');

    // Remove firewall rules
    Exec('netsh.exe', 'advfirewall firewall delete rule name="UFB Mesh Sync (TCP)"', '', SW_HIDE, ewWaitUntilTerminated, ResultCode);
    Exec('netsh.exe', 'advfirewall firewall delete rule name="UFB Mesh Sync (UDP)"', '', SW_HIDE, ewWaitUntilTerminated, ResultCode);

    // Restore Nilesoft Shell backups
    NilesoftDir := ExpandConstant('{commonpf}\Nilesoft Shell');
    if FileExists(NilesoftDir + '\shell.nss.bak') then
    begin
      FileCopy(NilesoftDir + '\shell.nss.bak', NilesoftDir + '\shell.nss', False);
      DeleteFile(NilesoftDir + '\shell.nss.bak');
    end;
    if FileExists(NilesoftDir + '\imports\modify.nss.bak') then
    begin
      FileCopy(NilesoftDir + '\imports\modify.nss.bak', NilesoftDir + '\imports\modify.nss', False);
      DeleteFile(NilesoftDir + '\imports\modify.nss.bak');
    end;

    // Remove our NSS files
    DeleteFile(NilesoftDir + '\imports\union_files.nss');
    DeleteFile(NilesoftDir + '\imports\union_folders.nss');
    DeleteFile(NilesoftDir + '\imports\union_projects.nss');
    DeleteFile(NilesoftDir + '\imports\union_goto.nss');
    DeleteFile(NilesoftDir + '\imports\union_terminal.nss');
    DeleteFile(NilesoftDir + '\imports\taskbar.nss');
    DeleteFile(NilesoftDir + '\imports\modify.nss');

    // Prompt to delete user data
    UserDataDir := ExpandConstant('{localappdata}\ufb');
    if DirExists(UserDataDir) then
    begin
      Response := MsgBox('Do you want to delete your user data, settings, and database?' + #13#10 +
                         'Location: ' + UserDataDir + #13#10#13#10 +
                         'Choose "Yes" for a clean uninstall.' + #13#10 +
                         'Choose "No" to keep data for future installations (RECOMMENDED).',
                         mbConfirmation, MB_YESNO);
      if Response = IDYES then
        DelTree(UserDataDir, True, True, True);
    end;
  end;
end;

[Run]
Filename: "{app}\{#MyAppExeName}"; Description: "{cm:LaunchProgram,{#StringChange(MyAppName, '&', '&&')}}"; Flags: nowait postinstall skipifsilent; Tasks: launchafter

[UninstallDelete]
Type: filesandordirs; Name: "{app}\cache"
Type: filesandordirs; Name: "{app}\temp"
Type: filesandordirs; Name: "{app}\logs"

[Messages]
WelcomeLabel2=This will install [name/ver] on your computer.%n%nUnion File Browser is a file browser and project management tool designed for visual effects and post-production workflows.%n%nIt is recommended that you close all other applications before continuing.
FinishedLabel=Setup has finished installing [name] on your computer.%n%nThe application may be launched by selecting the installed shortcuts.
