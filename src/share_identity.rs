//! Identidad TLS y equipos de confianza, cifrados con DPAPI para el usuario de Windows.
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use serde::{Deserialize, Serialize};
use windows::core::w;
use windows::Win32::Foundation::{LocalFree, HLOCAL};
use windows::Win32::Security::Cryptography::{
    CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
const MAX_DEVICES: usize = 20;
const MAX_STATE: u64 = 256 * 1024;

#[derive(Clone, Serialize, Deserialize)]
pub struct TrustedDevice {
    pub name: String,
    pub certificate: Vec<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    alias: Option<String>,
}

impl TrustedDevice {
    pub fn display_name(&self) -> &str {
        self.alias.as_deref().unwrap_or(&self.name)
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct State {
    version: u8,
    certificate: Vec<u8>,
    private_key: Vec<u8>,
    enabled: bool,
    devices: Vec<TrustedDevice>,
}

pub struct Store {
    path: PathBuf,
    state: Mutex<State>,
}

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShareIdentity").finish_non_exhaustive()
    }
}

impl Store {
    pub fn load() -> Result<Self> {
        let directory = std::env::var_os("LOCALAPPDATA")
            .ok_or("No se encontró la carpeta de datos del usuario.")?;
        Self::at(Path::new(&directory).join("NotchIsla").join("share.dat"))
    }

    pub(crate) fn at(path: PathBuf) -> Result<Self> {
        let state = match std::fs::metadata(&path) {
            Ok(info) => {
                if info.len() > MAX_STATE {
                    return Err("Los datos de vinculación son demasiado grandes.".into());
                }
                let data = protect(&std::fs::read(&path)?, false)?;
                let state: State = serde_json::from_slice(&data)?;
                if state.version != 1 || state.devices.len() > MAX_DEVICES {
                    return Err("Los datos de vinculación usan un formato incompatible.".into());
                }
                // No sustituir silenciosamente una identidad dañada: invalidaría las vinculaciones.
                validate_credentials(&state)?;
                state
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let identity = rcgen::generate_simple_self_signed(vec!["notchisla.local".into()])?;
                State {
                    version: 1,
                    certificate: identity.cert.der().to_vec(),
                    private_key: identity.signing_key.serialize_der(),
                    enabled: false,
                    devices: Vec::new(),
                }
            }
            Err(error) => return Err(error.into()),
        };
        let store = Self {
            path,
            state: Mutex::new(state),
        };
        if !store.path.exists() {
            store.persist(&store.state.lock().unwrap())?;
        }
        Ok(store)
    }

    pub fn credentials(&self) -> (CertificateDer<'static>, PrivateKeyDer<'static>) {
        let state = self.state.lock().unwrap();
        (
            CertificateDer::from(state.certificate.clone()),
            PrivatePkcs8KeyDer::from(state.private_key.clone()).into(),
        )
    }

    pub fn devices(&self) -> Vec<TrustedDevice> {
        self.state.lock().unwrap().devices.clone()
    }
    pub fn enabled(&self) -> bool {
        self.state.lock().unwrap().enabled
    }
    pub fn known(&self, certificate: &[u8]) -> Option<TrustedDevice> {
        self.state
            .lock()
            .unwrap()
            .devices
            .iter()
            .find(|device| device.certificate == certificate)
            .cloned()
    }

    pub fn remember(&self, name: String, certificate: Vec<u8>) -> Result<()> {
        self.update(|state| {
            if let Some(device) = state
                .devices
                .iter_mut()
                .find(|device| device.certificate == certificate)
            {
                device.name = name;
            } else {
                if state.devices.len() >= MAX_DEVICES {
                    return Err(
                        "Ya hay 20 equipos vinculados. Olvida uno antes de agregar otro.".into(),
                    );
                }
                let mut roots = rustls::RootCertStore::empty();
                roots.add(CertificateDer::from(certificate.clone()))?;
                state.devices.push(TrustedDevice {
                    name,
                    certificate,
                    alias: None,
                });
            }
            Ok(())
        })
    }

    pub fn forget(&self, certificate: &[u8]) -> Result<()> {
        self.update(|state| {
            state
                .devices
                .retain(|device| device.certificate != certificate);
            Ok(())
        })
    }

    /// El alias se guarda localmente y la confianza sigue asociada al certificado.
    pub fn rename(&self, certificate: &[u8], alias: &str) -> Result<()> {
        let alias = alias.trim();
        if alias.chars().count() > 48 || alias.chars().any(char::is_control) {
            return Err("Usa un nombre de hasta 48 caracteres, sin saltos de línea.".into());
        }
        self.update(|state| {
            let device = state
                .devices
                .iter_mut()
                .find(|device| device.certificate == certificate)
                .ok_or("El equipo ya no está vinculado.")?;
            device.alias = (!alias.is_empty()).then(|| alias.to_owned());
            Ok(())
        })
    }

    pub fn set_enabled(&self, enabled: bool) -> Result<()> {
        self.update(|state| {
            state.enabled = enabled;
            Ok(())
        })
    }

    fn update(&self, apply: impl FnOnce(&mut State) -> Result<()>) -> Result<()> {
        let mut guard = self.state.lock().unwrap();
        let mut next = guard.clone();
        apply(&mut next)?;
        self.persist(&next)?;
        *guard = next;
        Ok(())
    }

    fn persist(&self, state: &State) -> Result<()> {
        use std::io::Write;
        let data = protect(&serde_json::to_vec(state)?, true)?;
        if data.len() as u64 > MAX_STATE {
            return Err("Los datos de vinculación superan el límite.".into());
        }
        let parent = self.path.parent().ok_or("Ruta de vinculación inválida.")?;
        std::fs::create_dir_all(parent)?;
        let mut file = tempfile::NamedTempFile::new_in(parent)?;
        file.write_all(&data)?;
        file.as_file().sync_all()?;
        file.persist(&self.path)?;
        Ok(())
    }
}

fn validate_credentials(state: &State) -> Result<()> {
    rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(
            vec![CertificateDer::from(state.certificate.clone())],
            PrivatePkcs8KeyDer::from(state.private_key.clone()).into(),
        )?;
    Ok(())
}

fn protect(data: &[u8], encrypt: bool) -> Result<Vec<u8>> {
    let input = CRYPT_INTEGER_BLOB {
        cbData: u32::try_from(data.len())?,
        pbData: data.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    unsafe {
        if encrypt {
            CryptProtectData(
                &input,
                w!("Notch Isla · Equipos vinculados"),
                None,
                None,
                None,
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )?;
        } else {
            CryptUnprotectData(
                &input,
                None,
                None,
                None,
                None,
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )?;
        }
        let result = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
        let _ = LocalFree(Some(HLOCAL(output.pbData.cast())));
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conserva_identidad_y_confianza_cifradas_y_permite_revocar() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("share.dat");
        let store = Store::at(path.clone())?;
        let other = Store::at(directory.path().join("other.dat"))?;
        let cert = other.credentials().0.to_vec();
        let identity = store.credentials().0;
        store.remember("Mi portátil".into(), cert.clone())?;
        store.set_enabled(true)?;
        let bytes = std::fs::read(&path)?;
        assert!(!bytes.windows(11).any(|slice| slice == b"private_key"));
        let restored = Store::at(path.clone())?;
        assert_eq!(restored.credentials().0, identity);
        assert!(restored.enabled());
        assert_eq!(restored.known(&cert).unwrap().display_name(), "Mi portátil");
        restored.rename(&cert, "  PC del estudio  ")?;
        restored.remember("DESKTOP-123".into(), cert.clone())?;
        let restored = Store::at(path.clone())?;
        assert_eq!(
            restored.known(&cert).unwrap().display_name(),
            "PC del estudio"
        );
        restored.rename(&cert, "")?;
        assert_eq!(restored.known(&cert).unwrap().display_name(), "DESKTOP-123");
        restored.forget(&cert)?;
        assert!(restored.rename(&cert, "No vinculado").is_err());
        assert!(Store::at(path)?.known(&cert).is_none());
        Ok(())
    }

    #[test]
    fn no_reemplaza_identidad_corrupta() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("share.dat");
        std::fs::write(&path, b"datos corruptos")?;
        assert!(Store::at(path.clone()).is_err());
        assert_eq!(std::fs::read(path)?, b"datos corruptos");
        Ok(())
    }
}
