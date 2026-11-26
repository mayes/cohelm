; Cohelm installer for Windows (Inno Setup 6). Built by package-windows.ps1,
; which passes the staged files and the version:
;   iscc /DAppVersion=0.1.0 /DStage=<folder> /O<out> cohelm.iss
; Installs for the current user only, with no admin prompt.

#ifndef AppVersion
  #error AppVersion is required
#endif
#ifndef Stage
  #error Stage is required
#endif

[Setup]
; Keep this AppId for every release, so updates replace the old install.
AppId={{836F93DC-4D25-40D2-8780-2231F8543FCD}
AppName=Cohelm
AppVersion={#AppVersion}
AppVerName=Cohelm {#AppVersion}
AppPublisher=Cohelm contributors
AppPublisherURL=https://github.com/mayes/cohelm
DefaultDirName={localappdata}\Programs\Cohelm
DisableDirPage=yes
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputBaseFilename=Cohelm-{#AppVersion}-windows-x64-setup
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
LicenseFile={#Stage}\LICENSE
UninstallDisplayName=Cohelm
UninstallDisplayIcon={app}\browser\chrome.exe
CloseApplications=yes

[Tasks]
Name: desktopicon; Description: "Create a desktop shortcut"; Flags: unchecked

[Files]
Source: "{#Stage}\*"; DestDir: "{app}"; Flags: recursesubdirs createallsubdirs ignoreversion

[Icons]
Name: "{userprograms}\Cohelm"; Filename: "{app}\Cohelm.exe"; IconFilename: "{app}\browser\chrome.exe"
Name: "{userdesktop}\Cohelm"; Filename: "{app}\Cohelm.exe"; IconFilename: "{app}\browser\chrome.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\Cohelm.exe"; Description: "Start Cohelm"; Flags: nowait postinstall skipifsilent

; The profile in %LOCALAPPDATA%\Cohelm\User Data holds the person's logins
; and is left in place on uninstall.
