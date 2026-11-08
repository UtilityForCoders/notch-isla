//! Descubrimiento mDNS y envíos TLS de una sola sesión por la red local.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, SocketAddrV4, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo, TxtProperty};
use rustls::pki_types::{CertificateDer, PrivatePkcs8KeyDer, ServerName};
use rustls::{
    ClientConfig, ClientConnection, RootCertStore, ServerConfig, ServerConnection, StreamOwned,
};
use serde::{Deserialize, Serialize};

const SERVICE: &str = "_notchisla._tcp.local.";
const TLS_NAME: &str = "notchisla.local";
const PROTOCOL: u8 = 1;
const MAX_HEADER: usize = 16 * 1024;
const MAX_BYTES: u64 = 1024 * 1024 * 1024;
const DISCOVERY_TIME: Duration = Duration::from_secs(3);
const RECEIVE_TIME: Duration = Duration::from_secs(300);
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

pub struct Peer {
    pub name: String,
    pub code: String,
    addresses: Vec<SocketAddrV4>,
    certificate: CertificateDer<'static>,
}

pub enum ReceiveEvent {
    Ready(String),
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

pub fn discover() -> Result<Vec<Peer>> {
    let daemon = Discovery(ServiceDaemon::new()?);
    let events = daemon.0.browse(SERVICE)?;
    let until = Instant::now() + DISCOVERY_TIME;
    let mut peers = BTreeMap::new();
    while let Ok(event) = events.recv_timeout(until.saturating_duration_since(Instant::now())) {
        match event {
            ServiceEvent::ServiceResolved(service) if peers.len() < 64 => {
                let mut cert = Vec::new();
                for key in ["cert0", "cert1", "cert2", "cert3"] {
                    let Some(Some(part)) = service.get_property_val(key) else {
                        break;
                    };
                    cert.extend_from_slice(part);
                }
                let code = certificate_code(&cert);
                let mut addresses: Vec<_> = service
                    .get_addresses_v4()
                    .into_iter()
                    .filter(|ip| (ip.is_private() || ip.is_link_local()) && !ip.is_broadcast())
                    .map(|ip| SocketAddrV4::new(ip, service.get_port()))
                    .collect();
                addresses.sort_unstable();
                addresses.truncate(4);
                if !cert.is_empty() && !addresses.is_empty() && service.get_port() != 0 {
                    let name = clean_label(service.get_property_val_str("name").unwrap_or(""));
                    peers.insert(
                        service.get_fullname().to_owned(),
                        Peer {
                            name,
                            code,
                            addresses,
                            certificate: cert.into(),
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

pub fn send(peer: Peer, payload: Payload) -> Result<()> {
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

    let mut roots = RootCertStore::empty();
    roots.add(peer.certificate)?;
    let config = ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let socket = peer
        .addresses
        .into_iter()
        .find_map(|address| {
            TcpStream::connect_timeout(&address.into(), Duration::from_secs(2)).ok()
        })
        .ok_or("El dispositivo ya no está disponible.")?;
    let connection = ClientConnection::new(Arc::new(config), ServerName::try_from(TLS_NAME)?)?;
    let mut stream = StreamOwned::new(connection, Socket::new(socket, IO_TIME));
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

pub fn receive(
    downloads: PathBuf,
    cancel: Receiver<()>,
    mut event: impl FnMut(ReceiveEvent),
) -> Result<Option<Received>> {
    let listener = TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0))?;
    listener.set_nonblocking(true)?;
    let (certificate, config) = receiver_identity()?;
    let code = certificate_code(&certificate);

    let mut properties = vec![TxtProperty::from(("name", local_name()))];
    for (index, part) in certificate.chunks(200).enumerate() {
        properties.push(TxtProperty::from((format!("cert{index}"), part)));
    }
    let host = format!("notchisla-{code}.local.");
    let service = ServiceInfo::new(
        SERVICE,
        &code,
        &host,
        "",
        listener.local_addr()?.port(),
        properties,
    )?
    .enable_addr_auto();
    let fullname = service.get_fullname().to_owned();
    let daemon = Discovery(ServiceDaemon::new()?);
    daemon.0.register(service)?;
    event(ReceiveEvent::Ready(code));

    let deadline = Instant::now() + RECEIVE_TIME;
    let incoming = loop {
        if Instant::now() >= deadline || cancel.try_recv().is_ok() {
            break None;
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
                if let Ok(offer) = read_offer(&mut stream) {
                    break Some((stream, offer));
                }
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                if cancel.recv_timeout(Duration::from_millis(100)).is_ok() {
                    break None;
                }
            }
            Err(error) => return Err(error.into()),
        }
    };
    if let Ok(status) = daemon.0.unregister(&fullname) {
        let _ = status.recv_timeout(Duration::from_millis(500));
    }
    drop(daemon);
    drop(listener);

    let Some((mut stream, offer)) = incoming else {
        return Ok(None);
    };
    finish_receive(&downloads, &mut stream, offer, &cancel, event)
}

fn receiver_identity() -> Result<(CertificateDer<'static>, Arc<ServerConfig>)> {
    let identity = rcgen::generate_simple_self_signed(vec![TLS_NAME.into()])?;
    let certificate = identity.cert.der().clone();
    let config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(
            vec![certificate.clone()],
            PrivatePkcs8KeyDer::from(identity.signing_key.serialize_der()).into(),
        )?;
    Ok((certificate, Arc::new(config)))
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
        if cancel.try_recv().is_ok() {
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
    let accepted = answer.recv_timeout(APPROVAL_TIME) == Ok(true) && cancel.try_recv().is_err();
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

    #[test]
    fn texto_e_imagen_se_reciben_como_vista_previa_sin_descargas() -> Result<()> {
        for payload in [
            Payload::Text("Hola 👋\r\nsegunda línea".into()),
            Payload::Image(vec![1, 2, 3, 4]),
        ] {
            let directory = tempfile::tempdir()?;
            let downloads = directory.path().to_owned();
            let (certificate, config) = receiver_identity()?;
            let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
            let std::net::SocketAddr::V4(address) = listener.local_addr()? else {
                unreachable!()
            };
            let server = std::thread::spawn(move || -> Result<_> {
                let (socket, _) = listener.accept()?;
                let mut stream =
                    StreamOwned::new(ServerConnection::new(config)?, Socket::new(socket, IO_TIME));
                let offer = read_offer(&mut stream)?;
                let (_stop, cancel) = mpsc::channel();
                finish_receive(&downloads, &mut stream, offer, &cancel, |_| {
                    panic!("una vista previa no pregunta dónde guardar")
                })
            });
            send(
                Peer {
                    name: "Prueba".into(),
                    code: certificate_code(&certificate),
                    addresses: vec![address],
                    certificate,
                },
                payload,
            )?;
            match server.join().unwrap()?.unwrap() {
                Received::Text { text, .. } => assert_eq!(text, "Hola 👋\r\nsegunda línea"),
                Received::Image {
                    directory, path, ..
                } => {
                    assert_eq!(std::fs::read(&path)?, [1, 2, 3, 4]);
                    drop(directory);
                    assert!(!path.exists());
                }
                Received::Files(_) => panic!("no debe guardarse en Descargas"),
            }
            assert_eq!(directory.path().read_dir()?.count(), 0);
        }
        Ok(())
    }

    #[test]
    fn valida_limites_de_vista_previa_y_compatibilidad() -> Result<()> {
        let mut offer: Offer = serde_json::from_str(
            r#"{"version":1,"sender":"Anterior","entries":[{"name":"Mensaje.txt","bytes":4}]}"#,
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
    #[ignore = "requiere una interfaz IPv4 local y tráfico mDNS permitido"]
    fn descubre_y_envia_en_la_red_local() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let source = tempfile::tempdir()?;
        let photo = source.path().join("foto.png");
        let document = source.path().join("archivo.bin");
        std::fs::write(&photo, [0x89, b'P', b'N', b'G'])?;
        std::fs::write(&document, [0, 1, 2, 255])?;
        let downloads = directory.path().to_owned();
        let (stop, cancel) = mpsc::channel();
        let (ready, code) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            receive(downloads, cancel, |event| match event {
                ReceiveEvent::Ready(value) => ready.send(value).unwrap(),
                ReceiveEvent::Offer { decision, .. } => decision.send(true).unwrap(),
            })
        });
        let code = code.recv_timeout(IO_TIME)?;
        let result = (|| -> Result<()> {
            let peer = discover()?
                .into_iter()
                .find(|peer| peer.code == code)
                .ok_or("No se descubrió el receptor de prueba.")?;
            send(peer, Payload::Files(vec![photo, document]))
        })();
        let _ = stop.send(());
        let saved = worker.join().unwrap();
        result?;
        let Received::Files(path) = saved?.unwrap() else {
            panic!("esperaba archivos")
        };
        assert_eq!(
            std::fs::read(path.join("01 - foto.png"))?,
            [0x89, b'P', b'N', b'G']
        );
        assert_eq!(
            std::fs::read(path.join("02 - archivo.bin"))?,
            [0, 1, 2, 255]
        );
        Ok(())
    }

    #[test]
    fn solo_guarda_el_envio_aceptado_y_autentica_el_certificado() -> Result<()> {
        for (trusted, accepted) in [(true, true), (true, false), (false, true)] {
            let directory = tempfile::tempdir()?;
            let downloads = directory.path().to_owned();
            let (certificate, config) = receiver_identity()?;
            let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
            let std::net::SocketAddr::V4(address) = listener.local_addr()? else {
                unreachable!()
            };
            let server = std::thread::spawn(move || -> Result<_> {
                let (socket, _) = listener.accept()?;
                let mut stream =
                    StreamOwned::new(ServerConnection::new(config)?, Socket::new(socket, IO_TIME));
                let offer = read_offer(&mut stream)?;
                let (_stop, cancel) = mpsc::channel();
                finish_receive(&downloads, &mut stream, offer, &cancel, |event| {
                    if let ReceiveEvent::Offer { decision, .. } = event {
                        decision.send(accepted).unwrap();
                    }
                })
            });
            let certificate = if trusted {
                certificate
            } else {
                receiver_identity()?.0
            };
            let peer = Peer {
                name: "Receptor de prueba".into(),
                code: certificate_code(&certificate),
                addresses: vec![address],
                certificate,
            };
            let source = tempfile::tempdir()?;
            let path = source.path().join("prueba.txt");
            std::fs::write(&path, "hola desde la isla 👋")?;
            let result = send(peer, Payload::Files(vec![path]));
            let saved = server.join().unwrap();
            if trusted && accepted {
                result?;
                let Received::Files(path) = saved?.unwrap() else {
                    panic!("esperaba archivos")
                };
                assert_eq!(
                    std::fs::read_to_string(path.join("01 - prueba.txt"))?,
                    "hola desde la isla 👋"
                );
            } else {
                assert!(result.is_err());
                assert!(saved.is_err() || saved?.is_none());
                assert_eq!(directory.path().read_dir()?.count(), 0);
            }
        }
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
