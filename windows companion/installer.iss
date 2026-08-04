; Inno Setup script for the windowsapi service.

[Setup]
; NOTE: The value of AppId uniquely identifies this application. Do not use the same AppId value in installers for other applications.
; (To generate a new GUID, click Tools | Generate GUID inside the IDE.)
AppId={{D3B3E5B1-4C3D-4A5D-B6E7-F8D9C0A1B2C3}
AppName=Windows Automation API
AppVersion=1.0.0
AppPublisher=Samuzziel
; Must match service::install_dir() in src/service.rs (%ProgramData%\WindowsAPI).
; If these differ, `--install` copies the binary to ProgramData and registers
; THAT copy as the service, leaving two installs on disk and an Inno uninstaller
; that removes the wrong one.
DefaultDirName={commonappdata}\WindowsAPI
DefaultGroupName=Windows Automation API
AllowNoIcons=yes
; Where the installer EXE will be saved
OutputDir=.\Output
OutputBaseFilename=windowsapi-setup
Compression=lzma
SolidCompression=yes
WizardStyle=modern
; Registering a LocalSystem service requires elevation. That service account is
; what keeps the API reachable while the machine is locked, so this is not
; optional — the old PrivilegesRequired=lowest install cannot work any more.
PrivilegesRequired=admin

; Double-click and it sets itself up. There is nothing to choose: the install
; directory is fixed (it has to match service::install_dir()), there are no
; optional components, and the config comes from the folder the user put it in.
DisableWelcomePage=yes
DisableDirPage=yes
DisableProgramGroupPage=yes
DisableReadyPage=yes
DisableFinishedPage=no
; The uninstaller has to survive the install directory being ACL'd to SYSTEM and
; Administrators, so keep its data with the app.
UninstallFilesDir={app}

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Files]
; The main executable
Source: "target\release\windowsapi.exe"; DestDir: "{app}"; Flags: ignoreversion

; ── config.toml ───────────────────────────────────────────────────────────────
; Read from {src} — the folder the user ran setup.exe from — at INSTALL time,
; not compile time. Put your filled-in config.toml next to windowsapi-setup.exe
; and the two travel together into the install directory.
;
; `external` is what makes this an install-time read; without it Inno would try
; to embed the file when the installer is compiled. `skipifsourcedoesntexist`
; keeps setup working when there is no config beside it.
;
; This entry deliberately has no `onlyifdoesntexist`: a config the user placed
; next to setup.exe is an explicit instruction and overwrites an existing one.
Source: "{src}\config.toml"; DestDir: "{app}"; Flags: external skipifsourcedoesntexist ignoreversion

; Fallback when no config was supplied. config.example.toml contains
; PLACEHOLDERS ONLY and must stay that way — it is committed to git. The service
; refuses to start until they are replaced, which the finished page explains.
; `onlyifdoesntexist` stops this clobbering a working install on upgrade, and
; also stops it overwriting the file the entry above just placed.
Source: "config.example.toml"; DestDir: "{app}"; DestName: "config.toml"; Flags: onlyifdoesntexist

; Laptop power-preparation helper — see README.
Source: "scripts\prepare-laptop.ps1"; DestDir: "{app}\scripts"; Flags: ignoreversion

[Icons]
Name: "{group}\{cm:UninstallProgram,Windows Automation API}"; Filename: "{uninstallexe}"

[Run]
; Stop any previous service so this build's binary can replace the running one.
; A first-time install has nothing to stop, hence no error handling.
; NOTE: stopping the old service happens in PrepareToInstall, NOT here. [Run]
; executes after [Files], by which point Inno has already tried to overwrite a
; windowsapi.exe that the running service still holds locked.
;
; --quiet is mandatory on every call setup makes. These run runhidden with
; waituntilterminated, so the exe has no console to print to; without --quiet it
; falls back to a modal message box that is invisible and never dismissed, and
; setup hangs on it forever. Results go to windowsapi_error.log instead.

; Registers the LocalSystem service, locks the install directory down to SYSTEM
; and Administrators, and starts it. Safe to run while config.toml is still a
; placeholder: the service reports ERROR_BAD_CONFIGURATION and stops cleanly.
Filename: "{app}\windowsapi.exe"; Parameters: "--install --quiet"; Flags: runhidden waituntilterminated; StatusMsg: "Registering the Windows service..."

; ── Only when the credentials are still placeholders ──────────────────────────
; Checked at install time, not compile time, so it reacts to the config.toml the
; user actually supplied. Deliberately NOT `nowait`: setup waits for Notepad to
; close so the service start below sees the saved file.
Filename: "notepad.exe"; Parameters: """{app}\config.toml"""; Description: "Edit config.toml now (required — the service cannot start without it)"; Flags: postinstall skipifsilent waituntilterminated; Check: ConfigIsPlaceholder

Filename: "{app}\windowsapi.exe"; Parameters: "--start --quiet"; Description: "Start the service once the config is saved"; Flags: postinstall skipifsilent runhidden waituntilterminated; Check: ConfigIsPlaceholder

; Offer the laptop power prep regardless — a sleeping laptop is unreachable no
; matter how the service is configured.
Filename: "powershell.exe"; Parameters: "-ExecutionPolicy Bypass -NoProfile -File ""{app}\scripts\prepare-laptop.ps1"""; Description: "Stop this machine sleeping (recommended for laptops)"; Flags: postinstall skipifsilent unchecked waituntilterminated

[UninstallRun]
; Deregister before the files go, or the SCM keeps an entry pointing at a
; deleted binary.
; --quiet for the same reason as the install-time calls: runhidden leaves no
; console, and the message-box fallback would block uninstall forever.
Filename: "{app}\windowsapi.exe"; Parameters: "--uninstall --quiet"; Flags: runhidden waituntilterminated; RunOnceId: "RemoveService"

[UninstallDelete]
; Created at runtime, so Inno does not track them.
Type: files; Name: "{app}\cloudflared.exe"
Type: files; Name: "{app}\windowsapi_error.log"
Type: filesandordirs; Name: "{app}\public"

[Code]

{ True while config.toml still carries the shipped placeholders. Drives both the
  post-install checkboxes and the wording on the finished page, so the installer
  never claims success for a service that cannot start. }
function ConfigIsPlaceholder(): Boolean;
var
  Content: AnsiString;
begin
  { No config at all counts as "not ready". }
  Result := True;
  if LoadStringFromFile(ExpandConstant('{app}\config.toml'), Content) then
    Result := (Pos('YOUR_CLOUDFLARE_TUNNEL_TOKEN_HERE', Content) > 0) or
              (Pos('change-this-to-a-strong-random-secret', Content) > 0);
end;

{ Warn before overwriting a config that is already filled in. Without this, a
  reinstall from a folder holding a stale config.toml would silently replace
  working credentials. }
function InitializeSetup(): Boolean;
var
  Installed: AnsiString;
  Supplied: String;
begin
  Result := True;
  Supplied := ExpandConstant('{src}\config.toml');

  if FileExists(Supplied) and
     LoadStringFromFile(ExpandConstant('{commonappdata}\WindowsAPI\config.toml'), Installed) then
  begin
    if (Pos('YOUR_CLOUDFLARE_TUNNEL_TOKEN_HERE', Installed) = 0) and
       (Pos('change-this-to-a-strong-random-secret', Installed) = 0) then
    begin
      Result := MsgBox(
        'This machine already has a configured install.' + #13#10#13#10 +
        'The config.toml next to this installer will REPLACE the existing one at' + #13#10 +
        ExpandConstant('{commonappdata}\WindowsAPI\config.toml') + #13#10#13#10 +
        'Continue?',
        mbConfirmation, MB_YESNO) = IDYES;
    end;
  end;
end;

{ Stops any running install BEFORE Inno starts replacing files.

  This has to happen here rather than in [Run]: [Run] executes after [Files], by
  which point setup has already tried to overwrite a windowsapi.exe that the
  running service holds locked, and it stalls waiting for a file it can never
  write.

  sc.exe is used rather than `windowsapi.exe --stop` for the same reason —
  executing the target binary would lock the very file we are about to replace. }
procedure StopExistingInstall();
var
  Code: Integer;
begin
  Exec(ExpandConstant('{sys}\sc.exe'), 'stop WindowsAPI', '', SW_HIDE, ewWaitUntilTerminated, Code);
  { Give the service time to unwind. Its desktop agent dies with it via the job
    object, but a wedged agent can still hold the binary open. }
  Sleep(4000);
  Exec(ExpandConstant('{sys}\taskkill.exe'), '/IM windowsapi.exe /F', '', SW_HIDE, ewWaitUntilTerminated, Code);
  Sleep(1500);
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  StopExistingInstall();
  Result := '';
end;

procedure CurPageChanged(CurPageID: Integer);
begin
  if CurPageID = wpFinished then
  begin
    if ConfigIsPlaceholder() then
      WizardForm.FinishedLabel.Caption :=
        'Installed, but the service is NOT running yet.' + #13#10#13#10 +
        'config.toml still contains placeholder credentials. Tick the box below to ' +
        'fill in your tunnel_token and api_secret — the service starts as soon as ' +
        'you save and close Notepad.' + #13#10#13#10 +
        'Next time, put your filled-in config.toml in the same folder as this ' +
        'installer and it will be picked up automatically.'
    else
      WizardForm.FinishedLabel.Caption :=
        'Setup complete — the service is installed and running.' + #13#10#13#10 +
        'Your config.toml was installed to ' +
        ExpandConstant('{app}') + '.' + #13#10#13#10 +
        'The machine is now reachable while locked for /shell, /files/*, /system/* ' +
        'and /registry/*. Screenshot, clipboard and input routes need someone ' +
        'logged in — check both halves with GET /status.';
  end;
end;
