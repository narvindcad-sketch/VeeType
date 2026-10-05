#define AppName "VeeType"
#define AppVersion "0.1.0"
#define AppPublisher "VeeType"

[Setup]
AppId={{9B5876BA-347D-4869-A6C7-67E5B176E7A8}
AppName={#AppName}
AppVersion={#AppVersion}
AppPublisher={#AppPublisher}
DefaultDirName={localappdata}\Programs\VeeType
DefaultGroupName={#AppName}
UninstallDisplayName={#AppName}
OutputDir=Output
OutputBaseFilename=VeeType_Setup
Compression=lzma2
SolidCompression=yes
PrivilegesRequired=lowest
ArchitecturesInstallAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
SetupLogging=yes

[Tasks]
Name: "desktopicon"; Description: "Create a desktop shortcut"; GroupDescription: "Additional shortcuts:"; Flags: unchecked

[Files]
Source: "target\release\VeeType.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "config.example.toml"; DestDir: "{app}"; DestName: "config.toml"; Flags: onlyifdoesntexist
Source: "Models\*"; DestDir: "{app}\Models"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
Name: "{group}\{#AppName}"; Filename: "{app}\VeeType.exe"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\VeeType.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\VeeType.exe"; Description: "Launch {#AppName} now"; Flags: nowait postinstall skipifsilent
