; RepoM Windows Installer Script (Inno Setup 6)
;
; Builds a standalone Windows setup wizard (repom-<version>-windows-x86_64-setup.exe)
; containing the statically-linked repom.exe binary so it runs on any Windows 10/11 PC
; with zero external runtime packages (no VC++ Redistributable, WebView2, or .NET required).
;
; Build via packaging/windows/package.ps1 or directly:
;   iscc /DAppVersion=0.1.0 /DBinDir=target\x86_64-pc-windows-msvc\release /DRepoRoot=. /DOutputDir=dist\release packaging\windows\repom.iss

#ifndef AppVersion
  #define AppVersion "0.1.0"
#endif
#ifndef RepoRoot
  #define RepoRoot "..\.."
#endif
#ifndef BinDir
  #define BinDir "..\..\target\x86_64-pc-windows-msvc\release"
#endif
#ifndef OutputDir
  #define OutputDir "..\..\dist\release"
#endif

#define AppName "RepoM"
#define AppPublisher "Menuka Deshan"
#define AppURL "https://github.com/MenukaDeshan/RepoM"
#define AppExeName "repom.exe"

[Setup]
; Unique stable GUID for RepoM upgrades — never change or reuse for another app.
AppId={{7D3E9A42-5C18-4F6B-9E2A-8B41D6C0E3A9}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
AppPublisher={#AppPublisher}
AppPublisherURL={#AppURL}
AppSupportURL={#AppURL}/issues
AppUpdatesURL={#AppURL}/releases
DefaultDirName={autopf}\{#AppName}
DefaultGroupName={#AppName}
AllowNoIcons=yes
LicenseFile={#RepoRoot}\LICENSE
SetupIconFile={#RepoRoot}\assets\repom.ico
UninstallDisplayName={#AppName}
UninstallDisplayIcon={app}\repom.ico
OutputDir={#OutputDir}
OutputBaseFilename=repom-{#AppVersion}-windows-x86_64-setup
Compression=lzma2/ultra64
SolidCompression=yes
WizardStyle=modern
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
; Per-user install by default (no admin required), with a dialog option to install for all users.
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
CloseApplications=yes
RestartApplications=no

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"

[Files]
Source: "{#BinDir}\{#AppExeName}"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#RepoRoot}\assets\repom.ico"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#RepoRoot}\LICENSE"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#RepoRoot}\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#RepoRoot}\repom.example.toml"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\{#AppName}"; Filename: "{app}\{#AppExeName}"; IconFilename: "{app}\repom.ico"; Comment: "Scan, monitor, and manage local Git repositories against GitHub"
Name: "{group}\{cm:UninstallProgram,{#AppName}}"; Filename: "{uninstallexe}"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#AppExeName}"; IconFilename: "{app}\repom.ico"; Tasks: desktopicon; Comment: "Scan, monitor, and manage local Git repositories against GitHub"

[Registry]
; Allow launching RepoM via Win+R -> "repom"
Root: HKA; Subkey: "Software\Microsoft\Windows\CurrentVersion\App Paths\{#AppExeName}"; ValueType: string; ValueName: ""; ValueData: "{app}\{#AppExeName}"; Flags: uninsdeletekey
Root: HKA; Subkey: "Software\Microsoft\Windows\CurrentVersion\App Paths\{#AppExeName}"; ValueType: string; ValueName: "Path"; ValueData: "{app}"

[Run]
Filename: "{app}\{#AppExeName}"; Description: "{cm:LaunchProgram,{#StringChange(AppName, '&', '&&')}}"; Flags: nowait postinstall skipifsilent
