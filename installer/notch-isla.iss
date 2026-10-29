; Instalador de Notch Isla (Inno Setup 6). Se genera con installer\build.ps1, que compila la
; app y le pasa la versión de Cargo.toml (/DAppVersion=...).
;
; Instala solo para el usuario actual (sin permisos de administrador) en
; %LOCALAPPDATA%\Programs\Notch Isla, crea el acceso del menú Inicio y queda en
; "Aplicaciones instaladas" con su desinstalador.

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif

#define AppName "Notch Isla"
#define AppExe "notch-isla.exe"
#define AppUrl "https://github.com/UtilityForCoders/notch-isla"
; Mismo nombre de valor que usa la app en "Iniciar con Windows" (src/autostart.rs), así el
; menú de la bandeja muestra la opción marcada y el desinstalador puede quitarla.
#define RunKey "Software\Microsoft\Windows\CurrentVersion\Run"
#define RunValue "NotchIsla"

[Setup]
; Identificador fijo: no cambiarlo, o Windows tratará cada versión como otra app.
AppId={{977B762C-4545-4D26-8CF9-CC072B365CC0}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
AppPublisher=Gerson Portal
AppPublisherURL={#AppUrl}
AppSupportURL={#AppUrl}/issues
AppUpdatesURL={#AppUrl}/releases
DefaultDirName={autopf}\{#AppName}
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
; Windows 10 1809: lo mínimo para SMTC.
MinVersion=10.0.17763
SetupIconFile=..\assets\icon.ico
UninstallDisplayIcon={app}\{#AppExe}
UninstallDisplayName={#AppName}
WizardStyle=modern
Compression=lzma2/ultra64
SolidCompression=yes
OutputDir=..\target\installer
OutputBaseFilename=notch-isla-setup-{#AppVersion}
; Si la isla está abierta al actualizar o desinstalar, se cierra sola.
CloseApplications=force
RestartApplications=no

[Languages]
Name: "spanish"; MessagesFile: "compiler:Languages\Spanish.isl"
Name: "english"; MessagesFile: "compiler:Default.isl"

[CustomMessages]
spanish.TaskAutostart=Iniciar con Windows
spanish.TaskDesktop=Crear un acceso directo en el escritorio
spanish.LaunchApp=Abrir {#AppName}
english.TaskAutostart=Start with Windows
english.TaskDesktop=Create a desktop shortcut
english.LaunchApp=Open {#AppName}

[Tasks]
Name: "autostart"; Description: "{cm:TaskAutostart}"
Name: "desktopicon"; Description: "{cm:TaskDesktop}"; Flags: unchecked

[Files]
Source: "..\target\release\{#AppExe}"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\{#AppName}"; Filename: "{app}\{#AppExe}"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#AppExe}"; Tasks: desktopicon

[Registry]
Root: HKCU; Subkey: "{#RunKey}"; ValueType: string; ValueName: "{#RunValue}"; ValueData: """{app}\{#AppExe}"""; Tasks: autostart

[Run]
Filename: "{app}\{#AppExe}"; Description: "{cm:LaunchApp}"; Flags: nowait postinstall skipifsilent
; Actualización desde la app (instalación silenciosa): la isla se cerró para dejar instalar y
; se vuelve a abrir sola.
Filename: "{app}\{#AppExe}"; Flags: nowait; Check: WizardSilent

[UninstallRun]
; La isla no tiene ventana que cerrar: se termina el proceso antes de borrar el .exe.
Filename: "{sys}\taskkill.exe"; Parameters: "/f /im {#AppExe}"; Flags: runhidden; RunOnceId: "CerrarIsla"

[Code]
// Al desinstalar se quita "Iniciar con Windows" aunque se haya activado desde la bandeja.
procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usPostUninstall then
    RegDeleteValue(HKEY_CURRENT_USER, '{#RunKey}', '{#RunValue}');
end;
