//! Equipos vinculados por TLS mutuo, descubrimiento mDNS y recepción persistente.

use crate::share_identity::Store;
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, SocketAddrV4, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo, TxtProperty};
use rustls::pki_types::{CertificateDer, ServerName};
use rustls::{
    ClientConfig, ClientConnection, RootCertStore, ServerConfig, ServerConnection, StreamOwned,
};
use serde::{Deserialize, Serialize};

const SERVICE: &str = "_notchisla._tcp.local.";
const TLS_NAME: &str = "notchisla.local";
const PROTOCOL: u8 = 2;
const MAX_HEADER: usize = 16 * 1024;
const MAX_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_ADDRESSES: usize = 32;
const CONNECT_TIME: Duration = Duration::from_secs(2);
const DISCOVERY_TIME: Duration = Duration::from_secs(3);
const PAIRING_TIME: Duration = Duration::from_secs(120);
const APPROVAL_TIME: Duration = Duration::from_secs(60);
const IO_TIME: Duration = Duration::from_secs(10);
const TRANSFER_TIME: Duration = Duration::from_secs(600);
pub const MAX_ITEMS: usize = 20;
pub const MAX_IMAGE: u64 = 32 * 1024 * 1024;
pub const MAX_TEXT: usize = 256 * 1024;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

pub enum Payload {
    Files(Vec<PathBuf>),
    Text(String),
    Image(Vec<u8>),
}

#[derive(Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentKind {
    #[default]
    Files,
    Text,
    Image,
}

pub enum Received {
    Files(PathBuf),
    Text {
        sender: String,
        text: String,
    },
    Image {
        sender: String,
        directory: tempfile::TempDir,
        path: PathBuf,
    },
}

pub fn is_image(path: &Path) -> bool {
    path.extension().and_then(|s| s.to_str()).is_some_and(|s| {
        ["png", "jpg", "jpeg", "bmp", "gif", "webp"].contains(&s.to_ascii_lowercase().as_str())
    })
}

#[derive(Clone)]
pub struct Peer {
    pub name: String,
    pub code: String,
    addresses: Vec<SocketAddrV4>,
    pub certificate: CertificateDer<'static>,
    pub pairable: bool,
}

#[derive(Debug, Default)]
pub struct Pairing(Mutex<Option<Instant>>);

impl Pairing {
    pub fn open(&self) {
        *self.0.lock().unwrap() = Some(Instant::now() + PAIRING_TIME);
    }
    pub fn close(&self) {
        *self.0.lock().unwrap() = None;
    }
    pub fn is_open(&self) -> bool {
        self.0
            .lock()
            .unwrap()
            .is_some_and(|until| Instant::now() < until)
    }
}

pub enum ReceiveEvent {
    Ready(String),
    Pair {
        name: String,
        code: String,
        decision: Sender<bool>,
    },
    Paired(String),
    Completed {
        result: Result<Option<Received>>,
        resume: Sender<()>,
    },
    Offer {
        description: String,
        decision: Sender<bool>,
    },
}

struct Socket {
    tcp: TcpStream,
    deadline: Instant,
    idle: Duration,
}

impl Socket {
    fn new(tcp: TcpStream, time: Duration) -> Self {
        Self {
            tcp,
            deadline: Instant::now() + time,
            idle: time.min(IO_TIME),
        }
    }

    fn time_left(&self) -> io::Result<Duration> {
        self.deadline
            .checked_duration_since(Instant::now())
            .filter(|time| !time.is_zero())
            .map(|time| time.min(self.idle))
            .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "Se agotó el tiempo del envío."))
    }

    fn allow(&mut self, time: Duration, idle: Duration) {
        self.deadline = Instant::now() + time;
        self.idle = idle;
    }
}

impl Read for Socket {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.tcp.set_read_timeout(Some(self.time_left()?))?;
        self.tcp.read(buffer)
    }
}

impl Write for Socket {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.tcp.set_write_timeout(Some(self.time_left()?))?;
        self.tcp.write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.tcp.flush()
    }
}

#[derive(Serialize, Deserialize)]
struct Entry {
    name: String,
    bytes: u64,
}

#[derive(Serialize, Deserialize)]
struct Offer {
    version: u8,
    sender: String,
    entries: Vec<Entry>,
    #[serde(default)]
    kind: ContentKind,
}

fn local_name() -> String {
    clean_label(&std::env::var("COMPUTERNAME").unwrap_or_default())
}

fn clean_label(name: &str) -> String {
    let label: String = name
        .chars()
        .filter(|&c| !hidden_character(c))
        .take(48)
        .collect();
    if label.trim().is_empty() {
        "Notch Isla".into()
    } else {
        label
    }
}

fn hidden_character(c: char) -> bool {
    c.is_control() || matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
}

fn certificate_code(cert: &[u8]) -> String {
    let hash = ring::digest::digest(&ring::digest::SHA256, cert);
    hash.as_ref()[..6]
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect()
}

struct Discovery(ServiceDaemon);

impl Drop for Discovery {
    fn drop(&mut self) {
        let _ = self.0.shutdown();
    }
}

pub fn discover(store: &Store) -> Result<Vec<Peer>> {
    let own = store.credentials().0;
    let daemon = Discovery(ServiceDaemon::new()?);
    let events = daemon.0.browse(SERVICE)?;
    let until = Instant::now() + DISCOVERY_TIME;
    let mut peers = BTreeMap::new();
    while let Ok(event) = events.recv_timeout(until.saturating_duration_since(Instant::now())) {
        match event {
            ServiceEvent::ServiceResolved(service) if peers.len() < 64 => {
                if service.get_property_val_str("protocol") != Some("2") {
                    continue;
                }
                let mut cert = Vec::new();
                for key in ["cert0", "cert1", "cert2", "cert3"] {
                    let Some(Some(part)) = service.get_property_val(key) else {
                        break;
                    };
                    cert.extend_from_slice(part);
                }
                let code = certificate_code(&cert);
                let addresses =
                    discovered_addresses(service.get_addresses_v4(), service.get_port());
                if !cert.is_empty()
                    && cert != own.as_ref()
                    && !addresses.is_empty()
                    && service.get_port() != 0
                {
                    let name = clean_label(service.get_property_val_str("name").unwrap_or(""));
                    peers.insert(
                        service.get_fullname().to_owned(),
                        Peer {
                            name,
                            code,
                            addresses,
                            certificate: cert.into(),
                            pairable: service.get_property_val_str("pairing") == Some("1"),
                        },
                    );
                }
            }
            ServiceEvent::ServiceRemoved(_, name) => {
                peers.remove(&name);
            }
            _ => {}
        }
        if Instant::now() >= until {
            break;
        }
    }
    let mut peers: Vec<_> = peers.into_values().collect();
    peers.sort_by(|left, right| left.name.cmp(&right.name).then(left.code.cmp(&right.code)));
    peers.truncate(10);
    Ok(peers)
}

fn discovered_addresses(
    addresses: impl IntoIterator<Item = Ipv4Addr>,
    port: u16,
) -> Vec<SocketAddrV4> {
    let mut addresses: Vec<_> = addresses
        .into_iter()
        .filter(|ip| (ip.is_private() || ip.is_link_local()) && !ip.is_broadcast())
        .map(|ip| SocketAddrV4::new(ip, port))
        .collect();
    // No recortar a cuatro: WSL, VMware y adaptadores desconectados pueden preceder al Wi-Fi.
    addresses.sort_unstable_by_key(|address| (address.ip().is_link_local(), *address));
    addresses.dedup();
    if addresses.len() > MAX_ADDRESSES {
        return Vec::new();
    }
    addresses
}

impl Offer {
    fn validate(&self) -> Result<()> {
        if self.version != PROTOCOL || self.entries.is_empty() || self.entries.len() > MAX_ITEMS {
            return Err("El envío usa un formato incompatible.".into());
        }
        let size = self
            .entries
            .iter()
            .try_fold(0u64, |size, entry| size.checked_add(entry.bytes));
        if size.is_none_or(|size| size > MAX_BYTES) {
            return Err("El envío supera 1 GB.".into());
        }
        match self.kind {
            ContentKind::Text if self.entries.len() != 1 || size.unwrap() > MAX_TEXT as u64 => {
                return Err("El texto supera el límite permitido.".into())
            }
            ContentKind::Image if self.entries.len() != 1 || size.unwrap() > MAX_IMAGE => {
                return Err("La imagen supera 32 MB.".into())
            }
            _ => {}
        }
        Ok(())
    }

    fn description(&self) -> String {
        let names: Vec<_> = self
            .entries
            .iter()
            .take(5)
            .map(|entry| clean_label(&entry.name))
            .collect();
        let more = self.entries.len().saturating_sub(names.len());
        let total: u64 = self.entries.iter().map(|entry| entry.bytes).sum();
        let suffix = if more == 0 {
            String::new()
        } else {
            format!(" y {more} más")
        };
        format!(
            "{} quiere enviarte {} elemento(s), {:.1} MB.\n\n{}{suffix}",
            clean_label(&self.sender),
            self.entries.len(),
            total as f64 / 1_048_576.0,
            names.join("\n")
        )
    }
}

fn write_offer(stream: &mut impl Write, offer: &Offer) -> Result<()> {
    let data = serde_json::to_vec(offer)?;
    if data.len() > MAX_HEADER {
        return Err("Los nombres de archivo son demasiado largos.".into());
    }
    stream.write_all(&(data.len() as u32).to_be_bytes())?;
    stream.write_all(&data)?;
    stream.flush()?;
    Ok(())
}

fn read_offer(stream: &mut impl Read) -> Result<Offer> {
    let mut length = [0; 4];
    stream.read_exact(&mut length)?;
    let length = u32::from_be_bytes(length) as usize;
    if length > MAX_HEADER {
        return Err("La solicitud de envío es demasiado grande.".into());
    }
    let mut bytes = vec![0; length];
    stream.read_exact(&mut bytes)?;
    let offer: Offer = serde_json::from_slice(&bytes)?;
    offer.validate()?;
    Ok(offer)
}

fn copy_exact(source: impl Read, dest: &mut impl Write, size: u64) -> Result<()> {
    if io::copy(&mut source.take(size), dest)? != size {
        return Err("La transferencia se interrumpió o el archivo cambió.".into());
    }
    Ok(())
}

pub fn send(store: &Store, peer: Peer, payload: Payload) -> Result<()> {
    if store.known(&peer.certificate).is_none() {
        return Err("Vincula este equipo antes de enviarle contenido.".into());
    }
    let mut entries = Vec::new();
    let mut sources: Vec<Box<dyn Read>> = Vec::new();
    let kind = match &payload {
        Payload::Text(_) => ContentKind::Text,
        Payload::Image(_) => ContentKind::Image,
        Payload::Files(paths)
            if paths.len() == 1
                && is_image(&paths[0])
                && std::fs::metadata(&paths[0])?.len() <= MAX_IMAGE =>
        {
            ContentKind::Image
        }
        _ => ContentKind::Files,
    };
    match payload {
        Payload::Text(text) => {
            entries.push(Entry {
                name: "Mensaje.txt".into(),
                bytes: text.len() as u64,
            });
            sources.push(Box::new(io::Cursor::new(text.into_bytes())));
        }
        Payload::Image(bytes) => {
            entries.push(Entry {
                name: "Imagen.bmp".into(),
                bytes: bytes.len() as u64,
            });
            sources.push(Box::new(io::Cursor::new(bytes)));
        }
        Payload::Files(paths) => {
            for path in paths {
                let file = File::open(&path)?;
                let info = file.metadata()?;
                if !info.is_file() {
                    return Err("Arrastra archivos o fotos individuales.".into());
                }
                let name = path.file_name().ok_or("Archivo sin nombre.")?;
                entries.push(Entry {
                    name: name.to_string_lossy().into_owned(),
                    bytes: info.len(),
                });
                sources.push(Box::new(file));
            }
        }
    }
    let offer = Offer {
        version: PROTOCOL,
        sender: local_name(),
        entries,
        kind,
    };
    offer.validate()?;

    let mut stream = connect(store, &peer)?;
    write_message(
        &mut stream,
        &Request {
            version: PROTOCOL,
            sender: local_name(),
            pairing: false,
        },
    )?;
    write_offer(&mut stream, &offer)?;

    stream
        .sock
        .allow(APPROVAL_TIME + IO_TIME, APPROVAL_TIME + IO_TIME);
    let mut answer = [0];
    stream.read_exact(&mut answer)?;
    if answer != [1] {
        return Err("El destinatario rechazó el envío o se agotó el tiempo.".into());
    }
    stream.sock.allow(TRANSFER_TIME, IO_TIME);
    for (entry, source) in offer.entries.iter().zip(sources) {
        copy_exact(source, &mut stream, entry.bytes)?;
    }
    stream.flush()?;
    stream.read_exact(&mut answer)?;
    if answer != [1] {
        return Err("El destinatario no pudo guardar el envío.".into());
    }
    Ok(())
}

#[derive(Serialize, Deserialize)]
struct Request {
    version: u8,
    sender: String,
    pairing: bool,
}

fn write_message(stream: &mut impl Write, value: &impl Serialize) -> Result<()> {
    let bytes = serde_json::to_vec(value)?;
    if bytes.len() > MAX_HEADER {
        return Err("La solicitud es demasiado grande.".into());
    }
    stream.write_all(&(bytes.len() as u32).to_be_bytes())?;
    stream.write_all(&bytes)?;
    stream.flush()?;
    Ok(())
}

fn read_message<T: serde::de::DeserializeOwned>(stream: &mut impl Read) -> Result<T> {
    let mut length = [0; 4];
    stream.read_exact(&mut length)?;
    let length = u32::from_be_bytes(length) as usize;
    if length == 0 || length > MAX_HEADER {
        return Err("La solicitud es demasiado grande.".into());
    }
    let mut bytes = vec![0; length];
    stream.read_exact(&mut bytes)?;
    Ok(serde_json::from_slice(&bytes)?)
}

/// Prueba las rutas en paralelo; una IP virtual sin respuesta no retrasa la Wi-Fi.
/// Al elegir un socket se cierran los demás, sin iniciar envíos ni handshakes duplicados.
fn connect_tcp(
    addresses: &[SocketAddrV4],
    timeout: Duration,
) -> io::Result<(SocketAddrV4, TcpStream)> {
    let (tx, rx) = std::sync::mpsc::channel();
    for &address in addresses {
        let tx = tx.clone();
        std::thread::spawn(move || {
            let result = TcpStream::connect_timeout(&address.into(), timeout);
            let _ = tx.send((address, result));
        });
    }
    drop(tx);
    let mut failure = io::Error::new(
        io::ErrorKind::NotConnected,
        "No hay direcciones disponibles.",
    );
    for (address, result) in rx {
        match result {
            Ok(socket) => return Ok((address, socket)),
            Err(error) => {
                failure = error;
            }
        }
    }
    Err(failure)
}

fn connect(store: &Store, peer: &Peer) -> Result<StreamOwned<ClientConnection, Socket>> {
    let mut roots = RootCertStore::empty();
    roots.add(peer.certificate.clone())?;
    let (certificate, key) = store.credentials();
    let config = Arc::new(
        ClientConfig::builder()
            .with_root_certificates(roots)
            .with_client_auth_cert(vec![certificate], key)?,
    );
    if peer.addresses.is_empty() || peer.addresses.len() > MAX_ADDRESSES {
        return Err("El equipo no anunció una dirección de red válida. Vuelve a buscarlo.".into());
    }
    let deadline = Instant::now() + IO_TIME;
    let mut addresses = peer.addresses.clone();
    addresses.sort_unstable();
    addresses.dedup();
    let mut tcp_error = None;
    let mut tls_error = None;
    while !addresses.is_empty() {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        let (address, socket) = match connect_tcp(&addresses, remaining.min(CONNECT_TIME)) {
            Ok(connected) => connected,
            Err(error) => {
                tcp_error = Some(error);
                break;
            }
        };
        addresses.retain(|candidate| *candidate != address);
        let connection = ClientConnection::new(config.clone(), ServerName::try_from(TLS_NAME)?)?;
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        let mut stream = StreamOwned::new(
            connection,
            Socket::new(socket, remaining.min(Duration::from_secs(3))),
        );
        let handshake = (|| -> io::Result<()> {
            while stream.conn.is_handshaking() {
                stream.conn.complete_io(&mut stream.sock)?;
            }
            Ok(())
        })();
        match handshake {
            Ok(()) => {
                stream.sock.allow(IO_TIME, IO_TIME);
                return Ok(stream);
            }
            Err(error) => {
                tls_error = Some(error);
            }
        }
    }
    if let Some(error) = tls_error {
        return Err(format!(
            "El equipo respondió, pero no se pudo verificar la conexión segura. {error}"
        )
        .into());
    }
    let detail = match tcp_error.as_ref().map(io::Error::kind) {
        Some(io::ErrorKind::ConnectionRefused) => "El equipo rechaza la conexión. Reabre Notch Isla y activa Recibir de equipos vinculados allí.",
        Some(io::ErrorKind::TimedOut) | Some(io::ErrorKind::WouldBlock) => "El equipo se anuncia, pero no responde. Revisa el permiso de red de Notch Isla en Windows y que ambos PCs estén en la misma red.",
        _ => "No se pudo abrir la conexión al equipo. Revisa su recepción, la red y el permiso de Notch Isla en Windows.",
    };
    Err(detail.into())
}

/// El código se obtiene del canal TLS: una conexión intermediaria muestra otro código.
fn pairing_code(material: &[u8]) -> String {
    let code = certificate_code(material);
    format!("{} · {} · {}", &code[..4], &code[4..8], &code[8..])
}

pub fn pair(
    store: &Store,
    peer: Peer,
    mut confirm: impl FnMut(String, String) -> bool,
) -> Result<()> {
    if !peer.pairable {
        return Err("Abre Vincular equipo en el otro PC y vuelve a buscar.".into());
    }
    let mut stream = connect(store, &peer)?;
    write_message(
        &mut stream,
        &Request {
            version: PROTOCOL,
            sender: local_name(),
            pairing: true,
        },
    )?;
    // Respuesta de preparación: también completa el handshake antes de usar el exporter.
    let mut ready = [0];
    stream.read_exact(&mut ready)?;
    if ready != [1] {
        return Err("El otro equipo no permite vincularse en este momento.".into());
    }
    let material = stream
        .conn
        .export_keying_material([0u8; 32], b"notch-isla-pairing-v2", None)?;
    stream
        .sock
        .allow(APPROVAL_TIME + IO_TIME, APPROVAL_TIME + IO_TIME);
    let accepted = confirm(peer.name.clone(), pairing_code(&material));
    stream.write_all(&[u8::from(accepted)])?;
    stream.flush()?;
    if !accepted {
        return Err("Vinculación cancelada.".into());
    }
    stream.read_exact(&mut ready)?;
    if ready != [1] {
        return Err("El otro equipo rechazó la vinculación o no pudo guardarla.".into());
    }
    store.remember(peer.name, peer.certificate.to_vec())?;
    Ok(())
}

/// Verifica la posesión de la clave mediante TLS. Solo admite identidades guardadas,
/// salvo durante la ventana explícita de vinculación. No omite firmas ni validación X.509.
#[derive(Debug)]
struct KnownClients {
    store: Arc<Store>,
    pairing: Arc<Pairing>,
}

impl rustls::server::danger::ClientCertVerifier for KnownClients {
    fn root_hint_subjects(&self) -> &[rustls::DistinguishedName] {
        &[]
    }
    fn verify_client_cert(
        &self,
        cert: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        now: rustls::pki_types::UnixTime,
    ) -> std::result::Result<rustls::server::danger::ClientCertVerified, rustls::Error> {
        if self.store.known(cert).is_none() && !self.pairing.is_open() {
            return Err(rustls::Error::General("Equipo no vinculado".into()));
        }
        let mut roots = RootCertStore::empty();
        roots.add(cert.clone().into_owned())?;
        let verifier = rustls::server::WebPkiClientVerifier::builder(Arc::new(roots))
            .build()
            .map_err(|error| rustls::Error::General(error.to_string()))?;
        verifier.verify_client_cert(cert, intermediates, now)
    }
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        signature: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            signature,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        signature: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            signature,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

fn server_config(store: Arc<Store>, pairing: Arc<Pairing>) -> Result<Arc<ServerConfig>> {
    let (certificate, key) = store.credentials();
    Ok(Arc::new(
        ServerConfig::builder()
            .with_client_cert_verifier(Arc::new(KnownClients { store, pairing }))
            .with_single_cert(vec![certificate], key)?,
    ))
}

fn cancelled(cancel: &Receiver<()>) -> bool {
    !matches!(cancel.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty))
}

fn wait_decision(answer: &Receiver<bool>, cancel: &Receiver<()>) -> bool {
    let until = Instant::now() + APPROVAL_TIME;
    while Instant::now() < until && !cancelled(cancel) {
        match answer.recv_timeout(Duration::from_millis(100)) {
            Ok(value) => return value,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return false,
            Err(_) => {}
        }
    }
    false
}

enum Session {
    Paired(String),
    Received(Option<Received>),
}

fn session(
    downloads: &Path,
    stream: &mut StreamOwned<ServerConnection, Socket>,
    store: &Store,
    pairing: &Pairing,
    cancel: &Receiver<()>,
    event: &mut impl FnMut(ReceiveEvent),
) -> Result<Session> {
    let request: Request = read_message(stream)?;
    if request.version != PROTOCOL {
        return Err("Actualiza Notch Isla en ambos equipos para vincularlos.".into());
    }
    let certificate = stream
        .conn
        .peer_certificates()
        .and_then(|chain| chain.first())
        .ok_or("El equipo no presentó su identidad.")?
        .to_vec();
    if request.pairing {
        if !pairing.is_open() {
            stream.write_all(&[0])?;
            stream.flush()?;
            return Err("La vinculación no está activada.".into());
        }
        stream.write_all(&[1])?;
        stream.flush()?;
        let material =
            stream
                .conn
                .export_keying_material([0u8; 32], b"notch-isla-pairing-v2", None)?;
        let (confirm, answer) = std::sync::mpsc::channel();
        let name = clean_label(&request.sender);
        event(ReceiveEvent::Pair {
            name: name.clone(),
            code: pairing_code(&material),
            decision: confirm,
        });
        let accepted = wait_decision(&answer, cancel);
        stream
            .sock
            .allow(APPROVAL_TIME + IO_TIME, APPROVAL_TIME + IO_TIME);
        let mut other = [0];
        // Al cancelar localmente, responder sin esperar la otra confirmación.
        if !accepted {
            stream.write_all(&[0])?;
            stream.flush()?;
            return Err("Vinculación cancelada.".into());
        }
        stream.read_exact(&mut other)?;
        if other != [1] || cancelled(cancel) {
            return Err("El otro equipo canceló la vinculación.".into());
        }
        if let Err(error) = store.remember(name.clone(), certificate) {
            let _ = stream.write_all(&[0]).and_then(|_| stream.flush());
            return Err(error);
        }
        stream.write_all(&[1])?;
        stream.flush()?;
        pairing.close();
        return Ok(Session::Paired(name));
    }
    let device = store
        .known(&certificate)
        .ok_or("El equipo debe vincularse antes de enviar contenido.")?;
    let mut offer = read_offer(stream)?;
    // El nombre visible procede de la identidad guardada, nunca del envío entrante.
    offer.sender = device.name;
    if cancelled(cancel) || !store.enabled() || store.known(&certificate).is_none() {
        return Err("Recepción desactivada o equipo olvidado.".into());
    }
    finish_receive(downloads, stream, offer, cancel, event).map(Session::Received)
}

fn service_info(port: u16, certificate: &[u8], pairing: bool) -> Result<ServiceInfo> {
    let code = certificate_code(certificate);
    let mut properties = vec![
        TxtProperty::from(("name", local_name())),
        TxtProperty::from(("protocol", "2")),
        TxtProperty::from(("pairing", if pairing { "1" } else { "0" })),
    ];
    for (index, part) in certificate.chunks(200).enumerate() {
        properties.push(TxtProperty::from((format!("cert{index}"), part)));
    }
    Ok(ServiceInfo::new(
        SERVICE,
        &code,
        &format!("notchisla-{code}.local."),
        "",
        port,
        properties,
    )?
    .enable_addr_auto())
}

pub fn serve(
    downloads: PathBuf,
    store: Arc<Store>,
    pairing: Arc<Pairing>,
    cancel: Receiver<()>,
    mut event: impl FnMut(ReceiveEvent),
) -> Result<()> {
    let listener = TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0))?;
    listener.set_nonblocking(true)?;
    let config = server_config(store.clone(), pairing.clone())?;
    let certificate = store.credentials().0;
    let port = listener.local_addr()?.port();
    let daemon = Discovery(ServiceDaemon::new()?);
    let mut advertised_pairing = pairing.is_open();
    let info = service_info(port, &certificate, advertised_pairing)?;
    let fullname = info.get_fullname().to_owned();
    daemon.0.register(info)?;
    event(ReceiveEvent::Ready(certificate_code(&certificate)));
    while !cancelled(&cancel) && store.enabled() {
        if advertised_pairing != pairing.is_open() {
            advertised_pairing = pairing.is_open();
            daemon
                .0
                .register(service_info(port, &certificate, advertised_pairing)?)?;
        }
        match listener.accept() {
            Ok((socket, address)) => {
                let std::net::IpAddr::V4(ip) = address.ip() else {
                    continue;
                };
                if !ip.is_private() && !ip.is_link_local() {
                    continue;
                }
                socket.set_nonblocking(false)?;
                let mut stream = StreamOwned::new(
                    ServerConnection::new(config.clone())?,
                    Socket::new(socket, Duration::from_secs(3)),
                );
                let result = session(
                    &downloads,
                    &mut stream,
                    &store,
                    &pairing,
                    &cancel,
                    &mut event,
                );
                match result {
                    Ok(Session::Paired(name)) => event(ReceiveEvent::Paired(name)),
                    result => {
                        // Una sola vista previa a la vez: espera a que la UI termine antes de aceptar otra.
                        // Errores de handshake de equipos desconocidos no generan avisos ni diálogos.
                        let known = stream
                            .conn
                            .peer_certificates()
                            .and_then(|chain| chain.first())
                            .is_some_and(|cert| store.known(cert).is_some());
                        let result = match result {
                            Ok(Session::Received(received)) => Ok(received),
                            Err(error) if known => Err(error),
                            _ => continue,
                        };
                        let (resume, resumed) = std::sync::mpsc::channel();
                        event(ReceiveEvent::Completed { result, resume });
                        while !cancelled(&cancel) {
                            match resumed.recv_timeout(Duration::from_millis(100)) {
                                Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                                    break
                                }
                                Err(_) => {}
                            }
                        }
                    }
                }
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                if cancel.recv_timeout(Duration::from_millis(100)).is_ok() {
                    break;
                }
            }
            Err(error) => return Err(error.into()),
        }
    }
    if let Ok(status) = daemon.0.unregister(&fullname) {
        let _ = status.recv_timeout(Duration::from_millis(500));
    }
    Ok(())
}

fn finish_receive(
    downloads: &Path,
    stream: &mut StreamOwned<ServerConnection, Socket>,
    offer: Offer,
    cancel: &Receiver<()>,
    mut event: impl FnMut(ReceiveEvent),
) -> Result<Option<Received>> {
    // Recepción activada por el usuario: las vistas previas son pequeñas y temporales.
    // Los archivos siguen requiriendo confirmación antes de escribirse en Descargas.
    if offer.kind != ContentKind::Files {
        stream.sock.allow(TRANSFER_TIME, IO_TIME);
        if cancelled(cancel) {
            return Ok(None);
        }
        stream.write_all(&[1])?;
        stream.flush()?;
        let received = match offer.kind {
            ContentKind::Text => {
                let mut bytes = Vec::new();
                copy_exact(&mut *stream, &mut bytes, offer.entries[0].bytes)?;
                Received::Text {
                    sender: clean_label(&offer.sender),
                    text: String::from_utf8(bytes)?,
                }
            }
            ContentKind::Image => {
                let directory = tempfile::Builder::new()
                    .prefix("notch-preview-")
                    .tempdir()?;
                let path = directory.path().join("imagen");
                let mut file = File::create_new(&path)?;
                copy_exact(&mut *stream, &mut file, offer.entries[0].bytes)?;
                Received::Image {
                    sender: clean_label(&offer.sender),
                    directory,
                    path,
                }
            }
            ContentKind::Files => unreachable!(),
        };
        stream.write_all(&[1])?;
        stream.flush()?;
        return Ok(Some(received));
    }
    let (decision, answer) = std::sync::mpsc::channel();
    event(ReceiveEvent::Offer {
        description: offer.description(),
        decision,
    });
    let accepted = wait_decision(&answer, cancel);
    stream.sock.allow(TRANSFER_TIME, IO_TIME);
    if !accepted {
        stream.write_all(&[0])?;
        stream.flush()?;
        return Ok(None);
    }
    stream.write_all(&[1])?;
    stream.flush()?;
    let saved = save_offer(downloads, stream, &offer);
    if saved.is_ok() {
        let _ = stream.write_all(&[1]).and_then(|()| stream.flush());
    }
    saved.map(|path| Some(Received::Files(path)))
}

fn save_offer(downloads: &Path, stream: &mut impl Read, offer: &Offer) -> Result<PathBuf> {
    let directory = tempfile::Builder::new()
        .prefix(".notch-isla-")
        .tempdir_in(downloads)?;
    for (index, entry) in offer.entries.iter().enumerate() {
        let name: String = entry
            .name
            .chars()
            .take(100)
            .map(|c| {
                if hidden_character(c) || "\\/:*?\"<>|".contains(c) {
                    '_'
                } else {
                    c
                }
            })
            .collect();
        let name = name.trim_matches(['.', ' ']);
        let name = if name.is_empty() { "Archivo" } else { name };
        let path = directory.path().join(format!("{:02} - {name}", index + 1));
        let mut file = File::create_new(&path)?;
        let mut zone = path.into_os_string();
        zone.push(":Zone.Identifier");
        // Conserva las comprobaciones de Windows al abrir archivos recibidos.
        std::fs::write(zone, "[ZoneTransfer]\r\nZoneId=3\r\n")?;
        copy_exact(&mut *stream, &mut file, entry.bytes)?;
        file.sync_all()?;
    }
    let suffix = directory.path().file_name().unwrap().to_string_lossy();
    let suffix = suffix.trim_start_matches(".notch-isla-");
    let destination = downloads.join(format!("Notch Isla - {suffix}"));
    std::fs::rename(directory.path(), &destination)?;
    Ok(destination)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    fn identities() -> Result<(tempfile::TempDir, Arc<Store>, Arc<Store>)> {
        let directory = tempfile::tempdir()?;
        let first = Arc::new(Store::at(directory.path().join("first.dat"))?);
        let second = Arc::new(Store::at(directory.path().join("second.dat"))?);
        first.set_enabled(true)?;
        second.set_enabled(true)?;
        Ok((directory, first, second))
    }

    fn listen(store: &Store, pairable: bool) -> Result<(TcpListener, Peer)> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        let std::net::SocketAddr::V4(address) = listener.local_addr()? else {
            unreachable!()
        };
        let certificate = store.credentials().0;
        let peer = Peer {
            name: "Receptor".into(),
            code: certificate_code(&certificate),
            addresses: vec![address],
            certificate,
            pairable,
        };
        Ok((listener, peer))
    }

    #[test]
    fn conserva_wifi_aunque_haya_mas_de_cuatro_adaptadores() {
        let wifi = Ipv4Addr::new(192, 168, 50, 20);
        let ips = [
            Ipv4Addr::new(169, 254, 10, 1),
            Ipv4Addr::new(169, 254, 20, 1),
            Ipv4Addr::new(172, 16, 0, 1),
            Ipv4Addr::new(172, 20, 128, 1),
            Ipv4Addr::new(172, 24, 0, 1),
            wifi,
            Ipv4Addr::new(192, 168, 100, 1),
            Ipv4Addr::new(192, 168, 200, 1),
        ];
        let addresses = discovered_addresses(ips, 12345);
        assert_eq!(addresses.len(), ips.len());
        assert!(addresses.contains(&SocketAddrV4::new(wifi, 12345)));
        assert!(!addresses[0].ip().is_link_local());
    }

    #[test]
    fn conecta_por_otra_ruta_sin_omitir_la_verificacion_tls() -> Result<()> {
        let (directory, first, second) = identities()?;
        let impostor = Arc::new(Store::at(directory.path().join("impostor.dat"))?);
        let pairing = Arc::new(Pairing::default());
        pairing.open();
        let valid_config = server_config(second.clone(), pairing.clone())?;
        let wrong_config = server_config(impostor.clone(), pairing)?;
        let (valid, mut peer) = listen(&second, false)?;
        let (wrong, wrong_peer) = listen(&impostor, false)?;
        let wrong_address = wrong_peer.addresses[0];
        // Una ruta acepta TCP pero presenta una identidad distinta.
        peer.addresses.insert(0, wrong_address);
        let valid_server = std::thread::spawn(move || -> Result<()> {
            // Como el listener real, descarta sockets candidatos que no llegan a iniciar TLS.
            for _ in 0..2 {
                let (socket, _) = valid.accept()?;
                let mut stream = StreamOwned::new(
                    ServerConnection::new(valid_config.clone())?,
                    Socket::new(socket, IO_TIME),
                );
                let result = (|| -> io::Result<()> {
                    while stream.conn.is_handshaking() {
                        stream.conn.complete_io(&mut stream.sock)?;
                    }
                    Ok(())
                })();
                if result.is_ok() {
                    return Ok(());
                }
            }
            Err("No llegó una conexión TLS válida.".into())
        });
        let wrong_server = std::thread::spawn(move || -> Result<()> {
            let (socket, _) = wrong.accept()?;
            let mut stream = StreamOwned::new(
                ServerConnection::new(wrong_config)?,
                Socket::new(socket, IO_TIME),
            );
            // Ninguna solicitud ni contenido debe enviarse a esta identidad.
            assert!(read_message::<Request>(&mut stream).is_err());
            Ok(())
        });
        let stream = connect(&first, &peer)?;
        assert_eq!(
            stream.conn.peer_certificates().unwrap()[0],
            second.credentials().0
        );
        drop(stream);
        valid_server.join().unwrap()?;
        wrong_server.join().unwrap()?;
        Ok(())
    }

    #[test]
    fn vincula_ambos_con_el_mismo_codigo_y_recibe_varios_envios_sin_reactivar() -> Result<()> {
        let (directory, first, second) = identities()?;
        let downloads = directory.path().join("downloads");
        std::fs::create_dir(&downloads)?;
        let (listener, peer) = listen(&second, true)?;
        let pairing = Arc::new(Pairing::default());
        pairing.open();
        let config = server_config(second.clone(), pairing.clone())?;
        let server_store = second.clone();
        let (codes, code) = mpsc::channel();
        let server = std::thread::spawn(move || -> Result<Vec<Option<Received>>> {
            let (_stop, cancel) = mpsc::channel();
            let mut received = Vec::new();
            for index in 0..5 {
                let (socket, _) = listener.accept()?;
                let mut stream = StreamOwned::new(
                    ServerConnection::new(config.clone())?,
                    Socket::new(socket, IO_TIME),
                );
                let result = session(
                    &downloads,
                    &mut stream,
                    &server_store,
                    &pairing,
                    &cancel,
                    &mut |event| match event {
                        ReceiveEvent::Pair { code, decision, .. } => {
                            codes.send(code).unwrap();
                            decision.send(true).unwrap();
                        }
                        ReceiveEvent::Offer { decision, .. } => {
                            decision.send(index != 4).unwrap();
                        }
                        _ => panic!("evento inesperado"),
                    },
                )?;
                match result {
                    Session::Paired(_) => {
                        assert_eq!(index, 0);
                        assert!(!pairing.is_open());
                    }
                    Session::Received(result) => received.push(result),
                }
            }
            Ok(received)
        });
        pair(&first, peer.clone(), |_, displayed| {
            assert_eq!(displayed, code.recv_timeout(IO_TIME).unwrap());
            true
        })?;
        assert!(first.known(&second.credentials().0).is_some());
        assert!(second.known(&first.credentials().0).is_some());
        // Reabrir el almacén simula reiniciar la app: conserva claves y confianza.
        let restored = Store::at(directory.path().join("first.dat"))?;
        send(
            &restored,
            peer.clone(),
            Payload::Text("Hola 👋\r\nsegunda línea".into()),
        )?;
        send(&restored, peer.clone(), Payload::Image(vec![1, 2, 3, 4]))?;
        let file = directory.path().join("documento.txt");
        std::fs::write(&file, "archivo aceptado")?;
        send(&restored, peer.clone(), Payload::Files(vec![file.clone()]))?;
        assert!(send(&restored, peer, Payload::Files(vec![file])).is_err());
        let mut received = server.join().unwrap()?.into_iter();
        let Some(Received::Text { sender, text }) = received.next().unwrap() else {
            panic!("esperaba texto")
        };
        assert_eq!(sender, local_name());
        assert_eq!(text, "Hola 👋\r\nsegunda línea");
        let Some(Received::Image {
            directory: temporary,
            path,
            ..
        }) = received.next().unwrap()
        else {
            panic!("esperaba imagen")
        };
        assert_eq!(std::fs::read(&path)?, [1, 2, 3, 4]);
        drop(temporary);
        assert!(!path.exists());
        let Some(Received::Files(path)) = received.next().unwrap() else {
            panic!("esperaba archivo")
        };
        assert_eq!(
            std::fs::read_to_string(path.join("01 - documento.txt"))?,
            "archivo aceptado"
        );
        assert!(received.next().unwrap().is_none());
        assert_eq!(directory.path().join("downloads").read_dir()?.count(), 1);
        Ok(())
    }

    #[test]
    fn vincular_requiere_confirmacion_de_ambos_equipos() -> Result<()> {
        for (local, remote) in [(false, true), (true, false), (false, false)] {
            let (directory, first, second) = identities()?;
            let (listener, peer) = listen(&second, true)?;
            let pairing = Arc::new(Pairing::default());
            pairing.open();
            let config = server_config(second.clone(), pairing.clone())?;
            let other = second.clone();
            let downloads = directory.path().to_owned();
            let server = std::thread::spawn(move || -> Result<()> {
                let (socket, _) = listener.accept()?;
                let mut stream =
                    StreamOwned::new(ServerConnection::new(config)?, Socket::new(socket, IO_TIME));
                let (_stop, cancel) = mpsc::channel();
                assert!(session(
                    &downloads,
                    &mut stream,
                    &other,
                    &pairing,
                    &cancel,
                    &mut |event| {
                        if let ReceiveEvent::Pair { decision, .. } = event {
                            let _ = decision.send(remote);
                        }
                    }
                )
                .is_err());
                Ok(())
            });
            assert!(pair(&first, peer, |_, _| local).is_err());
            server.join().unwrap()?;
            assert!(first.devices().is_empty());
            assert!(second.devices().is_empty());
        }
        Ok(())
    }

    #[test]
    fn desconocidos_y_revocados_no_pueden_enviar_ni_abrir_dialogos() -> Result<()> {
        for pairing_open in [false, true] {
            let (directory, attacker, receiver) = identities()?;
            // El atacante conoce al receptor; el receptor no confía en el atacante.
            attacker.remember("Receptor".into(), receiver.credentials().0.to_vec())?;
            let (listener, peer) = listen(&receiver, false)?;
            let pairing = Arc::new(Pairing::default());
            if pairing_open {
                pairing.open();
            }
            let config = server_config(receiver.clone(), pairing.clone())?;
            let downloads = directory.path().to_owned();
            let server = std::thread::spawn(move || -> Result<()> {
                let (socket, _) = listener.accept()?;
                let mut stream =
                    StreamOwned::new(ServerConnection::new(config)?, Socket::new(socket, IO_TIME));
                let (_stop, cancel) = mpsc::channel();
                assert!(session(
                    &downloads,
                    &mut stream,
                    &receiver,
                    &pairing,
                    &cancel,
                    &mut |_| panic!("un desconocido no puede abrir diálogos de contenido")
                )
                .is_err());
                Ok(())
            });
            assert!(send(&attacker, peer, Payload::Text("intruso".into())).is_err());
            server.join().unwrap()?;
        }
        let (directory, first, second) = identities()?;
        first.remember("Segundo".into(), second.credentials().0.to_vec())?;
        second.remember("Primero".into(), first.credentials().0.to_vec())?;
        let pairing = Arc::new(Pairing::default());
        let config = server_config(second.clone(), pairing.clone())?;
        // El verificador ya existe: olvidar debe surtir efecto sin reiniciar el listener.
        second.forget(&first.credentials().0)?;
        let (listener, peer) = listen(&second, false)?;
        let downloads = directory.path().to_owned();
        let server = std::thread::spawn(move || -> Result<()> {
            let (socket, _) = listener.accept()?;
            let mut stream =
                StreamOwned::new(ServerConnection::new(config)?, Socket::new(socket, IO_TIME));
            let (_stop, cancel) = mpsc::channel();
            assert!(session(
                &downloads,
                &mut stream,
                &second,
                &pairing,
                &cancel,
                &mut |_| panic!("equipo revocado")
            )
            .is_err());
            Ok(())
        });
        assert!(send(&first, peer, Payload::Text("revocado".into())).is_err());
        server.join().unwrap()?;
        Ok(())
    }

    #[test]
    fn nombre_igual_no_sustituye_identidad_y_rechaza_protocolo_anterior() -> Result<()> {
        let (directory, first, second) = identities()?;
        let impostor = Store::at(directory.path().join("impostor.dat"))?;
        first.remember("Mi PC".into(), second.credentials().0.to_vec())?;
        let (_, mut peer) = listen(&impostor, false)?;
        peer.name = "Mi PC".into();
        assert!(send(&first, peer, Payload::Text("secreto".into())).is_err());
        let legacy = Offer {
            version: 1,
            sender: "Anterior".into(),
            kind: ContentKind::Text,
            entries: vec![Entry {
                name: "Mensaje.txt".into(),
                bytes: 1,
            }],
        };
        assert!(legacy.validate().is_err());
        Ok(())
    }

    #[test]
    #[ignore = "requiere una interfaz IPv4 local y tráfico mDNS permitido"]
    fn descubre_vincula_y_comparte_en_la_red_local() -> Result<()> {
        let (directory, first, second) = identities()?;
        let first_cert = first.credentials().0;
        let second_cert = second.credentials().0;
        let (ready, started) = mpsc::channel();
        let (codes, code) = mpsc::channel();
        let (texts, text) = mpsc::channel();
        let mut workers = Vec::new();
        let mut stops = Vec::new();
        // Los dos equipos sirven y descubren con sus propias identidades desde antes de vincular.
        for store in [first.clone(), second.clone()] {
            let pairing = Arc::new(Pairing::default());
            pairing.open();
            let downloads = directory.path().to_owned();
            let (stop, cancel) = mpsc::channel();
            stops.push(stop);
            let ready = ready.clone();
            let codes = codes.clone();
            let texts = texts.clone();
            workers.push(std::thread::spawn(move || {
                serve(downloads, store, pairing, cancel, |event| match event {
                    ReceiveEvent::Ready(_) => {
                        ready.send(()).unwrap();
                    }
                    ReceiveEvent::Pair { code, decision, .. } => {
                        codes.send(code).unwrap();
                        decision.send(true).unwrap();
                    }
                    ReceiveEvent::Completed { result, resume } => {
                        let Some(Received::Text { text, .. }) = result.unwrap() else {
                            panic!("esperaba texto")
                        };
                        texts.send(text).unwrap();
                        resume.send(()).unwrap();
                    }
                    ReceiveEvent::Paired(_) => {}
                    _ => panic!("evento inesperado"),
                })
            }));
        }
        let result = (|| -> Result<()> {
            started.recv_timeout(IO_TIME)?;
            started.recv_timeout(IO_TIME)?;
            let laptop = discover(&first)?
                .into_iter()
                .find(|peer| peer.pairable && peer.certificate == second_cert)
                .ok_or("No se descubrió la laptop de prueba.")?;
            pair(&first, laptop.clone(), |_, displayed| {
                code.recv_timeout(IO_TIME)
                    .is_ok_and(|remote| remote == displayed)
            })?;
            send(&first, laptop, Payload::Text("PC a laptop".into()))?;
            assert_eq!(text.recv_timeout(IO_TIME)?, "PC a laptop");
            let pc = discover(&second)?
                .into_iter()
                .find(|peer| peer.certificate == first_cert)
                .ok_or("No se descubrió el PC de prueba.")?;
            send(&second, pc, Payload::Text("laptop a PC".into()))?;
            assert_eq!(text.recv_timeout(IO_TIME)?, "laptop a PC");
            Ok(())
        })();
        for stop in stops {
            let _ = stop.send(());
        }
        for worker in workers {
            worker.join().unwrap()?;
        }
        result
    }

    #[test]
    fn valida_limites_de_vista_previa_y_compatibilidad() -> Result<()> {
        let mut offer: Offer = serde_json::from_str(
            r#"{"version":2,"sender":"Anterior","entries":[{"name":"Mensaje.txt","bytes":4}]}"#,
        )?;
        assert!(offer.kind == ContentKind::Files);
        offer.kind = ContentKind::Text;
        offer.entries[0].bytes = MAX_TEXT as u64 + 1;
        assert!(offer.validate().is_err());
        offer.kind = ContentKind::Image;
        offer.entries[0].bytes = MAX_IMAGE + 1;
        assert!(offer.validate().is_err());
        offer.entries[0].bytes = MAX_IMAGE;
        offer.validate()?;
        offer.entries.push(Entry {
            name: "extra".into(),
            bytes: 0,
        });
        assert!(offer.validate().is_err());
        Ok(())
    }

    #[test]
    fn elimina_los_archivos_parciales_si_el_emisor_se_desconecta() -> Result<()> {
        let downloads = tempfile::tempdir()?;
        let offer = Offer {
            version: PROTOCOL,
            sender: "Prueba".into(),
            kind: ContentKind::Files,
            entries: vec![Entry {
                name: "../foto.png".into(),
                bytes: 8,
            }],
        };
        let mut partial = &b"foto"[..];
        assert!(save_offer(downloads.path(), &mut partial, &offer).is_err());
        assert_eq!(downloads.path().read_dir()?.count(), 0);
        Ok(())
    }
}
