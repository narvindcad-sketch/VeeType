#define AppName "VeeType"
#ifndef AppVersion
#define AppVersion "0.3.0"
#endif
#define AppPublisher "VeeType"

[Setup]
AppId={{9B5876BA-347D-4869-A6C7-67E5B176E7A8}
AppName={#AppName}
AppVersion={#AppVersion}
AppPublisher={#AppPublisher}
DefaultDirName={localappdata}\VeeType
DefaultGroupName={#AppName}
UninstallDisplayName={#AppName}
SetupIconFile=icon.ico
WizardImageFile=wizard.bmp
WizardSmallImageFile=wizard-small.bmp
Uninstallable=not IsPortable
CreateUninstallRegKey=not IsPortable
UninstallDisplayIcon={app}\icon.ico
#ifndef NoSign
SignTool=MsSign
#endif
OutputDir=Output
OutputBaseFilename=VeeType_Installer_v{#AppVersion}
Compression=lzma2
SolidCompression=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
SetupLogging=yes

[Tasks]
Name: "desktopicon"; Description: "Create a desktop shortcut"; GroupDescription: "Additional shortcuts:"; Flags: unchecked

[Files]
Source: "target\release\VeeType.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "icon.ico"; DestDir: "{app}"; Flags: ignoreversion
Source: "config.example.toml"; DestDir: "{app}"; DestName: "config.toml"; Flags: onlyifdoesntexist


[Dirs]
Name: "{app}\Models"

[Icons]
Name: "{group}\{#AppName}"; Filename: "{app}\VeeType.exe"; IconFilename: "{app}\icon.ico"; Check: not IsPortable
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\VeeType.exe"; Tasks: desktopicon; IconFilename: "{app}\icon.ico"; Check: not IsPortable

[UninstallDelete]
; Removes downloaded models, vault, config and logs that the installer did not create.
Type: filesandordirs; Name: "{app}"

[Run]
Filename: "{app}\VeeType.exe"; Description: "Launch {#AppName} now"; Flags: nowait postinstall skipifsilent

[Code]
var
  InstallTypePage: TInputOptionWizardPage;

procedure InitializeWizard;
begin
  InstallTypePage := CreateInputOptionPage(wpWelcome,
    'Installation type', 'How would you like to install {#AppName}?',
    'Choose Normal to add Start Menu shortcuts and an uninstaller. Choose Portable for a self-contained folder that makes no registry changes.',
    True, False);
  InstallTypePage.Add('Normal installation (recommended)');
  InstallTypePage.Add('Portable installation');
  InstallTypePage.SelectedValueIndex := 0;
end;

function IsPortable: Boolean;
begin
  Result := Assigned(InstallTypePage) and (InstallTypePage.SelectedValueIndex = 1);
end;
