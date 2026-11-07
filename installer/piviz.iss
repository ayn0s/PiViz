; Inno Setup script for the PiViz Windows installer.
; Built by .github/workflows/release.yml; locally (Inno Setup 6), after `cargo build --release`:
;   iscc /DAppVersion=0.1.0 installer\piviz.iss
; The installer lands in dist\PiViz-Setup-v<version>.exe.

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#ifndef SourceExe
  #define SourceExe "..\target\release\piviz.exe"
#endif

[Setup]
; Never change AppId: Windows uses it to recognise upgrades and the uninstaller.
AppId={{1C790FB9-4BD8-4D9D-988D-C247C0EFC881}
AppName=PiViz
AppVersion={#AppVersion}
AppVerName=PiViz {#AppVersion}
AppPublisher=PiViz
; Per-user install by default: no administrator rights needed.
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
DefaultDirName={autopf}\PiViz
DisableProgramGroupPage=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
SetupIconFile=..\assets\icon.ico
UninstallDisplayIcon={app}\piviz.exe
UninstallDisplayName=PiViz
OutputDir=..\dist
OutputBaseFilename=PiViz-Setup-v{#AppVersion}
Compression=lzma2
SolidCompression=yes
WizardStyle=modern

[Languages]
Name: "french"; MessagesFile: "compiler:Languages\French.isl"
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#SourceExe}"; DestDir: "{app}"; DestName: "piviz.exe"; Flags: ignoreversion
Source: "..\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\LICENSE-MIT"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\LICENSE-APACHE"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\PiViz"; Filename: "{app}\piviz.exe"
Name: "{autodesktop}\PiViz"; Filename: "{app}\piviz.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\piviz.exe"; Description: "{cm:LaunchProgram,PiViz}"; Flags: nowait postinstall skipifsilent
