//! Paso de Notch Isla (hasta la 0.8) a Nocho. Con el nombre cambiaron la clave de registro de
//! los ajustes, la carpeta de datos y el valor de "Iniciar con Windows". Al abrirse, Nocho trae
//! lo que dejó la versión anterior para que nadie pierda sus ajustes ni sus equipos vinculados.
//!
//! Cada paso solo actúa si existe el dato viejo y todavía no el nuevo: repetirlo no hace nada.

use std::path::{Path, PathBuf};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, ERROR_SUCCESS};
use windows::Win32::System::Registry::{
    RegCloseKey, RegCopyTreeW, RegCreateKeyExW, RegDeleteKeyValueW, RegDeleteTreeW, RegGetValueW,
    RegOpenKeyExW, HKEY, HKEY_CURRENT_USER, KEY_ALL_ACCESS, KEY_READ, REG_OPTION_NON_VOLATILE,
    RRF_RT_REG_SZ,
};
use windows::Win32::System::Threading::{OpenMutexW, SYNCHRONIZATION_SYNCHRONIZE};

use crate::{autostart, settings};

/// Clave de los ajustes de Notch Isla.
const LEGACY_KEY: PCWSTR = w!("Software\\NotchIsla");
/// Valor de "Iniciar con Windows" de Notch Isla.
const LEGACY_RUN_VALUE: PCWSTR = w!("NotchIsla");
/// Carpeta de Notch Isla en `%LOCALAPPDATA%` (identidad de Compartir cerca y mensajes recibidos).
const LEGACY_DIR: &str = "NotchIsla";
/// Candado de instancia única de Notch Isla.
const LEGACY_MUTEX: PCWSTR = w!("Local\\NotchIsla.Instancia");

/// Trae los datos de Notch Isla. Se llama al abrirse, antes de leer los ajustes.
pub fn run() {
    registry();
    data_dir();
    run_at_startup();
}

/// Cuánto se espera a que se cierre una Notch Isla abierta.
const LEGACY_WAIT: std::time::Duration = std::time::Duration::from_secs(15);

/// Espera a que no haya una Notch Isla (0.8 o anterior) abierta, porque dos islas a la vez se
/// pisarían. Al actualizarse, la Notch Isla vieja se cierra mientras el instalador ya abre Nocho:
/// si Nocho se cerrara al verla, nadie la volvería a abrir (el ayudante de la 0.8 solo busca su
/// propia isla). `false` si después de `LEGACY_WAIT` sigue abierta.
pub fn wait_for_legacy() -> bool {
    let until = std::time::Instant::now() + LEGACY_WAIT;
    while legacy_running() {
        if std::time::Instant::now() >= until {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    true
}

/// Hay una Notch Isla (0.8 o anterior) abierta.
fn legacy_running() -> bool {
    unsafe {
        match OpenMutexW(SYNCHRONIZATION_SYNCHRONIZE, false, LEGACY_MUTEX) {
            Ok(handle) => {
                let _ = CloseHandle(handle);
                true
            }
            Err(_) => false,
        }
    }
}

struct Key(HKEY);

impl Drop for Key {
    fn drop(&mut self) {
        unsafe {
            let _ = RegCloseKey(self.0);
        }
    }
}

fn open(path: PCWSTR) -> Option<Key> {
    let mut key = HKEY::default();
    (unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, path, None, KEY_READ, &mut key) } == ERROR_SUCCESS)
        .then_some(Key(key))
}

/// Copia `HKCU\Software\NotchIsla` en la clave nueva y borra la vieja.
fn registry() {
    move_key(LEGACY_KEY, settings::KEY);
}

fn move_key(legacy: PCWSTR, current: PCWSTR) {
    if open(current).is_some() {
        return;
    }
    let Some(old) = open(legacy) else {
        return;
    };
    let mut new = HKEY::default();
    let created = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            current,
            None,
            None,
            REG_OPTION_NON_VOLATILE,
            KEY_ALL_ACCESS,
            None,
            &mut new,
            None,
        )
    };
    if created != ERROR_SUCCESS {
        return;
    }
    let new = Key(new);
    let copied = unsafe { RegCopyTreeW(old.0, None, new.0) } == ERROR_SUCCESS;
    drop(old);
    if copied {
        unsafe {
            let _ = RegDeleteTreeW(HKEY_CURRENT_USER, legacy);
        }
    }
}

/// Pasa `%LOCALAPPDATA%\NotchIsla` a la carpeta nueva.
fn data_dir() {
    let Some(base) = std::env::var_os("LOCALAPPDATA").map(PathBuf::from) else {
        return;
    };
    move_data(&base.join(LEGACY_DIR), &base.join(settings::DATA_DIR));
}

/// Copia los datos de `old` a `new` y después borra `old`. No se renombra la carpeta: basta
/// con que el indexador o el antivirus la tengan abierta para que Windows lo impida. Solo
/// actúa si `new` no tiene datos todavía (puede existir vacía si algo la creó antes).
fn move_data(old: &Path, new: &Path) {
    let new_is_empty = match std::fs::read_dir(new) {
        Ok(mut entries) => entries.next().is_none(),
        Err(_) => !new.exists(),
    };
    if !old.is_dir() || !new_is_empty {
        return;
    }
    if copy_tree(old, new).is_err() {
        // A medias no sirve: se deja la carpeta vieja intacta y se reintenta al próximo inicio.
        let _ = std::fs::remove_dir_all(new);
        return;
    }
    fix_inbox_paths(old, new);
    let _ = std::fs::remove_dir_all(old);
}

fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

/// Los mensajes recibidos guardan la ruta completa de sus imágenes, dentro de la carpeta de
/// datos: se cambian a la carpeta nueva para no perderlos.
fn fix_inbox_paths(old: &Path, new: &Path) {
    let inbox = new.join("share-inbox.json");
    let (Ok(text), Some(old), Some(new)) = (
        std::fs::read_to_string(&inbox),
        json_path(old),
        json_path(new),
    ) else {
        return;
    };
    if text.contains(&old) {
        let _ = std::fs::write(&inbox, text.replace(&old, &new));
    }
}

/// La ruta tal como queda escrita dentro de un texto JSON (con las barras escapadas).
fn json_path(path: &Path) -> Option<String> {
    let quoted = serde_json::to_string(path.to_str()?).ok()?;
    Some(quoted.trim_matches('"').to_owned())
}

/// Cambia el valor viejo de "Iniciar con Windows" por el nuevo, que apunta a este ejecutable.
fn run_at_startup() {
    let exists = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            autostart::RUN_KEY,
            LEGACY_RUN_VALUE,
            RRF_RT_REG_SZ,
            None,
            None,
            None,
        )
    } == ERROR_SUCCESS;
    if exists {
        unsafe {
            let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, autostart::RUN_KEY, LEGACY_RUN_VALUE);
        }
        autostart::set(true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mueve_los_datos_y_corrige_las_rutas_de_los_mensajes() {
        let base = tempfile::tempdir().unwrap();
        let (old, new) = (base.path().join("NotchIsla"), base.path().join("Nocho"));
        std::fs::create_dir_all(old.join("share-inbox")).unwrap();
        std::fs::write(old.join("share.dat"), b"identidad").unwrap();
        let image = old.join("share-inbox").join("1.png");
        std::fs::write(&image, b"png").unwrap();
        let inbox =
            serde_json::json!([{ "sender": "PC", "content": { "kind": "image", "path": image } }]);
        std::fs::write(old.join("share-inbox.json"), inbox.to_string()).unwrap();
        // Una carpeta nueva vacía no impide la migración.
        std::fs::create_dir_all(&new).unwrap();

        move_data(&old, &new);

        assert!(!old.exists());
        assert_eq!(std::fs::read(new.join("share.dat")).unwrap(), b"identidad");
        let moved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(new.join("share-inbox.json")).unwrap()).unwrap();
        let path = PathBuf::from(moved[0]["content"]["path"].as_str().unwrap());
        assert_eq!(path, new.join("share-inbox").join("1.png"));
        assert!(path.exists());
    }

    #[test]
    fn no_pisa_datos_que_ya_estan_en_la_carpeta_nueva() {
        let base = tempfile::tempdir().unwrap();
        let (old, new) = (base.path().join("NotchIsla"), base.path().join("Nocho"));
        std::fs::create_dir_all(&old).unwrap();
        std::fs::create_dir_all(&new).unwrap();
        std::fs::write(old.join("share.dat"), b"vieja").unwrap();
        std::fs::write(new.join("share.dat"), b"nueva").unwrap();

        move_data(&old, &new);

        assert_eq!(std::fs::read(new.join("share.dat")).unwrap(), b"nueva");
        assert!(old.exists());
    }

    #[test]
    fn mueve_la_clave_de_registro_con_sus_valores() {
        use windows::Win32::System::Registry::{RegSetKeyValueW, REG_DWORD};
        let (old, new) = (
            w!(r"Software\NochoPruebaMigracionVieja"),
            w!(r"Software\NochoPruebaMigracionNueva"),
        );
        unsafe {
            let _ = RegDeleteTreeW(HKEY_CURRENT_USER, old);
            let _ = RegDeleteTreeW(HKEY_CURRENT_USER, new);
            let value = 1u32.to_le_bytes();
            assert_eq!(
                RegSetKeyValueW(
                    HKEY_CURRENT_USER,
                    old,
                    w!("LyricsIsland"),
                    REG_DWORD.0,
                    Some(value.as_ptr().cast()),
                    4,
                ),
                ERROR_SUCCESS
            );
        }

        move_key(old, new);

        let moved = open(new).is_some();
        let left = open(old).is_some();
        unsafe {
            let _ = RegDeleteTreeW(HKEY_CURRENT_USER, old);
            let _ = RegDeleteTreeW(HKEY_CURRENT_USER, new);
        }
        assert!(moved, "la clave nueva debe existir");
        assert!(!left, "la clave vieja debe borrarse");
    }
}
