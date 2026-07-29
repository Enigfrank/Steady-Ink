const
  DriveFixed = 3;
  PreferredDriveLetters = 'DEFGHIJKLMNOPQRSTUVWXYZABC';

function GetLogicalDrives(): LongWord;
  external 'GetLogicalDrives@kernel32.dll stdcall';
function GetDriveType(RootPathName: String): LongWord;
  external 'GetDriveTypeW@kernel32.dll stdcall';

// Convert an uppercase ASCII drive letter to the matching Win32 drive mask bit.
function DriveMaskForLetter(DriveLetter: Char): LongWord;
var
  DriveIndex: Integer;
begin
  DriveIndex := Ord(DriveLetter) - Ord('A');
  if (DriveIndex < 0) or (DriveIndex > 25) then
    Result := 0
  else
    Result := 1 shl DriveIndex;
end;

// Select D first, then the remaining fixed non-system drives in stable letter order.
function SelectPreferredInstallDrive(
  LogicalDriveMask: LongWord;
  FixedDriveMask: LongWord;
  SystemDriveMask: LongWord
): String;
var
  CandidateBit: LongWord;
  CandidateLetter: Char;
  Index: Integer;
begin
  Result := '';
  for Index := 1 to Length(PreferredDriveLetters) do begin
    CandidateLetter := PreferredDriveLetters[Index];
    CandidateBit := DriveMaskForLetter(CandidateLetter);
    if ((LogicalDriveMask and CandidateBit) <> 0) and
       ((FixedDriveMask and CandidateBit) <> 0) and
       ((SystemDriveMask and CandidateBit) = 0) then begin
      Result := CandidateLetter + ':\';
      Exit;
    end;
  end;
end;

// Query fixed local drives once while preserving the logical-drive availability mask.
function GetFixedDriveMask(LogicalDriveMask: LongWord): LongWord;
var
  CandidateBit: LongWord;
  CandidateRoot: String;
  DriveIndex: Integer;
begin
  Result := 0;
  for DriveIndex := 0 to 25 do begin
    CandidateBit := 1 shl DriveIndex;
    if (LogicalDriveMask and CandidateBit) <> 0 then begin
      CandidateRoot := Chr(Ord('A') + DriveIndex) + ':\';
      if GetDriveType(CandidateRoot) = DriveFixed then
        Result := Result or CandidateBit;
    end;
  end;
end;

// Resolve the Windows system drive to the same mask representation used by drive discovery.
function GetSystemDriveMask(): LongWord;
var
  SystemDrive: String;
begin
  SystemDrive := Uppercase(ExtractFileDrive(ExpandConstant('{win}')));
  if Length(SystemDrive) = 0 then
    Result := 0
  else
    Result := DriveMaskForLetter(SystemDrive[1]);
end;

// Return a non-system fixed-drive default for fresh installs and Program Files otherwise.
function GetDefaultInstallDir(Param: String): String;
var
  FixedDriveMask: LongWord;
  LogicalDriveMask: LongWord;
  PreferredDrive: String;
begin
  LogicalDriveMask := GetLogicalDrives();
  FixedDriveMask := GetFixedDriveMask(LogicalDriveMask);
  PreferredDrive := SelectPreferredInstallDrive(
    LogicalDriveMask,
    FixedDriveMask,
    GetSystemDriveMask()
  );
  if PreferredDrive <> '' then
    Result := PreferredDrive + 'Steady Ink'
  else
    Result := ExpandConstant('{autopf}\Steady Ink');
end;
