#ifndef OutputDir
#define OutputDir "."
#endif
#ifndef TestAppId
#error TestAppId must be defined by scripts/release/test-installer-default-dir.ps1
#endif
#ifndef FallbackDir
#error FallbackDir must be defined by scripts/release/test-installer-default-dir.ps1
#endif
#ifndef ResultFile
#error ResultFile must be defined by scripts/release/test-installer-default-dir.ps1
#endif

[Setup]
AppId={#TestAppId}
AppName=Steady Ink Default Directory Test
AppVersion=1.0.0
DefaultDirName={#FallbackDir}
DisableDirPage=no
UsePreviousAppDir=yes
PrivilegesRequired=lowest
OutputDir={#OutputDir}
OutputBaseFilename=Steady-Ink-Default-Dir-Test
Compression=none
SolidCompression=no
WizardStyle=modern

[Code]
#include "default-dir.iss"

// Build a Win32 drive mask from a compact uppercase drive-letter fixture.
function MaskOf(DriveLetters: String): LongWord;
var
  Index: Integer;
begin
  Result := 0;
  for Index := 1 to Length(DriveLetters) do
    Result := Result or DriveMaskForLetter(DriveLetters[Index]);
end;

// Assert one pure drive-selection scenario before the isolated test installer starts.
procedure AssertPreferredDrive(
  Scenario: String;
  LogicalDrives: String;
  FixedDrives: String;
  SystemDrive: Char;
  Expected: String
);
var
  Actual: String;
begin
  Actual := SelectPreferredInstallDrive(
    MaskOf(LogicalDrives),
    MaskOf(FixedDrives),
    DriveMaskForLetter(SystemDrive)
  );
  if Actual <> Expected then
    RaiseException(Format('%s: expected "%s", got "%s"', [Scenario, Expected, Actual]));
end;

// Exercise every required priority, exclusion, and fallback branch without production payload files.
function InitializeSetup(): Boolean;
var
  ActualDefault: String;
begin
  AssertPreferredDrive('D is preferred', 'CDE', 'CDE', 'C', 'D:\');
  AssertPreferredDrive('D is removable', 'CDE', 'CE', 'C', 'E:\');
  AssertPreferredDrive('D is unavailable', 'CE', 'CE', 'C', 'E:\');
  AssertPreferredDrive('only system drive', 'C', 'C', 'C', '');
  AssertPreferredDrive('D is system drive', 'CDE', 'CDE', 'D', 'E:\');
  AssertPreferredDrive('fixed but unavailable is ignored', 'CE', 'CDE', 'C', 'E:\');

  ActualDefault := GetDefaultInstallDir('');
  if CompareText(ExtractFileName(ActualDefault), 'Steady Ink') <> 0 then
    RaiseException(Format('runtime default has an unexpected name: "%s"', [ActualDefault]));
  Result := True;
end;

// Record the effective application directory after each isolated test installation.
procedure CurStepChanged(CurStep: TSetupStep);
begin
  if CurStep = ssPostInstall then
    SaveStringToFile('{#ResultFile}', ExpandConstant('{app}'), False);
end;
