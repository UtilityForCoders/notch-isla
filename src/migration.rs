//! Paso de Notch Isla (hasta la 0.8) a Nocho. Con el nombre cambiaron la clave de registro de
//! los ajustes, la carpeta de datos y el valor de "Iniciar con Windows". Al abrirse, Nocho trae
//! lo que dejó la versión anterior para que nadie pierda sus ajustes ni sus equipos vinculados.
//!
//! Cada paso solo actúa si existe el dato viejo y todavía no el nuevo: repetirlo no hace nada.

use std::path::PathBuf;

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

/// Hay una Notch Isla (0.8 o anterior) abierta: dos islas a la vez se pisarían.
pub fn legacy_running() -> bool {
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
    if open(settings::KEY).is_some() {
        return;
    }
    let Some(old) = open(LEGACY_KEY) else {
        return;
    };
    let mut new = HKEY::default();
    let created = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            settings::KEY,
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
            let _ = RegDeleteTreeW(HKEY_CURRENT_USER, LEGACY_KEY);
        }
    }
}

/// Renombra `%LOCALAPPDATA%\NotchIsla` a la carpeta nueva.
fn data_dir() {
    let Some(base) = std::env::var_os("LOCALAPPDATA").map(PathBuf::from) else {
        return;
    };
    let (old, new) = (base.join(LEGACY_DIR), base.join(settings::DATA_DIR));
    if old.is_dir() && !new.exists() {
        let _ = std::fs::rename(old, new);
    }
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
