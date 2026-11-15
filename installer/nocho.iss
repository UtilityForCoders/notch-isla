; Instalador de Nocho (Inno Setup 6). Se genera con installer\build.ps1, que compila la
; app y le pasa la versión de Cargo.toml (/DAppVersion=...).
;
; Instala solo para el usuario actual (sin permisos de administrador) en
; %LOCALAPPDATA%\Programs\Nocho, crea el acceso del menú Inicio y queda en
; "Aplicaciones instaladas" con su desinstalador.
;
; Hasta la 0.8 la app se llamaba Notch Isla. Este instalador actualiza encima de esa versión:
; usa el mismo AppId, cierra la Notch Isla abierta y borra su carpeta, sus accesos directos y su
; valor de "Iniciar con Windows". Los ajustes y datos los migra la app al abrirse (src/migration.rs).

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif

#define AppName "Nocho"
#define AppExe "nocho.exe"
#define AppUrl "https://github.com/UtilityForCoders/Nocho"
; Mismo nombre de valor que usa la app en "Iniciar con Windows" (src/autostart.rs), así el
; menú de la bandeja muestra la opción marcada y el desinstalador puede quitarla.
#define RunKey "Software\Microsoft\Windows\CurrentVersion\Run"
#define RunValue "Nocho"
; Lo que dejaba Notch Isla (0.8 o anterior).
#define LegacyName "Notch Isla"
#define LegacyExe "notch-isla.exe"
#define LegacyRunValue "NotchIsla"

[Setup]
; Identificador fijo: no cambiarlo, o Windows tratará cada versión como otra app. Es el mismo
; de Notch Isla, así la actualización reemplaza su entrada en "Aplicaciones instaladas".
AppId={{977B762C-4545-4D26-8CF9-CC072B365CC0}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
AppPublisher=Gerson Portal
AppPublisherURL={#AppUrl}
AppSupportURL={#AppUrl}/issues
AppUpdatesURL={#AppUrl}/releases
DefaultDirName={autopf}\{#AppName}
; Con el mismo AppId, Inno Setup reutilizaría la carpeta "Notch Isla" de la versión anterior.
UsePreviousAppDir=no
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
; Windows 10 1809: lo mínimo para SMTC.
MinVersion=10.0.17763
SetupIconFile=..\assets\nocho.ico
UninstallDisplayIcon={app}\{#AppExe}
UninstallDisplayName={#AppName}
WizardStyle=modern
Compression=lzma2/ultra64
SolidCompression=yes
OutputDir=..\target\installer
OutputBaseFilename=nocho-setup-{#AppVersion}
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

[InstallDelete]
; Notch Isla: su carpeta (con su desinstalador) y sus accesos directos.
Type: filesandordirs; Name: "{autopf}\{#LegacyName}"
Type: files; Name: "{autoprograms}\{#LegacyName}.lnk"
Type: files; Name: "{autodesktop}\{#LegacyName}.lnk"

[Files]
Source: "..\target\release\{#AppExe}"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\{#AppName}"; Filename: "{app}\{#AppExe}"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#AppExe}"; Tasks: desktopicon

[Registry]
; El "Iniciar con Windows" de Notch Isla apuntaba a un .exe que ya no existe.
Root: HKCU; Subkey: "{#RunKey}"; ValueName: "{#LegacyRunValue}"; Flags: deletevalue
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
// Notch Isla abierta desde su carpeta vieja: CloseApplications solo cierra lo que está en la
// carpeta nueva, así que se termina aparte antes de borrar su carpeta.
function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  ResultCode: Integer;
begin
  Exec(ExpandConstant('{sys}\taskkill.exe'), '/f /im {#LegacyExe}', '', SW_HIDE,
    ewWaitUntilTerminated, ResultCode);
  Result := '';
end;

// Al desinstalar se quita "Iniciar con Windows" aunque se haya activado desde la bandeja.
procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usPostUninstall then
  begin
    RegDeleteValue(HKEY_CURRENT_USER, '{#RunKey}', '{#RunValue}');
    RegDeleteValue(HKEY_CURRENT_USER, '{#RunKey}', '{#LegacyRunValue}');
  end;
end;
