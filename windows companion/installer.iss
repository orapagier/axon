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

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Files]
; The main executable
Source: "target\release\windowsapi.exe"; DestDir: "{app}"; Flags: ignoreversion
; Bundle config.toml if it exists in the project root, otherwise bundle
; config.example.toml but rename it to config.toml.
;
; config.example.toml contains PLACEHOLDERS ONLY, and must stay that way — it is
; committed to git and it is what every fresh install starts from. The service
; refuses to start until the placeholders are replaced.
;
; onlyifdoesntexist protects a config the user has already filled in from being
; overwritten when they install a newer build over the top.
#if FileExists("config.toml")
  Source: "config.toml"; DestDir: "{app}"; Flags: ignoreversion
#else
  Source: "config.example.toml"; DestDir: "{app}"; DestName: "config.toml"; Flags: onlyifdoesntexist
#endif
; Laptop power-preparation helper — see README.
Source: "scripts\prepare-laptop.ps1"; DestDir: "{app}\scripts"; Flags: ignoreversion

[Icons]
Name: "{group}\{cm:UninstallProgram,Windows Automation API}"; Filename: "{uninstallexe}"

[Run]
; Stop any previous service so this build's binary can replace the running one.
; A first-time install has nothing to stop, hence no error handling.
Filename: "{app}\windowsapi.exe"; Parameters: "--stop"; Flags: runhidden waituntilterminated; StatusMsg: "Stopping existing service..."

; A fresh install ships placeholder credentials and the service will refuse to
; start until they are filled in, so offer the config for editing first.
#if !FileExists("config.toml")
Filename: "notepad.exe"; Parameters: """{app}\config.toml"""; Description: "Edit config.toml (required before the service can start)"; Flags: postinstall skipifsilent nowait
#endif

; Registers the LocalSystem service, locks the install directory down to SYSTEM
; and Administrators, and starts it. Safe to run while config.toml is still a
; placeholder: the service reports ERROR_BAD_CONFIGURATION and stops cleanly,
; and `windowsapi --start` picks it up once the credentials are filled in.
Filename: "{app}\windowsapi.exe"; Parameters: "--install"; Flags: runhidden waituntilterminated; StatusMsg: "Registering the Windows service..."

[UninstallRun]
; Deregister before the files go, or the SCM keeps an entry pointing at a
; deleted binary.
Filename: "{app}\windowsapi.exe"; Parameters: "--uninstall"; Flags: runhidden waituntilterminated; RunOnceId: "RemoveService"

[UninstallDelete]
; Created at runtime, so Inno does not track them.
Type: files; Name: "{app}\cloudflared.exe"
Type: files; Name: "{app}\windowsapi_error.log"
Type: filesandordirs; Name: "{app}\public"
