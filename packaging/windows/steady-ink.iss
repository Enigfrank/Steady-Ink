#define MyAppName "Steady Ink"
#ifndef MyAppVersion
#error MyAppVersion must be defined by scripts/release/package.ps1
#endif
#ifndef SourceDir
#define SourceDir "..\.."
#endif
#ifndef OutputDir
#define OutputDir "dist"
#endif

[Setup]
AppId={{8B5B8F72-22A6-4E79-9BB7-4B5B5B3E9AA5}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppVerName={#MyAppName} {#MyAppVersion}
AppPublisher=Enigfrank
AppPublisherURL=https://github.com/Enigfrank/Steady-Ink
AppSupportURL=https://github.com/Enigfrank/Steady-Ink/issues
DefaultDirName={autopf}\Steady Ink
DefaultGroupName={#MyAppName}
DisableProgramGroupPage=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
PrivilegesRequired=admin
OutputDir={#OutputDir}
OutputBaseFilename=Steady-Ink-{#MyAppVersion}-Setup
SetupIconFile={#SourceDir}\assets\steady-ink-icon.ico
UninstallDisplayIcon={app}\steady-ink.exe
VersionInfoVersion={#MyAppVersion}.0
VersionInfoCompany=Enigfrank
VersionInfoCopyright=Copyright (C) 2026 Enigfrank
VersionInfoDescription={#MyAppName} installer
VersionInfoProductName={#MyAppName}
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
CloseApplications=yes
RestartApplications=no
ChangesAssociations=no

[Languages]
Name: "en"; MessagesFile: "compiler:Default.isl"
Name: "chinesesimp"; MessagesFile: "{#SourceDir}\packaging\windows\Languages\ChineseSimplified.isl"

[CustomMessages]
en.AdditionalIcons=Additional shortcuts:
en.DesktopIconTask=Create a desktop shortcut
en.StartMenuIconTask=Create a Start Menu shortcut
en.LaunchProgram=Run %1
chinesesimp.AdditionalIcons=附加快捷方式：
chinesesimp.DesktopIconTask=创建桌面快捷方式
chinesesimp.StartMenuIconTask=创建开始菜单快捷方式
chinesesimp.LaunchProgram=运行 %1

[Tasks]
Name: "desktopicon"; Description: "{cm:DesktopIconTask}"; GroupDescription: "{cm:AdditionalIcons}"
Name: "startmenuicon"; Description: "{cm:StartMenuIconTask}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#SourceDir}\target\release\steady-ink.exe"; DestDir: "{app}"; Flags: ignoreversion

[InstallDelete]
Type: files; Name: "{app}\LICENSE"
Type: files; Name: "{app}\README.en.md"
Type: files; Name: "{app}\README.zh-CN.md"
Type: files; Name: "{app}\steady-ink-icon.ico"
Type: files; Name: "{app}\assets\steady-ink-icon.svg"
Type: dirifempty; Name: "{app}\assets"

[Icons]
Name: "{commondesktop}\{#MyAppName}"; Filename: "{app}\steady-ink.exe"; IconFilename: "{app}\steady-ink.exe"; IconIndex: 0; Tasks: desktopicon
Name: "{commonprograms}\{#MyAppName}\{#MyAppName}"; Filename: "{app}\steady-ink.exe"; IconFilename: "{app}\steady-ink.exe"; IconIndex: 0; Tasks: startmenuicon

[Registry]
; Only a fresh installation creates the default value. Upgrades preserve the actual HKLM state.
Root: HKLM64; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "Steady Ink"; ValueData: """{app}\steady-ink.exe"""; Flags: uninsdeletevalue; Check: ShouldCreateAutostart

[Run]
Filename: "{app}\steady-ink.exe"; Description: "{cm:LaunchProgram,{#MyAppName}}"; Flags: nowait postinstall skipifsilent runasoriginaluser

[Code]
const
  AutostartSubkey = 'Software\Microsoft\Windows\CurrentVersion\Run';
  AutostartValueName = 'Steady Ink';
var
  AutostartValueExists: Boolean;
  ExistingInstallation: Boolean;

// Capture the pre-install state after {app} is initialized but before installation starts.
function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  ExistingValue: String;
begin
  NeedsRestart := False;
  ExistingValue := '';
  AutostartValueExists := RegQueryStringValue(HKLM64, AutostartSubkey, AutostartValueName, ExistingValue);
  ExistingInstallation := FileExists(ExpandConstant('{app}\steady-ink.exe'));
  Result := '';
end;

// Create the default startup value only for a fresh installation.
function ShouldCreateAutostart(): Boolean;
begin
  Result := (not AutostartValueExists) and (not ExistingInstallation);
end;

// Remove only Steady Ink's machine-wide value and shortcuts during uninstall.
procedure CurUninstallStepChanged(AStep: TUninstallStep);
begin
  if AStep = usUninstall then begin
    RegDeleteValue(HKLM64, AutostartSubkey, AutostartValueName);
    DeleteFile(ExpandConstant('{commondesktop}\Steady Ink.lnk'));
    DeleteFile(ExpandConstant('{commonprograms}\Steady Ink\Steady Ink.lnk'));
    RemoveDir(ExpandConstant('{commonprograms}\Steady Ink'));
  end;
end;

// Remove shortcuts that were deselected during an upgrade.
procedure CurStepChanged(CurStep: TSetupStep);
begin
  if CurStep = ssInstall then begin
    if not WizardIsTaskSelected('desktopicon') then
      DeleteFile(ExpandConstant('{commondesktop}\Steady Ink.lnk'));
    if not WizardIsTaskSelected('startmenuicon') then begin
      DeleteFile(ExpandConstant('{commonprograms}\Steady Ink\Steady Ink.lnk'));
      RemoveDir(ExpandConstant('{commonprograms}\Steady Ink'));
    end;
  end;
end;
