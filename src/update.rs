//! Actualizaciones: consulta el último release publicado en GitHub y, si es más nuevo, descarga
//! el instalador para instalarlo con un clic.
//!
//! Corre en un hilo aparte que pasa casi todo el tiempo dormido (0 % de CPU): revisa al minuto
//! de abrir la app y luego cada 6 horas. No envía ningún dato: son GET sin cookies ni
//! identificadores. El instalador se verifica (SHA-256 publicado en el mismo release) antes de
//! ofrecerlo. Si la app es la versión portable (no instalada), solo se avisa y se abre la página.
//!
//! No se usa la API de GitHub: sin autenticar admite 60 consultas por hora y por IP, y en redes
//! que comparten IP (operadoras, oficinas) se agota y la app no podía buscar ni descargar. La
//! versión sale de la redirección de `releases/latest` y la huella, del archivo `.sha256` que
//! publica el CI junto al instalador; ninguna de las dos tiene ese límite.

use std::path::{Path, PathBuf};
use std::time::Duration;

use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, ERROR_SUCCESS, HWND, LPARAM, WPARAM};
use windows::Win32::Networking::WinHttp::{
    WinHttpCloseHandle, WinHttpConnect, WinHttpOpen, WinHttpOpenRequest, WinHttpQueryHeaders,
    WinHttpReadData, WinHttpReceiveResponse, WinHttpSendRequest, WinHttpSetOption,
    WinHttpSetTimeouts, WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_FLAG_SECURE,
    WINHTTP_OPTION_REDIRECT_POLICY, WINHTTP_OPTION_REDIRECT_POLICY_NEVER,
    WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_QUERY_LOCATION, WINHTTP_QUERY_STATUS_CODE,
};
use windows::Win32::Security::Cryptography::{BCryptHash, BCRYPT_SHA256_ALG_HANDLE};
use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_SZ};
use windows::Win32::System::Threading::{OpenMutexW, SYNCHRONIZATION_SYNCHRONIZE};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, SW_SHOWNORMAL, WM_APP};

use crate::{autostart, settings};

/// Mensaje con la actualización encontrada (`Box<Update>` en lparam).
pub const WM_UPDATE: u32 = WM_APP + 4;

/// Página de descarga, para la versión portable o si la descarga falló.
pub const DOWNLOAD_URL: &str = "https://github.com/UtilityForCoders/Nocho/releases/latest";

const DOWNLOAD_HOST: &str = "github.com";
const REPO_PATH: &str = "/UtilityForCoders/Nocho";
/// `releases/latest` redirige aquí, seguido de la etiqueta del último release.
const TAG_URL: &str = "https://github.com/UtilityForCoders/Nocho/releases/tag/";

/// Entrada del desinstalador que crea Inno Setup (el AppId de installer/nocho.iss + "_is1").
const UNINSTALL_KEY: PCWSTR = w!(
    "Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\{977B762C-4545-4D26-8CF9-CC072B365CC0}_is1"
);

/// Espera antes de la primera consulta, para no competir con el arranque de Windows.
/// En debug son 5 s, para probar el aviso sin esperar.
const FIRST_CHECK: Duration = Duration::from_secs(if cfg!(debug_assertions) { 5 } else { 60 });
/// Cada cuánto se vuelve a consultar: la app suele pasar días abierta.
const CHECK_EVERY: Duration = Duration::from_secs(6 * 60 * 60);

/// Versión nueva disponible.
#[derive(Clone)]
pub struct Update {
    pub version: String,
    /// Instalador ya descargado y verificado; `None` si hay que ir a la página de descarga.
    pub installer: Option<PathBuf>,
    /// Huella SHA-256 que publicó GitHub para el instalador: se vuelve a comprobar justo antes
    /// de ejecutarlo, por si el archivo cambió desde la descarga.
    digest: String,
}

struct Handle(*mut core::ffi::c_void);

impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            let _ = WinHttpCloseHandle(self.0);
        }
    }
}

pub fn start(hwnd: HWND) {
    // HWND no es Send; se pasa como número.
    let hwnd = hwnd.0 as usize;
    std::thread::spawn(move || {
        std::thread::sleep(FIRST_CHECK);
        let current = current_version();
        let mut notified: Option<String> = None;
        loop {
            if let Some(update) = check(&current, notified.as_deref()) {
                notified = Some(update.version.clone());
                let ptr = Box::into_raw(Box::new(update));
                let sent = unsafe {
                    PostMessageW(
                        Some(HWND(hwnd as _)),
                        WM_UPDATE,
                        WPARAM(0),
                        LPARAM(ptr as isize),
                    )
                };
                if sent.is_err() {
                    // La ventana ya no existe: liberar y terminar.
                    drop(unsafe { Box::from_raw(ptr) });
                    return;
                }
            }
            std::thread::sleep(CHECK_EVERY);
        }
    });
}

/// Busca una versión más nueva que `current` (y distinta de la ya avisada) y, si la app está
/// instalada, descarga su instalador.
fn check(current: &str, notified: Option<&str>) -> Option<Update> {
    match lookup(current) {
        // Se avisa una sola vez por versión.
        Ok(Some(update)) if notified != Some(update.version.as_str()) => Some(update),
        _ => None,
    }
}

/// Consulta el último release: `Ok(Some)` si hay una versión más nueva que `current` (con su
/// instalador descargado si la app está instalada), `Ok(None)` si ya es la última, `Err` si
/// no se pudo consultar (sin internet, GitHub no responde...).
fn lookup(current: &str) -> std::result::Result<Option<Update>, ()> {
    let location =
        redirect_location(DOWNLOAD_HOST, &format!("{REPO_PATH}/releases/latest")).ok_or(())?;
    lookup_release(current, &location, is_installed(), download_installer)
}

/// `location` es adonde redirige `releases/latest`: `TAG_URL` seguido de la etiqueta.
fn lookup_release(
    current: &str,
    location: &str,
    installed: bool,
    download: impl FnOnce(&str, &str) -> Option<(PathBuf, String)>,
) -> std::result::Result<Option<Update>, ()> {
    let tag = location.strip_prefix(TAG_URL).ok_or(())?;
    let version = tag.trim_start_matches('v').to_string();
    // La versión se usa en el nombre del archivo y en la dirección: solo números y puntos.
    if !is_plain_version(&version) || tag != format!("v{version}") {
        return Err(());
    }
    if !is_newer(&version, current) {
        return Ok(None);
    }
    // La versión portable no descarga nada: solo avisa y abre la página.
    let downloaded = installed.then(|| download(tag, &version)).flatten();
    let (installer, digest) = match downloaded {
        Some((path, digest)) => (Some(path), digest),
        None => (None, String::new()),
    };
    Ok(Some(Update {
        version,
        installer,
        digest,
    }))
}

/// "1.2.3": de 1 a 4 números de hasta 5 cifras separados por puntos, nada más.
fn is_plain_version(v: &str) -> bool {
    let parts: Vec<&str> = v.split('.').collect();
    (1..=4).contains(&parts.len())
        && parts
            .iter()
            .all(|p| (1..=5).contains(&p.len()) && p.bytes().all(|b| b.is_ascii_digit()))
}

/// Mensaje con el resultado de "Buscar actualizaciones" cuando no hay versión nueva:
/// `wparam` = 0 si ya es la última, 1 si no se pudo consultar. Si la hay, llega `WM_UPDATE`
/// con `wparam` = 1 (pedida a mano).
pub const WM_UPDATE_CHECKED: u32 = WM_APP + 8;

/// Resultado de volver a consultar el último release al pulsar Instalar.
pub const WM_UPDATE_PREPARED: u32 = WM_APP + 10;
pub type PreparedUpdate = std::result::Result<Option<Update>, ()>;

pub fn prepare_install(hwnd: HWND) {
    let hwnd = hwnd.0 as usize;
    std::thread::spawn(move || {
        // No usar el Update del aviso: puede llevar horas guardado.
        let result = Box::into_raw(Box::new(lookup(&current_version())));
        if unsafe {
            PostMessageW(
                Some(HWND(hwnd as _)),
                WM_UPDATE_PREPARED,
                WPARAM(0),
                LPARAM(result as isize),
            )
        }
        .is_err()
        {
            drop(unsafe { Box::from_raw(result) });
        }
    });
}

/// Recupera el resultado de preparar la instalación.
///
/// # Safety
/// `lparam` debe proceder de `WM_UPDATE_PREPARED` y recuperarse una sola vez.
pub unsafe fn take_prepared(lparam: LPARAM) -> PreparedUpdate {
    *unsafe { Box::from_raw(lparam.0 as *mut PreparedUpdate) }
}

/// "Buscar actualizaciones" de la bandeja: consulta ya, en otro hilo, y avisa el resultado.
pub fn check_now(hwnd: HWND) {
    let hwnd = hwnd.0 as usize;
    std::thread::spawn(move || {
        let target = Some(HWND(hwnd as _));
        unsafe {
            match lookup(&current_version()) {
                Ok(Some(update)) => {
                    let ptr = Box::into_raw(Box::new(update));
                    if PostMessageW(target, WM_UPDATE, WPARAM(1), LPARAM(ptr as isize)).is_err() {
                        drop(Box::from_raw(ptr));
                    }
                }
                Ok(None) => {
                    let _ = PostMessageW(target, WM_UPDATE_CHECKED, WPARAM(0), LPARAM(0));
                }
                Err(()) => {
                    let _ = PostMessageW(target, WM_UPDATE_CHECKED, WPARAM(1), LPARAM(0));
                }
            }
        }
    });
}

/// Versión de esta app. En debug se puede fingir otra con la variable NOCHO_VERSION
/// (p. ej. "0.1.0") para ver el aviso.
fn current_version() -> String {
    if cfg!(debug_assertions) {
        if let Ok(v) = std::env::var("NOCHO_VERSION") {
            return v;
        }
    }
    env!("CARGO_PKG_VERSION").to_string()
}

/// Recupera la actualización que mandó `start`.
///
/// # Safety
/// `lparam` debe venir de un mensaje `WM_UPDATE`.
pub unsafe fn take(lparam: LPARAM) -> Update {
    *unsafe { Box::from_raw(lparam.0 as *mut Update) }
}

impl Update {
    /// Texto de la opción del menú de la bandeja.
    pub fn menu_label(&self) -> String {
        match self.installer {
            Some(_) => format!("Instalar la versión {}", self.version),
            None => format!("Descargar la versión {}", self.version),
        }
    }

    /// Instala si el instalador ya está descargado (devuelve `true`: la app debe cerrarse);
    /// si no, abre la página de descarga.
    pub fn apply(&self) -> bool {
        if let Some(installer) = &self.installer {
            if install(installer, &self.version, &self.digest) {
                return true;
            }
        }
        open_download_page();
        false
    }
}

/// Abre la página de descarga en el navegador.
fn open_download_page() {
    unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            &HSTRING::from(DOWNLOAD_URL),
            None,
            None,
            SW_SHOWNORMAL,
        );
    }
}

/// Opciones del instalador para instalar sin ventanas.
const SILENT: [&str; 4] = ["/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART", "/SP-"];

/// Argumento con el que la app arranca como ayudante de la actualización (ver `run_updater`).
pub const UPDATER_FLAG: &str = "--actualizar";

/// Las dos únicas opciones de tareas que se le pasan al instalador.
const TASKS: [&str; 2] = ["/MERGETASKS=autostart", "/MERGETASKS=!autostart"];

/// El archivo existe y su SHA-256 es `digest` (en hexadecimal, sin importar mayúsculas).
fn file_matches(path: &Path, digest: &str) -> bool {
    !digest.is_empty()
        && std::fs::read(path)
            .ok()
            .and_then(|data| sha256_hex(&data))
            .is_some_and(|hash| hash.eq_ignore_ascii_case(digest))
}

/// Instala `installer` (la versión `version`) sin ventanas. Devuelve `true` si arrancó:
/// entonces la app debe cerrarse para dejar instalar.
///
/// No se ejecuta el instalador directamente: se lanza una copia de esta app en la carpeta
/// temporal como ayudante (`run_updater`), que espera al instalador y se asegura de que la isla
/// vuelva a abrirse, aunque la instalación falle. Antes se anota qué versión se está
/// instalando: al abrirse de nuevo, la isla compara y avisa si se actualizó o no.
fn install(installer: &Path, version: &str, digest: &str) -> bool {
    // El instalador estuvo un rato en la carpeta temporal: si alguien lo cambió, no se ejecuta.
    if !file_matches(installer, digest) {
        let _ = std::fs::remove_file(installer);
        return false;
    }
    // Respeta "Iniciar con Windows" tal como está ahora, aunque se haya cambiado desde la bandeja.
    let tasks = if autostart::is_enabled() {
        TASKS[0]
    } else {
        TASKS[1]
    };
    settings::set_pending_update(Some(version));
    // La copia va fuera de la carpeta de la app: el instalador reemplaza ese .exe.
    let helper = std::env::temp_dir().join("nocho-actualizador.exe");
    let started = std::env::current_exe()
        .and_then(|exe| std::fs::copy(exe, &helper))
        .and_then(|_| {
            std::process::Command::new(&helper)
                .arg(UPDATER_FLAG)
                .arg(installer)
                .arg(tasks)
                .arg(digest)
                .spawn()
        })
        .is_ok()
        // Sin ayudante (no se pudo copiar), el instalador solo: igual vuelve a abrir la isla
        // si termina bien.
        || std::process::Command::new(installer)
            .args(SILENT)
            .arg(tasks)
            .spawn()
            .is_ok();
    if !started {
        settings::set_pending_update(None);
    }
    started
}

/// Modo ayudante: la isla ya se cerró. Corre el instalador, espera a que termine y, si la isla
/// no volvió a abrirse (la instalación falló, o el instalador no la abrió), la abre. Así nunca
/// queda cerrada sin avisar; al abrirse, la isla dice si se actualizó.
///
/// Para que nadie pueda usar este modo (en un .exe firmado) para lanzar otro programa, solo
/// ejecuta un instalador de Nocho en la carpeta temporal, con las opciones conocidas y
/// cuya huella coincide con la recibida (la que publicó GitHub, comprobada por la isla).
pub fn run_updater(args: &[String]) {
    let (Some(installer), Some(tasks), Some(digest)) = (args.first(), args.get(1), args.get(2))
    else {
        return;
    };
    let installer = Path::new(installer);
    let expected_place = installer.parent().and_then(|d| d.canonicalize().ok())
        == std::env::temp_dir().canonicalize().ok();
    let expected_name = installer
        .file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| n.strip_prefix("nocho-setup-"))
        .and_then(|n| n.strip_suffix(".exe"))
        .is_some_and(is_plain_version);
    if !expected_place
        || !expected_name
        || !TASKS.contains(&tasks.as_str())
        || !file_matches(installer, digest)
    {
        return;
    }
    let _ = std::process::Command::new(installer)
        .args(SILENT)
        .arg(tasks)
        .status();
    // Si salió bien, el instalador ya abrió la isla; se le dan unos segundos.
    for _ in 0..20 {
        if island_running() {
            return;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    if let Some(exe) = install_location().map(|dir| dir.join("nocho.exe")) {
        let _ = std::process::Command::new(exe).spawn();
    }
}

/// Hay una isla abierta (existe su candado de instancia única, ver `main`).
fn island_running() -> bool {
    unsafe {
        match OpenMutexW(
            SYNCHRONIZATION_SYNCHRONIZE,
            false,
            w!("Local\\Nocho.Instancia"),
        ) {
            Ok(handle) => {
                let _ = CloseHandle(handle);
                true
            }
            Err(_) => false,
        }
    }
}

/// Cómo salió la última actualización, al abrirse la isla: `(true, versión)` si ahora corre
/// la versión nueva, `(false, versión)` si la que `install` dejó anotada no quedó instalada,
/// `None` si no hubo actualización. Se avisa una sola vez.
///
/// Si no hay nada anotado pero esta versión es más nueva que la última que se abrió, también
/// cuenta como actualizada: pasa al instalar a mano o desde una isla anterior a este aviso.
pub fn take_install_result() -> Option<(bool, String)> {
    let current = current_version();
    let last = settings::swap_last_version(&current);
    if let Some(version) = settings::take_pending_update() {
        let ok = version == current;
        return Some((ok, if ok { current } else { version }));
    }
    match last {
        Some(last) => is_newer(&current, &last).then_some((true, current)),
        // Las islas de antes de este aviso no anotaban la versión: si el instalador de esta
        // versión se descargó hace unos minutos, es que la isla se acaba de actualizar sola.
        None => just_installed(&current).then_some((true, current)),
    }
}

/// El instalador de `version` está en la carpeta temporal y se escribió hace poco.
fn just_installed(version: &str) -> bool {
    let path = std::env::temp_dir().join(format!("nocho-setup-{version}.exe"));
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age < Duration::from_secs(15 * 60))
}

/// Carpeta donde instaló la app el instalador (según su entrada en "Aplicaciones instaladas").
fn install_location() -> Option<PathBuf> {
    let mut buf = [0u16; 1024];
    let mut size = std::mem::size_of_val(&buf) as u32;
    let found = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            UNINSTALL_KEY,
            w!("InstallLocation"),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut size),
        )
    } == ERROR_SUCCESS;
    if !found {
        return None;
    }
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Some(PathBuf::from(String::from_utf16_lossy(&buf[..len])))
}

/// La app corre desde la carpeta donde la dejó el instalador (y no como .exe portable).
fn is_installed() -> bool {
    let Some(location) = install_location() else {
        return false;
    };
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf));
    match (exe_dir, location.canonicalize()) {
        (Some(dir), Ok(location)) => dir.canonicalize().is_ok_and(|dir| dir == location),
        _ => false,
    }
}

/// Descarga el instalador de `version` a la carpeta temporal y lo verifica contra la huella
/// SHA-256 que el CI publica en el mismo release (`nocho-setup-x.y.z.exe.sha256`). Si ya estaba
/// descargado y es correcto, lo reutiliza. Devuelve dónde quedó y su huella.
fn download_installer(tag: &str, version: &str) -> Option<(PathBuf, String)> {
    let name = format!("nocho-setup-{version}.exe");
    // Siempre desde github.com y la ruta del repo; la redirección al almacenamiento de GitHub
    // la sigue WinHTTP.
    let release = format!("{REPO_PATH}/releases/download/{tag}");
    let checksum = get(DOWNLOAD_HOST, &format!("{release}/{name}.sha256"))?;
    let digest = parse_checksum(&String::from_utf8(checksum).ok()?, &name)?;

    let path = std::env::temp_dir().join(&name);
    if file_matches(&path, &digest) {
        return Some((path, digest));
    }
    let data = get(DOWNLOAD_HOST, &format!("{release}/{name}"))?;
    if sha256_hex(&data).as_deref() != Some(digest.as_str()) {
        return None;
    }
    std::fs::write(&path, &data).ok()?;
    Some((path, digest))
}

/// Huella de un archivo `.sha256` con el formato de `sha256sum`: `<64 hex>  <nombre>`. El
/// nombre, si viene, debe ser `name`. Devuelve la huella en minúsculas.
fn parse_checksum(text: &str, name: &str) -> Option<String> {
    let mut parts = text.split_whitespace();
    let digest = parts.next()?;
    let file = parts.next().map(|f| f.trim_start_matches('*'));
    let valid = digest.len() == 64 && digest.bytes().all(|b| b.is_ascii_hexdigit());
    (valid && file.is_none_or(|f| f == name) && parts.next().is_none())
        .then(|| digest.to_ascii_lowercase())
}

/// Huella SHA-256 en hexadecimal (minúsculas).
fn sha256_hex(data: &[u8]) -> Option<String> {
    let mut hash = [0u8; 32];
    let status = unsafe { BCryptHash(BCRYPT_SHA256_ALG_HANDLE, None, data, &mut hash) };
    status
        .is_ok()
        .then(|| hash.iter().map(|b| format!("{b:02x}")).collect())
}

/// Tamaño máximo de una descarga: el instalador pesa unos 2,5 MB y las respuestas JSON, pocos KB.
const MAX_DOWNLOAD: usize = 32 * 1024 * 1024;

/// Petición GET por HTTPS ya respondida, con su código de estado.
struct Response {
    request: Handle,
    status: u32,
    // Se cierran después de la petición (los campos se liberan en orden).
    _connect: Handle,
    _session: Handle,
}

/// Envía un GET por HTTPS. `follow` = seguir las redirecciones.
fn send(host: &str, path: &str, follow: bool) -> Option<Response> {
    unsafe {
        let agent = HSTRING::from(format!("nocho/{}", env!("CARGO_PKG_VERSION")));
        let session = Handle(WinHttpOpen(
            &agent,
            WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
            None,
            None,
            0,
        ));
        if session.0.is_null() {
            return None;
        }
        // Resolver, conectar, enviar y recibir: 30 s como máximo cada uno.
        let _ = WinHttpSetTimeouts(session.0, 30_000, 30_000, 30_000, 30_000);

        let connect = Handle(WinHttpConnect(session.0, &HSTRING::from(host), 443, 0));
        if connect.0.is_null() {
            return None;
        }
        let request = Handle(WinHttpOpenRequest(
            connect.0,
            w!("GET"),
            &HSTRING::from(path),
            None,
            None,
            std::ptr::null(),
            WINHTTP_FLAG_SECURE,
        ));
        if request.0.is_null() {
            return None;
        }
        if !follow {
            let never = WINHTTP_OPTION_REDIRECT_POLICY_NEVER.to_ne_bytes();
            WinHttpSetOption(
                Some(request.0),
                WINHTTP_OPTION_REDIRECT_POLICY,
                Some(&never),
            )
            .ok()?;
        }
        WinHttpSendRequest(request.0, None, None, 0, 0, 0).ok()?;
        WinHttpReceiveResponse(request.0, std::ptr::null_mut()).ok()?;

        let mut status = 0u32;
        let mut len = std::mem::size_of::<u32>() as u32;
        WinHttpQueryHeaders(
            request.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            None,
            Some(&mut status as *mut u32 as *mut _),
            &mut len,
            std::ptr::null_mut(),
        )
        .ok()?;
        Some(Response {
            request,
            status,
            _connect: connect,
            _session: session,
        })
    }
}

/// GET por HTTPS: el cuerpo de la respuesta si es 200. Lo usa también la letra.
pub(crate) fn get(host: &str, path: &str) -> Option<Vec<u8>> {
    let response = send(host, path, true)?;
    if response.status != 200 {
        return None;
    }
    let mut body = Vec::new();
    let mut chunk = vec![0u8; 64 * 1024];
    loop {
        let mut read = 0u32;
        unsafe {
            WinHttpReadData(
                response.request.0,
                chunk.as_mut_ptr().cast(),
                chunk.len() as u32,
                &mut read,
            )
        }
        .ok()?;
        if read == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..read as usize]);
        // Un servidor que manda de más (por error o a propósito) no llena la memoria.
        if body.len() > MAX_DOWNLOAD {
            return None;
        }
    }
    Some(body)
}

/// Adonde redirige un GET por HTTPS (la cabecera `Location`), sin seguir la redirección.
fn redirect_location(host: &str, path: &str) -> Option<String> {
    let response = send(host, path, false)?;
    if !matches!(response.status, 301 | 302 | 303 | 307 | 308) {
        return None;
    }
    let mut buf = [0u16; 512];
    let mut len = std::mem::size_of_val(&buf) as u32;
    unsafe {
        WinHttpQueryHeaders(
            response.request.0,
            WINHTTP_QUERY_LOCATION,
            None,
            Some(buf.as_mut_ptr().cast()),
            &mut len,
            std::ptr::null_mut(),
        )
    }
    .ok()?;
    // `len` sale en bytes, sin contar el cero final.
    let chars = (len as usize / 2).min(buf.len());
    String::from_utf16(&buf[..chars]).ok()
}

/// `true` si `candidate` (p. ej. "0.3.0") es mayor que `current`, comparando número a número.
fn is_newer(candidate: &str, current: &str) -> bool {
    let parse = |v: &str| -> Option<Vec<u32>> { v.split('.').map(|n| n.parse().ok()).collect() };
    match (parse(candidate), parse(current)) {
        (Some(a), Some(b)) => a > b,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tag(t: &str) -> String {
        format!("{TAG_URL}{t}")
    }

    #[test]
    fn vuelve_a_consultar_y_descarga_la_version_reciente_sin_pasos_intermedios() {
        let cached = lookup_release("0.4.0", &tag("v0.5.0"), true, |_, version| {
            Some((
                PathBuf::from(format!("nocho-setup-{version}.exe")),
                "anterior".into(),
            ))
        })
        .unwrap()
        .unwrap();
        assert_eq!(cached.version, "0.5.0");
        let latest = lookup_release("0.4.0", &tag("v0.5.2"), true, |tag, version| {
            assert_eq!(tag, "v0.5.2");
            assert_eq!(version, "0.5.2");
            Some((PathBuf::from("nocho-setup-0.5.2.exe"), "reciente".into()))
        })
        .unwrap()
        .unwrap();
        assert_eq!(latest.version, "0.5.2");
        assert_eq!(
            latest.installer.unwrap(),
            PathBuf::from("nocho-setup-0.5.2.exe")
        );
        assert_eq!(latest.digest, "reciente");
    }

    #[test]
    fn no_reutiliza_un_instalador_anterior_si_falla_la_descarga_reciente() {
        let latest = lookup_release("0.4.0", &tag("v0.5.2"), true, |_, _| None)
            .unwrap()
            .unwrap();
        assert_eq!(latest.version, "0.5.2");
        assert!(latest.installer.is_none());
        assert!(latest.digest.is_empty());
    }

    #[test]
    fn no_descarga_si_ya_esta_al_dia_o_el_release_es_invalido() {
        let no_download = |_: &str, _: &str| -> Option<(PathBuf, String)> {
            panic!("no debe descargar este release")
        };
        assert!(lookup_release("0.5.2", &tag("v0.5.2"), true, no_download)
            .unwrap()
            .is_none());
        assert!(lookup_release("0.4.0", &tag("v0.5.2-beta"), true, no_download).is_err());
        assert!(lookup_release("0.4.0", &tag("v0.5.2/../x"), true, no_download).is_err());
        // Otra redirección (p. ej. a la página de inicio de sesión) no cuenta como release.
        assert!(lookup_release(
            "0.4.0",
            "https://github.com/login?return_to=v9.9.9",
            true,
            no_download
        )
        .is_err());
        assert!(lookup_release(
            "0.4.0",
            "https://github.com/otro/repo/releases/tag/v9.9.9",
            true,
            no_download
        )
        .is_err());
        let portable = lookup_release("0.4.0", &tag("v0.5.2"), false, no_download)
            .unwrap()
            .unwrap();
        assert!(portable.installer.is_none());
    }

    #[test]
    fn solo_versiones_simples() {
        assert!(is_plain_version("0.4.5"));
        assert!(is_plain_version("1.0"));
        assert!(!is_plain_version(""));
        assert!(!is_plain_version("0.4.5-beta"));
        assert!(!is_plain_version("..\\..\\x"));
        assert!(!is_plain_version("0.4.5/../../evil"));
        assert!(!is_plain_version("1.2.3.4.5"));
    }

    /// El modo ayudante no ejecuta nada que no sea un instalador de Nocho verificado.
    #[test]
    fn ayudante_rechaza_otros_programas() {
        let dir = std::env::temp_dir();
        let fake = dir.join("nocho-setup-9.9.9.exe");
        std::fs::write(&fake, b"no es un instalador").unwrap();
        let real_digest = sha256_hex(b"no es un instalador").unwrap();
        let arg = |p: &Path| p.to_string_lossy().to_string();
        assert!(file_matches(&fake, &real_digest));
        assert!(!file_matches(&fake, "00"));
        assert!(!file_matches(&fake, ""));
        // Huella equivocada, nombre o carpeta ajenos, tareas inventadas: no se ejecuta nada
        // (si se ejecutara, `status` fallaría igual; lo que importa es que vuelve enseguida).
        for args in [
            vec![arg(&fake), TASKS[0].into(), "00".into()],
            vec![arg(&fake), "/TASKS=otra".into(), real_digest.clone()],
            vec![
                "C:\\Windows\\System32\\calc.exe".into(),
                TASKS[0].into(),
                real_digest.clone(),
            ],
        ] {
            run_updater(&args);
        }
        let _ = std::fs::remove_file(&fake);
    }

    #[test]
    fn compara_versiones() {
        assert!(is_newer("0.3.0", "0.2.1"));
        assert!(is_newer("0.2.10", "0.2.9"));
        assert!(is_newer("1.0.0", "0.9.9"));
        assert!(!is_newer("0.2.1", "0.2.1"));
        assert!(!is_newer("0.2.0", "0.2.1"));
        assert!(!is_newer("0.3.0-beta", "0.2.1"));
    }

    #[test]
    fn lee_la_huella_publicada() {
        let hex = "940F75DF1C8C4B3F441D0C44E2C69B2B9B0CFAB50A5905DA6AB21A6992B49E60";
        let name = "nocho-setup-0.10.1.exe";
        let lower = Some(hex.to_ascii_lowercase());
        assert_eq!(parse_checksum(&format!("{hex}  {name}\n"), name), lower);
        assert_eq!(parse_checksum(&format!("{hex} *{name}\r\n"), name), lower);
        assert_eq!(parse_checksum(&format!("{hex}\n"), name), lower);
        assert_eq!(parse_checksum(&format!("{hex}  otro.exe"), name), None);
        assert_eq!(parse_checksum(&format!("{hex}  {name} extra"), name), None);
        assert_eq!(parse_checksum("abc  nocho-setup-0.10.1.exe", name), None);
        assert_eq!(parse_checksum("<html>Not Found</html>", name), None);
        assert_eq!(parse_checksum("", name), None);
    }

    #[test]
    fn huella_sha256() {
        assert_eq!(
            sha256_hex(b"abc").as_deref(),
            Some("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
        );
    }

    /// Usa la red: `cargo test -- --ignored`. Lee el último release como la app, sin la API.
    #[test]
    #[ignore]
    fn consulta_el_ultimo_release_de_github() {
        let location =
            redirect_location(DOWNLOAD_HOST, &format!("{REPO_PATH}/releases/latest")).unwrap();
        let tag = location.strip_prefix(TAG_URL).expect(&location);
        let version = tag.trim_start_matches('v');
        assert!(is_newer(version, "0.0.0"), "versión rara: {version}");
        assert!(is_plain_version(version));
        let update = lookup_release("0.0.0", &location, false, |_, _| None)
            .unwrap()
            .unwrap();
        assert_eq!(update.version, version);
    }

    /// Usa la red y necesita un release con `.sha256` (desde 0.10.1): descarga el instalador y
    /// lo verifica como lo hace la app.
    #[test]
    #[ignore]
    fn descarga_y_verifica_el_instalador() {
        let location =
            redirect_location(DOWNLOAD_HOST, &format!("{REPO_PATH}/releases/latest")).unwrap();
        let tag = location.strip_prefix(TAG_URL).unwrap();
        let version = tag.trim_start_matches('v');
        let (path, digest) =
            download_installer(tag, version).expect("descarga o verificación fallida");
        // Lo mismo que comprueba la isla justo antes de ejecutarlo.
        assert!(file_matches(&path, &digest));
    }
}
