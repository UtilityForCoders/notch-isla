//! Baterías y conexiones de periféricos. Las consultas se hacen en un hilo separado
//! cada 5 s con avisos de conexión, o cada 30 s si están desactivados.
//! Solo se envían consultas de estado a receptores con protocolo identificado.

mod hidpp;
mod standard;

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use hidapi::{HidApi, HidDevice};
use windows::core::{Interface, GUID, HSTRING};
use windows::Devices::Bluetooth::GenericAttributeProfile::GattCommunicationStatus;
use windows::Devices::Bluetooth::{
    BluetoothCacheMode, BluetoothConnectionStatus, BluetoothDevice, BluetoothLEDevice,
};
use windows::Devices::Enumeration::{DeviceInformation, DeviceInformationKind};
use windows::Foundation::IPropertyValue;
use windows::Gaming::Input::RawGameController;
use windows::Storage::Streams::DataReader;
use windows::System::Power::BatteryStatus;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::WinRT::{RoInitialize, RoUninitialize, RO_INIT_MULTITHREADED};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};
use windows_future::{AsyncOperationCompletedHandler, IAsyncOperation};

pub const WM_BATTERY: u32 = WM_APP + 11;
pub const LOW_PERCENT: u8 = 20;
const INTERVAL: Duration = Duration::from_secs(30);
const CONNECTION_INTERVAL: Duration = Duration::from_secs(5);
const DEVICE_ABSENCE_GRACE: Duration = Duration::from_secs(5);
const RECEIVER_LABEL_GRACE: Duration = Duration::from_secs(5);
const RECEIVER_READING_GRACE: Duration = Duration::from_secs(15);
const BT_BATTERY: &str = "{104EA319-6EE2-4701-BD47-8DDBF425BBE5} 2";
const BT_ENDPOINT_CONTAINER: &str = "System.Devices.Aep.ContainerId";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Percent(u8),
    Low,
    Medium,
    Full,
}

impl Level {
    pub fn label(self) -> String {
        match self {
            Self::Percent(p) => format!("{p} %"),
            Self::Low => "Baja (aprox.)".into(),
            Self::Medium => "Media (aprox.)".into(),
            Self::Full => "Alta (aprox.)".into(),
        }
    }

    fn low(self) -> bool {
        matches!(self, Self::Percent(p) if p <= LOW_PERCENT) || self == Self::Low
    }

    fn recharged(self) -> bool {
        matches!(self, Self::Percent(p) if p >= 25) || matches!(self, Self::Medium | Self::Full)
    }
}

#[derive(Clone, Debug)]
pub struct Device {
    pub id: String,
    pub name: String,
    pub kind: DeviceKind,
    pub connection: &'static str,
    pub level: Option<Level>,
    pub charging: bool,
    /// Valor retenido de un sondeo anterior o enlace sin confirmar.
    pub stale: bool,
    /// El porcentaje viene de la propiedad PnP de Windows, no de una lectura GATT.
    pub windows_cached: bool,
    /// Un receptor presente no implica que su periférico esté encendido.
    pub detail: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DeviceKind {
    Mouse,
    Keyboard,
    Headphones,
    Speaker,
    Gamepad,
    #[default]
    Unknown,
}

impl DeviceKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Mouse => "Mouse",
            Self::Keyboard => "Teclado",
            Self::Headphones => "Audífonos",
            Self::Speaker => "Parlantes",
            Self::Gamepad => "Mando",
            Self::Unknown => "Dispositivo",
        }
    }
}

// Bluetooth SIG Assigned Numbers: Appearance y Class of Device.
fn kind_from_appearance(raw: u16) -> DeviceKind {
    match raw {
        0x03c1 => DeviceKind::Keyboard,
        0x03c2 => DeviceKind::Mouse,
        0x03c3 | 0x03c4 => DeviceKind::Gamepad,
        0x0940..=0x0944 => DeviceKind::Headphones,
        0x0841..=0x0845 => DeviceKind::Speaker,
        _ => DeviceKind::Unknown,
    }
}

fn kind_from_class(raw: u32) -> DeviceKind {
    let major = (raw >> 8) & 0x1f;
    let minor = (raw >> 2) & 0x3f;
    match major {
        4 => match minor {
            1 | 6 => DeviceKind::Headphones,
            5 => DeviceKind::Speaker,
            _ => DeviceKind::Unknown,
        },
        5 => match minor & 0x30 {
            0x10 => DeviceKind::Keyboard,
            0x20 => DeviceKind::Mouse,
            0 if matches!(minor & 0x0f, 1 | 2) => DeviceKind::Gamepad,
            _ => DeviceKind::Unknown, // Un combo teclado/mouse no es un tipo único.
        },
        _ => DeviceKind::Unknown,
    }
}

fn kind_from_hid_usages(usages: impl Iterator<Item = (u16, u16)>) -> DeviceKind {
    let mut found = None;
    for (page, usage) in usages {
        let kind = match (page, usage) {
            (1, 2) => DeviceKind::Mouse,
            (1, 6) => DeviceKind::Keyboard,
            (1, 4 | 5) => DeviceKind::Gamepad,
            _ => continue,
        };
        if found.is_some_and(|previous| previous != kind) {
            return DeviceKind::Unknown;
        }
        found = Some(kind);
    }
    found.unwrap_or_default()
}

impl Device {
    fn source(&self) -> Source {
        if self.id.starts_with("bt:") {
            Source::Bluetooth
        } else if self.id.starts_with("gamepad:") {
            Source::Controller
        } else {
            Source::Receiver
        }
    }

    pub fn active_link(&self) -> bool {
        !self.receiver_without_link()
    }
    /// El USB sigue conectado aunque el enlace con el periférico no responda.
    /// No constituye una segunda conexión activa ni una identidad física común.
    pub fn receiver_without_link(&self) -> bool {
        self.connection == "USB / 2.4 GHz" && (self.stale || self.level.is_none())
    }

    pub fn has_low_battery(&self) -> bool {
        !self.stale && !self.charging && self.level.is_some_and(Level::low)
    }

    pub fn status(&self) -> String {
        let mut text = self
            .level
            .map_or_else(|| "Batería no disponible".into(), Level::label);
        if self.stale {
            text.push_str(" · Última lectura");
        } else if self.charging {
            text.push_str(" · Cargando");
        } else if self.has_low_battery() {
            text.push_str(" · Baja");
        }
        text
    }

    fn update_bluetooth_level(&mut self, direct: Option<Level>, cached: Option<Level>) {
        if let Some(level) = direct {
            self.level = Some(level);
            self.stale = false;
            self.windows_cached = false;
            self.detail.clear();
        } else if let Some(level) = cached.filter(|&level| {
            level != Level::Percent(0) && (self.level.is_none() || self.windows_cached)
        }) {
            self.level = Some(level);
            self.stale = false;
            self.windows_cached = true;
            self.detail.clear();
        }
    }
}

#[derive(Clone, Default)]
pub struct Snapshot {
    pub devices: Vec<Device>,
    pub updated: Option<Instant>,
    pub scanning: bool,
    pub errors: Vec<String>,
    pub unavailable: HashSet<Source>,
    checking_links: HashMap<String, Instant>,
}

impl Snapshot {
    pub fn confirmed_link(&self, device: &Device) -> bool {
        device.active_link()
            && !self.checking_links.contains_key(&device.id)
            && !self.unavailable.contains(&device.source())
    }

    pub fn show_inactive_receiver(&self, device: &Device) -> bool {
        device.receiver_without_link() && !self.checking_links.contains_key(&device.id)
    }

    /// Mantiene la etiqueta estable durante un fallo breve de lectura. La muestra
    /// sigue marcada como antigua internamente y no puede generar avisos.
    pub fn show_previous_reading(&self, device: &Device) -> bool {
        device.stale
            && device.level.is_some()
            && !(device.source() == Source::Receiver
                && self
                    .checking_links
                    .get(&device.id)
                    .zip(self.updated)
                    .is_some_and(|(since, now)| now.duration_since(*since) < RECEIVER_LABEL_GRACE))
    }
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub enum Source {
    Bluetooth,
    Controller,
    Receiver,
}

enum Request {
    Refresh,
    Stop,
}

pub struct Monitor {
    shared: Arc<Mutex<Snapshot>>,
    sender: mpsc::SyncSender<Request>,
    connection_notices: Arc<AtomicBool>,
}

impl Monitor {
    pub fn start(hwnd: HWND, connections: bool) -> Self {
        let shared = Arc::new(Mutex::new(Snapshot {
            scanning: true,
            ..Snapshot::default()
        }));
        let (sender, receiver) = mpsc::sync_channel(1);
        let state = shared.clone();
        let connection_notices = Arc::new(AtomicBool::new(connections));
        let enabled = connection_notices.clone();
        let hwnd = hwnd.0 as isize;
        std::thread::spawn(move || {
            let initialized = unsafe { RoInitialize(RO_INIT_MULTITHREADED) }.is_ok();
            // WGI completa su lista inicial de forma asíncrona.
            let _ = RawGameController::RawGameControllers();
            std::thread::sleep(Duration::from_millis(500));
            let mut stability = LinkStability::default();
            loop {
                let mut snapshot = scan();
                let mut previous = state.lock().unwrap_or_else(|e| e.into_inner());
                stability.update(&mut snapshot, &previous);
                *previous = snapshot;
                drop(previous);
                unsafe {
                    let _ = PostMessageW(Some(HWND(hwnd as _)), WM_BATTERY, WPARAM(0), LPARAM(0));
                }
                let interval = if enabled.load(Ordering::Relaxed) {
                    CONNECTION_INTERVAL
                } else {
                    INTERVAL
                };
                match receiver.recv_timeout(interval) {
                    Ok(Request::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    _ => {}
                }
            }
            if initialized {
                unsafe { RoUninitialize() };
            }
        });
        Self {
            shared,
            sender,
            connection_notices,
        }
    }

    pub fn snapshot(&self) -> Snapshot {
        self.shared
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn refresh(&self) {
        let mut state = self.shared.lock().unwrap_or_else(|e| e.into_inner());
        if !state.scanning && self.sender.try_send(Request::Refresh).is_ok() {
            state.scanning = true;
        }
    }

    pub fn set_connection_notices(&self, enabled: bool) {
        self.connection_notices.store(enabled, Ordering::Relaxed);
        self.refresh();
    }
}

/// Conserva filas durante una ausencia breve y distingue la presencia del
/// receptor de la capacidad de leer el periférico en este sondeo.
#[derive(Default)]
struct LinkStability {
    missing_since: HashMap<String, Instant>,
    // Solo receptores que ya respondieron. None indica que aún no falló la lectura.
    receiver_silent_since: HashMap<String, Option<Instant>>,
}

impl LinkStability {
    fn update(&mut self, current: &mut Snapshot, previous: &Snapshot) {
        let Some(now) = current.updated else { return };
        for device in &current.devices {
            self.missing_since.remove(&device.id);
        }
        for old in &previous.devices {
            if current.devices.iter().any(|device| device.id == old.id) {
                continue;
            }
            if current.unavailable.contains(&old.source()) {
                self.missing_since.remove(&old.id);
            } else {
                let since = self.missing_since.entry(old.id.clone()).or_insert(now);
                if now.duration_since(*since) >= DEVICE_ABSENCE_GRACE {
                    self.missing_since.remove(&old.id);
                    self.receiver_silent_since.remove(&old.id);
                    continue;
                }
            }
            let mut retained = old.clone();
            retained.stale = true;
            retained.charging = false;
            if old.source() != Source::Receiver {
                current.checking_links.insert(old.id.clone(), now);
            }
            current.devices.push(retained);
        }
        for device in &current.devices {
            if device.source() != Source::Receiver {
                continue;
            }
            if device.active_link() && !current.unavailable.contains(&Source::Receiver) {
                self.receiver_silent_since.insert(device.id.clone(), None);
            } else if let Some(silent) = self.receiver_silent_since.get_mut(&device.id) {
                let since = *silent.get_or_insert(now);
                if now.duration_since(since) < RECEIVER_READING_GRACE {
                    current.checking_links.insert(device.id.clone(), since);
                }
            }
        }
        preserve_last_readings(current, previous);
        current
            .devices
            .sort_by_key(|device| device.name.to_lowercase());
    }
}

/// Observa transiciones a un enlace confirmado. El primer sondeo de cada fuente
/// es la referencia inicial, no una avalancha de avisos al abrir la aplicación.
#[derive(Default)]
pub struct Connections {
    initialized: HashSet<Source>,
    devices: HashMap<String, Source>,
    updated: Option<Instant>,
    enabled: Option<bool>,
    suppress_on_enable: HashSet<Source>,
}

impl Connections {
    pub fn update(&mut self, snapshot: &Snapshot, enabled: bool) -> Vec<Device> {
        if self.enabled == Some(false) && enabled {
            self.suppress_on_enable.extend([
                Source::Bluetooth,
                Source::Controller,
                Source::Receiver,
            ]);
        }
        self.enabled = Some(enabled);
        let Some(now) = snapshot.updated else {
            return Vec::new();
        };
        if self.updated.is_some_and(|previous| previous >= now) {
            return Vec::new();
        }
        self.updated = Some(now);
        let mut notices = Vec::new();
        for source in [Source::Bluetooth, Source::Controller, Source::Receiver] {
            if snapshot.unavailable.contains(&source) {
                continue;
            }
            let first = self.initialized.insert(source);
            let suppressed = self.suppress_on_enable.remove(&source);
            let baseline = first || suppressed;
            // Rearmar solo cuando desaparezca el dispositivo de la lista estabilizada.
            // Un dongle presente sin datos no distingue suspensión de apagado.
            self.devices.retain(|id, device_source| {
                *device_source != source || snapshot.devices.iter().any(|device| device.id == *id)
            });
            for device in snapshot
                .devices
                .iter()
                .filter(|d| d.source() == source && snapshot.confirmed_link(d))
            {
                let new = self.devices.insert(device.id.clone(), source).is_none();
                if !baseline && new && enabled {
                    notices.push(device.clone());
                }
            }
        }
        notices
    }
}

impl Drop for Monitor {
    fn drop(&mut self) {
        // No se espera al controlador USB desde el hilo de la ventana.
        let _ = self.sender.try_send(Request::Stop);
    }
}

/// Se conserva el aviso durante desconexiones y lecturas fallidas. Solo una recarga
/// comprobada lo rearma; la histéresis evita avisos por oscilaciones alrededor del umbral.
#[derive(Default)]
pub struct Alerts {
    warned: HashSet<String>,
}

impl Alerts {
    pub fn retry_unshown(&mut self, id: &str) {
        self.warned.remove(id);
    }

    pub fn update(&mut self, devices: &[Device], enabled: bool) -> Vec<Device> {
        let mut alerts = Vec::new();
        for device in devices {
            if device.stale || device.receiver_without_link() {
                continue;
            }
            if device.charging || device.level.is_some_and(Level::recharged) {
                self.warned.remove(&device.id);
            } else if enabled && device.has_low_battery() && self.warned.insert(device.id.clone()) {
                alerts.push(device.clone());
            }
        }
        alerts
    }
}

fn preserve_last_readings(current: &mut Snapshot, previous: &Snapshot) {
    for device in &mut current.devices {
        let Some(old) = previous.devices.iter().find(|old| old.id == device.id) else {
            continue;
        };
        if device.kind == DeviceKind::Unknown {
            device.kind = old.kind;
        }
        if old.level.is_some()
            && (device.level.is_none() || (device.windows_cached && !old.windows_cached))
        {
            device.level = old.level;
            device.windows_cached = old.windows_cached;
            device.stale = true;
        }
    }
}

fn scan() -> Snapshot {
    scan_with_diagnostics(false)
}

fn scan_with_diagnostics(trace: bool) -> Snapshot {
    let mut snapshot = Snapshot::default();
    let at = Instant::now();
    if let Err(e) = bluetooth(&mut snapshot.devices, trace) {
        snapshot.unavailable.insert(Source::Bluetooth);
        snapshot.errors.push(format!("Bluetooth: {e}"));
    }
    if trace {
        println!("Bluetooth: {:.2} s", at.elapsed().as_secs_f32());
    }
    if let Err(e) = controllers(&mut snapshot.devices) {
        snapshot.unavailable.insert(Source::Controller);
        snapshot.errors.push(format!("Mandos: {e}"));
    }
    if trace {
        println!("Mandos: {:.2} s", at.elapsed().as_secs_f32());
    }
    if let Err(e) = receivers(&mut snapshot.devices, trace) {
        snapshot.unavailable.insert(Source::Receiver);
        snapshot.errors.push(format!("Receptores USB: {e}"));
    }
    if trace {
        println!("Receptores: {:.2} s", at.elapsed().as_secs_f32());
    }
    snapshot.devices.sort_by_key(|d| d.name.to_lowercase());
    snapshot.updated = Some(Instant::now());
    snapshot
}

fn properties(names: &[&str]) -> windows_collections::IIterable<HSTRING> {
    names
        .iter()
        .map(|s| HSTRING::from(*s))
        .collect::<Vec<_>>()
        .into()
}

fn property(device: &DeviceInformation, name: &str) -> Option<IPropertyValue> {
    device
        .Properties()
        .ok()?
        .Lookup(&HSTRING::from(name))
        .ok()?
        .cast()
        .ok()
}

/// Un dispositivo Bluetooth apagado o su controlador no debe detener el monitor.
fn finish<T: windows::core::RuntimeType + 'static>(
    operation: IAsyncOperation<T>,
) -> windows::core::Result<T> {
    let (sender, receiver) = mpsc::sync_channel(1);
    operation.SetCompleted(&AsyncOperationCompletedHandler::new(move |_, _| {
        let _ = sender.try_send(());
        Ok(())
    }))?;
    if receiver.recv_timeout(Duration::from_secs(3)).is_err() {
        let _ = operation.Cancel();
        return Err(windows::core::Error::from(windows::core::HRESULT(
            0x800705b4u32 as i32,
        )));
    }
    operation.GetResults()
}

fn property_percent(device: &DeviceInformation) -> Option<Level> {
    let value = property(device, BT_BATTERY)?;
    let p = value
        .GetUInt8()
        .ok()
        .map(u32::from)
        .or_else(|| value.GetUInt32().ok())
        .or_else(|| value.GetInt32().ok().and_then(|v| u32::try_from(v).ok()))?;
    (p <= 100).then_some(Level::Percent(p as u8))
}

fn bluetooth(out: &mut Vec<Device>, trace: bool) -> windows::core::Result<()> {
    // La batería que conoce Windows está en el nodo PnP, no siempre en el AEP.
    let nodes = finish(
        DeviceInformation::FindAllAsyncWithKindAqsFilterAndAdditionalProperties(
            &HSTRING::from("System.Devices.DeviceInstanceId:~<\"BTH\" AND System.Devices.Present:=System.StructuredQueryType.Boolean#True"),
            &properties(&[BT_BATTERY, "System.Devices.ContainerId"]),
            DeviceInformationKind::Device,
        )?,
    )?;
    if trace {
        println!("Bluetooth: nodos PnP enumerados");
    }
    let mut batteries = HashMap::new();
    for node in &nodes {
        if let (Some(level), Some(container)) = (
            property_percent(&node),
            property(&node, "System.Devices.ContainerId").and_then(|p| p.GetGuid().ok()),
        ) {
            batteries.insert(container, level);
        }
    }
    // El selector de conexión evita el descubrimiento de radio. El emparejamiento
    // pertenece al endpoint Bluetooth (AEP), no a la interfaz de dispositivo.
    for le in [false, true] {
        let selector = if le {
            BluetoothLEDevice::GetDeviceSelectorFromConnectionStatus(
                BluetoothConnectionStatus::Connected,
            )?
        } else {
            BluetoothDevice::GetDeviceSelectorFromConnectionStatus(
                BluetoothConnectionStatus::Connected,
            )?
        };
        let devices = finish(
            DeviceInformation::FindAllAsyncWithKindAqsFilterAndAdditionalProperties(
                &selector,
                &properties(&[BT_ENDPOINT_CONTAINER]),
                DeviceInformationKind::AssociationEndpoint,
            )?,
        )?;
        if trace {
            println!("Bluetooth: endpoints LE={le}: {}", devices.Size()?);
        }
        for info in &devices {
            // El selector de conexión no exige emparejamiento. Compruébalo antes
            // de abrir el dispositivo o consultar su batería.
            if !info
                .Pairing()
                .and_then(|pairing| pairing.IsPaired())
                .unwrap_or(false)
            {
                if trace {
                    println!("Bluetooth: candidato omitido sin emparejamiento confirmado");
                }
                continue;
            }
            let id = info.Id()?;
            let (connected, native_name, address, kind, direct_level) = if le {
                BluetoothLEDevice::FromIdAsync(&id)
                    .and_then(finish)
                    .map(|d| {
                        let connected = d.ConnectionStatus().ok();
                        let result = (
                            connected,
                            d.Name().ok(),
                            d.BluetoothAddress().ok(),
                            d.Appearance()
                                .and_then(|a| a.RawValue())
                                .map(kind_from_appearance)
                                .unwrap_or_default(),
                            if connected == Some(BluetoothConnectionStatus::Connected) {
                                ble_battery(&d)
                            } else {
                                None
                            },
                        );
                        let _ = d.Close();
                        result
                    })
                    .unwrap_or_default()
            } else {
                BluetoothDevice::FromIdAsync(&id)
                    .and_then(finish)
                    .map(|d| {
                        let result = (
                            d.ConnectionStatus().ok(),
                            d.Name().ok(),
                            d.BluetoothAddress().ok(),
                            d.ClassOfDevice()
                                .and_then(|c| c.RawValue())
                                .map(kind_from_class)
                                .unwrap_or_default(),
                            None,
                        );
                        let _ = d.Close();
                        result
                    })
                    .unwrap_or_default()
            };
            if connected != Some(BluetoothConnectionStatus::Connected) {
                continue;
            }
            let address = address
                .map(|a| format!("{a:012X}"))
                .unwrap_or_else(|| id.to_string());
            let cached_level = property(&info, BT_ENDPOINT_CONTAINER)
                .and_then(|p| p.GetGuid().ok())
                .and_then(|key| batteries.get(&key).copied())
                .or_else(|| {
                    (&nodes).into_iter().find_map(|node| {
                        node.Id()
                            .ok()
                            .filter(|s| s.to_string().to_uppercase().contains(&address))
                            .and_then(|_| property_percent(&node))
                    })
                });
            let key = format!("bt:{address}");
            if let Some(existing) = out.iter_mut().find(|d| d.id == key) {
                // Un dispositivo dual puede informar batería solo en su interfaz LE.
                if existing.kind == DeviceKind::Unknown {
                    existing.kind = kind;
                }
                existing.update_bluetooth_level(direct_level, cached_level);
                continue;
            }
            let mut device = Device {
                id: key,
                kind,
                name: native_name
                    .filter(|n| !n.is_empty())
                    .or_else(|| info.Name().ok().filter(|n| !n.is_empty()))
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| format!("Bluetooth {address}")),
                connection: "Bluetooth",
                level: None,
                charging: false,
                stale: false,
                windows_cached: false,
                detail: "Sin lectura de batería por Bluetooth".into(),
            };
            device.update_bluetooth_level(direct_level, cached_level);
            out.push(device);
        }
    }
    Ok(())
}
fn ble_battery(device: &BluetoothLEDevice) -> Option<Level> {
    let services = device
        .GetGattServicesForUuidAsync(GUID::from_u128(0x0000180f_0000_1000_8000_00805f9b34fb))
        .and_then(finish)
        .ok()?;
    if services.Status().ok()? != GattCommunicationStatus::Success {
        return None;
    }
    for service in &services.Services().ok()? {
        let value = (|| {
            let characteristics = service
                .GetCharacteristicsForUuidAsync(GUID::from_u128(
                    0x00002a19_0000_1000_8000_00805f9b34fb,
                ))
                .and_then(finish)
                .ok()?;
            if characteristics.Status().ok()? != GattCommunicationStatus::Success {
                return None;
            }
            for characteristic in &characteristics.Characteristics().ok()? {
                let reading = characteristic
                    .ReadValueWithCacheModeAsync(BluetoothCacheMode::Uncached)
                    .and_then(finish)
                    .ok()?;
                if reading.Status().ok()? != GattCommunicationStatus::Success {
                    continue;
                }
                let buffer = reading.Value().ok()?;
                if buffer.Length().ok()? != 1 {
                    continue;
                }
                let percent = DataReader::FromBuffer(&buffer).ok()?.ReadByte().ok()?;
                if percent <= 100 {
                    return Some(Level::Percent(percent));
                }
            }
            None
        })();
        let _ = service.Close();
        if value.is_some() {
            return value;
        }
    }
    None
}

fn capacity_level(remaining: i32, full: i32) -> Option<Level> {
    if full <= 0 || remaining < 0 || remaining > full {
        return None;
    }
    // Algunos controladores devuelven los cuatro niveles de XInput como 0..3.
    if full == 3 {
        return Some(match remaining {
            0 | 1 => Level::Low,
            2 => Level::Medium,
            _ => Level::Full,
        });
    }
    Some(Level::Percent(
        ((i64::from(remaining) * 100) / i64::from(full)) as u8,
    ))
}

fn controllers(out: &mut Vec<Device>) -> windows::core::Result<()> {
    let controllers = RawGameController::RawGameControllers()?;
    let mut names = HashMap::new();
    for controller in &controllers {
        *names
            .entry(controller.DisplayName()?.to_string().to_lowercase())
            .or_insert(0) += 1;
    }
    for controller in &controllers {
        let name = controller.DisplayName()?.to_string();
        let id = controller.NonRoamableId()?.to_string();
        let report = controller.TryGetBatteryReport().ok();
        let status = report.as_ref().and_then(|r| r.Status().ok());
        let level = report.as_ref().and_then(|r| {
            capacity_level(
                r.RemainingCapacityInMilliwattHours().ok()?.Value().ok()?,
                r.FullChargeCapacityInMilliwattHours().ok()?.Value().ok()?,
            )
        });
        // Solo fusionar nombres inequívocos; dos mandos iguales deben seguir separados.
        let unique_bt = names.get(&name.to_lowercase()) == Some(&1)
            && out
                .iter()
                .filter(|d| d.connection == "Bluetooth" && d.name.eq_ignore_ascii_case(&name))
                .count()
                == 1;
        if let Some(existing) = out.iter_mut().find(|d| {
            unique_bt && d.connection == "Bluetooth" && d.name.eq_ignore_ascii_case(&name)
        }) {
            if level.is_some() {
                existing.level = level;
                existing.stale = false;
                existing.windows_cached = false;
                existing.detail.clear();
            }
            existing.charging = status == Some(BatteryStatus::Charging);
            existing.kind = DeviceKind::Gamepad;
            continue;
        }
        out.push(Device {
            id: format!("gamepad:{id}"),
            kind: DeviceKind::Gamepad,
            name,
            connection: if controller.IsWireless().unwrap_or(false) {
                "Inalámbrico"
            } else {
                "USB / receptor"
            },
            level,
            charging: status == Some(BatteryStatus::Charging),
            stale: false,
            windows_cached: false,
            detail: if level.is_none() {
                "El mando o su receptor no informa la batería".into()
            } else {
                String::new()
            },
        });
    }
    Ok(())
}

/// Respuesta de estado AJAZZ 3151:5007/402D, con report id conservado por Windows.
fn parse_ajazz(data: &[u8]) -> Option<Level> {
    if data.len() < 8
        || data[0] != 5
        || data[1..3] != [0, 0]
        || data[3] > 100
        || data[3..8].iter().all(|b| *b == 0)
    {
        return None;
    }
    Some(Level::Percent(data[3]))
}

fn ajazz(device: &HidDevice, trace: bool) -> Option<Level> {
    for size in [65, 67] {
        let mut request = vec![0; size];
        request[1] = 0xf7;
        let sent = device.send_feature_report(&request);
        if trace {
            println!("AJAZZ consulta F7 ({size} bytes): {sent:?}");
        }
        if sent.is_err() {
            continue;
        }
        std::thread::sleep(Duration::from_millis(30));
        let mut response = [0u8; 65];
        response[0] = 5;
        let read = device.get_feature_report(&mut response);
        if trace {
            match &read {
                Ok(count) => println!("AJAZZ respuesta: {:02x?}", &response[..(*count).min(16)]),
                Err(e) => println!("AJAZZ respuesta: {e:?}"),
            }
        }
        if let Ok(count) = read {
            if let Some(level) = parse_ajazz(&response[..count]) {
                return Some(level);
            }
        }
    }
    None
}

fn compx_query(device: &HidDevice, query: u8) -> Option<[u8; 32]> {
    // hidapi exige el report id 0 delante del informe sin numerar de 32 bytes.
    let mut request = [0u8; 33];
    request[1] = query;
    device.write(&request).ok()?;
    let deadline = Instant::now() + Duration::from_millis(400);
    while Instant::now() < deadline {
        let mut response = [0u8; 32];
        let remaining = deadline
            .saturating_duration_since(Instant::now())
            .as_millis()
            .max(1) as i32;
        let count = device.read_timeout(&mut response, remaining).ok()?;
        if count == 32 && valid_compx(&response, query) {
            return Some(response);
        }
    }
    None
}

fn valid_compx(response: &[u8], query: u8) -> bool {
    response.len() == 32
        && response[0] == query
        && response[1] == 0
        && response[2] == 1
        && response[3] != 0
        && response[4..31]
            .iter()
            .fold(0u8, |sum, b| sum.wrapping_add(*b))
            == response[31]
}

fn compx_battery(device: &HidDevice) -> Option<Level> {
    // La identidad puede ser una copia vieja del receptor. Exigir primero respuesta
    // a una consulta que debe llegar al mouse (frecuencia, sin modificarla).
    compx_query(device, 0x12)?;
    let info = compx_query(device, 0x10)?;
    let model = &info[4..8];
    if !matches!(model, b"M620" | b"P630" | b"M630") || info[13] > 100 {
        return None;
    }
    Some(Level::Percent(info[13]))
}

fn parse_mchose(data: &[u8]) -> Option<(Level, bool)> {
    if data.len() < 13 || !matches!(data[0], 0x11 | 0x12) || data[1] != !0x06u8 {
        return None;
    }
    let body: Vec<u8> = data[2..].iter().map(|b| !b).collect();
    let vendor = u16::from_le_bytes([body[0], body[1]]);
    // Bit 3: el mouse está enlazado. Una estructura válida con el enlace
    // apagado puede contener el último porcentaje almacenado por el receptor.
    if !matches!(vendor, 0x5253 | 0x3837) || body[8] & 0x08 == 0 || body[9] > 100 || body[10] > 1 {
        return None;
    }
    Some((Level::Percent(body[9]), body[10] == 1))
}

fn mchose(device: &HidDevice, trace: bool) -> Option<(Level, bool)> {
    // El receptor a veces entrega un marco vacío mientras espera al mouse.
    // Dar tiempo a cada solicitud y repetir ambas variantes una sola vez.
    for (report, size) in [(0x11, 21), (0x12, 65), (0x11, 21), (0x12, 65)] {
        let mut request = vec![0xff; size];
        request[0] = report;
        request[1] = !0x06u8;
        let sent = device.send_feature_report(&request);
        if trace {
            println!("MCHOSE consulta {report:02x}: {sent:?}");
        }
        if sent.is_err() {
            continue;
        }
        for delay_ms in [20, 15, 15, 15] {
            std::thread::sleep(Duration::from_millis(delay_ms));
            let mut response = vec![0; size];
            response[0] = report;
            if let Ok(count) = device.get_feature_report(&mut response) {
                if trace {
                    println!("MCHOSE respuesta: {:02x?}", &response[..count.min(16)]);
                }
                if let Some(value) = parse_mchose(&response[..count]) {
                    return Some(value);
                }
            }
        }
    }
    None
}

fn receivers(out: &mut Vec<Device>, trace: bool) -> hidapi::HidResult<()> {
    let api = HidApi::new()?;
    let mut seen = HashSet::new();
    let mut handled = HashSet::new();
    for info in api.device_list() {
        let vendor = info.vendor_id();
        let product = info.product_id();
        let name = info.product_string().unwrap_or("Periférico USB");
        let ajazz_family = known_ajazz_apex(vendor, product, usages(&api, vendor, product));
        let known_ajazz = ajazz_family && info.usage_page() == 0xffff && info.usage() == 2;
        let known_compx = matches!(vendor, 0x248a | 0x249a)
            && matches!(product, 0x5c2f | 0x5b2f)
            && info.interface_number() == 2;
        let known_mchose = ((vendor == 0x5253 && matches!(product, 0x1020 | 0x1021))
            || (matches!(vendor, 0x5253 | 0x3837) && name.to_lowercase().contains("k7")))
            && info.usage_page() == 0xff01;
        // Otras variantes se muestran sin enviarles comandos no verificados.
        let candidate = known_ajazz
            || known_compx
            || known_mchose
            || (info.usage_page() >= 0xff00
                && (name.to_lowercase().contains("ajazz")
                    || name.to_lowercase().contains("mchose")));
        if !candidate {
            continue;
        }
        // La colección que permite leer tiene prioridad sobre las otras del mismo receptor.
        if !known_ajazz
            && !known_compx
            && !known_mchose
            && api.device_list().any(|d| {
                d.vendor_id() == vendor
                    && d.product_id() == product
                    && ((ajazz_family && d.usage_page() == 0xffff && d.usage() == 2)
                        || (known_mchose_family(vendor, product, name) && d.usage_page() == 0xff01))
            })
        {
            continue;
        }
        let path = info.path().to_string_lossy();
        // Las colecciones comparten el prefijo de ruta; conservar receptores idénticos separados.
        let id = format!("hid:{}", path.to_lowercase());
        if !seen.insert(id.clone()) {
            continue;
        }
        let value = info.open_device(&api).ok().and_then(|device| {
            if known_ajazz {
                ajazz(&device, trace).map(|l| (l, false))
            } else if known_compx {
                compx_battery(&device).map(|l| (l, false))
            } else if known_mchose {
                mchose(&device, trace)
            } else {
                None
            }
        });
        out.push(Device {
            id,
            name: name.to_string(),
            // Estos perfiles implementados pertenecen a familias de mouse.
            // Sus colecciones de teclado para macros no cambian el tipo físico.
            kind: if known_ajazz || known_compx || known_mchose {
                DeviceKind::Mouse
            } else {
                kind_from_hid_usages(usages(&api, vendor, product))
            },
            connection: "USB / 2.4 GHz",
            level: value.map(|v| v.0),
            charging: value.is_some_and(|v| v.1),
            stale: false,
            windows_cached: false,
            detail: if value.is_some() {
                String::new()
            } else if known_ajazz || known_compx || known_mchose {
                "Sin lectura: apagado, dormido u ocupado".into()
            } else {
                "Protocolo de batería aún no compatible".into()
            },
        });
        handled.insert((vendor, product));
    }
    handled.extend(hidpp::scan(&api, out, trace));
    standard_battery(&api, out, &handled, trace);
    Ok(())
}

fn usages(api: &HidApi, vendor: u16, product: u16) -> impl Iterator<Item = (u16, u16)> + '_ {
    api.device_list()
        .filter(move |d| d.vendor_id() == vendor && d.product_id() == product)
        .map(|d| (d.usage_page(), d.usage()))
}

/// Cualquier otro periférico USB que declare Battery Strength en su descriptor HID.
/// Solo se piden los informes declarados; ningún comando de fabricante.
fn standard_battery(
    api: &HidApi,
    out: &mut Vec<Device>,
    handled: &HashSet<(u16, u16)>,
    trace: bool,
) {
    let mut seen = HashSet::new();
    for info in api.device_list() {
        let (vendor, product) = (info.vendor_id(), info.product_id());
        let path = info.path().to_string_lossy().to_string();
        if handled.contains(&(vendor, product)) || hidpp::bluetooth_path(&path) {
            continue;
        }
        let key = hidpp::interface_key(&path);
        if seen.contains(&key) {
            continue;
        }
        let level = match standard::battery_strength(&path) {
            standard::Strength::Undeclared => continue,
            standard::Strength::NoReading => None,
            standard::Strength::Percent(p) => Some(Level::Percent(p)),
        };
        seen.insert(key.clone());
        if trace {
            println!("HID estándar {vendor:04x}:{product:04x}: {level:?}");
        }
        out.push(Device {
            id: format!("hidstd:{key}"),
            name: info
                .product_string()
                .unwrap_or("Periférico USB")
                .to_string(),
            kind: kind_from_hid_usages(usages(api, vendor, product)),
            connection: "USB / 2.4 GHz",
            level,
            charging: false,
            stale: false,
            windows_cached: false,
            detail: if level.is_some() {
                String::new()
            } else {
                "Sin lectura: apagado, dormido u ocupado".into()
            },
        });
    }
}

/// Receptores AJAZZ con la consulta F7 en la colección `FFFF:0002`. Además de
/// los modelos probados (`5007`, `402D`), se acepta otro producto del mismo
/// fabricante solo si expone un mouse; los teclados con esa colección no reciben
/// la consulta. La respuesta se valida antes de aceptarla.
fn known_ajazz_apex(
    vendor: u16,
    product: u16,
    mut usages: impl Iterator<Item = (u16, u16)>,
) -> bool {
    vendor == 0x3151 && (matches!(product, 0x5007 | 0x402d) || usages.any(|u| u == (1, 2)))
}

fn known_mchose_family(vendor: u16, product: u16, name: &str) -> bool {
    (vendor == 0x5253 && matches!(product, 0x1020 | 0x1021))
        || (matches!(vendor, 0x5253 | 0x3837) && name.to_lowercase().contains("k7"))
}

/// Diagnóstico explícito para validar hardware sin abrir otra instancia de la isla.
pub fn diagnostics() {
    let initialized = unsafe { RoInitialize(RO_INIT_MULTITHREADED) }.is_ok();
    // WGI termina de enumerar los mandos de forma asíncrona.
    let _ = RawGameController::RawGameControllers();
    std::thread::sleep(Duration::from_millis(500));
    let snapshot = scan_with_diagnostics(true);
    for device in &snapshot.devices {
        println!(
            "{} | {} | {} | {}{} | {}",
            device.name,
            device.kind.label(),
            device.connection,
            device.status(),
            if device.windows_cached {
                " · Caché de Windows"
            } else {
                ""
            },
            device.detail
        );
    }
    for error in snapshot.errors {
        eprintln!("{error}");
    }
    if let Ok(api) = HidApi::new() {
        println!("\nInterfaces HID (identificadores para comprobar compatibilidad):");
        for info in api.device_list() {
            println!(
                "{:04x}:{:04x} usage={:04x}:{:04x} {}",
                info.vendor_id(),
                info.product_id(),
                info.usage_page(),
                info.usage(),
                info.product_string().unwrap_or("")
            );
        }
    }
    if initialized {
        unsafe { RoUninitialize() };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bluetooth_appearance_identifies_types_without_guessing_names() {
        assert_eq!(kind_from_appearance(0x03c1), DeviceKind::Keyboard);
        assert_eq!(kind_from_appearance(0x03c2), DeviceKind::Mouse);
        assert_eq!(kind_from_appearance(0x03c4), DeviceKind::Gamepad);
        for appearance in [0x0941, 0x0942, 0x0943] {
            assert_eq!(kind_from_appearance(appearance), DeviceKind::Headphones);
        }
        assert_eq!(kind_from_appearance(0x0841), DeviceKind::Speaker);
        assert_eq!(kind_from_appearance(0x0000), DeviceKind::Unknown);
        assert_eq!(kind_from_appearance(0x03c0), DeviceKind::Unknown);
        assert_eq!(kind_from_appearance(0x0083), DeviceKind::Unknown); // Laptop, no teclado.
    }

    #[test]
    fn bluetooth_class_decodes_keyboard_pointing_bits_and_audio_subclasses() {
        assert_eq!(kind_from_class(0x0540), DeviceKind::Keyboard);
        assert_eq!(kind_from_class(0x2580), DeviceKind::Mouse);
        assert_eq!(kind_from_class(0x0508), DeviceKind::Gamepad);
        assert_eq!(kind_from_class(0x0404), DeviceKind::Headphones);
        assert_eq!(kind_from_class(0x0418), DeviceKind::Headphones);
        assert_eq!(kind_from_class(0x0414), DeviceKind::Speaker);
        assert_eq!(kind_from_class(0x05c0), DeviceKind::Unknown); // Combo, no asumir solo teclado.
        assert_eq!(kind_from_class(0x010c), DeviceKind::Unknown);
    }

    #[test]
    fn hid_classification_is_conservative_for_composite_receivers() {
        assert_eq!(
            kind_from_hid_usages([(1, 2), (0xff01, 1)].into_iter()),
            DeviceKind::Mouse
        );
        assert_eq!(
            kind_from_hid_usages([(1, 6), (0x0c, 1)].into_iter()),
            DeviceKind::Keyboard
        );
        assert_eq!(
            kind_from_hid_usages([(1, 5)].into_iter()),
            DeviceKind::Gamepad
        );
        assert_eq!(
            kind_from_hid_usages([(1, 2), (1, 6)].into_iter()),
            DeviceKind::Unknown
        );
        assert_eq!(
            kind_from_hid_usages([(0xff01, 1), (0x0c, 1)].into_iter()),
            DeviceKind::Unknown
        );
    }

    fn connected_device(id: &str, responds: bool) -> Device {
        let mut d = device(responds.then_some(Level::Percent(78)), false);
        d.id = id.into();
        d.connection = if id.starts_with("bt:") {
            "Bluetooth"
        } else if id.starts_with("gamepad:") {
            "USB / receptor"
        } else {
            "USB / 2.4 GHz"
        };
        d
    }

    fn connection_snapshot(at: Instant, seconds: u64, devices: Vec<Device>) -> Snapshot {
        Snapshot {
            updated: Some(at + Duration::from_secs(seconds)),
            devices,
            ..Snapshot::default()
        }
    }

    #[test]
    fn connected_notifications_skip_startup_and_repeated_readings() {
        let at = Instant::now();
        let mut tracker = Connections::default();
        let initial = vec![
            connected_device("bt:old", true),
            connected_device("hid:old", true),
        ];
        assert!(tracker
            .update(&connection_snapshot(at, 0, initial.clone()), true)
            .is_empty());
        assert!(tracker
            .update(&connection_snapshot(at, 5, initial.clone()), true)
            .is_empty());
        let mut devices = initial;
        devices.push(connected_device("bt:new", true));
        let current = connection_snapshot(at, 10, devices.clone());
        let notices = tracker.update(&current, true);
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].id, "bt:new");
        assert!(tracker.update(&current, true).is_empty());
        assert!(tracker
            .update(&connection_snapshot(at, 15, devices), true)
            .is_empty());
    }

    #[test]
    fn receiver_only_notifies_when_the_peripheral_responds() {
        let at = Instant::now();
        let mut tracker = Connections::default();
        let mut stability = LinkStability::default();
        let mut previous = Snapshot::default();
        let mut scan = |seconds, device| {
            let mut snapshot = connection_snapshot(at, seconds, vec![device]);
            stability.update(&mut snapshot, &previous);
            previous = snapshot.clone();
            snapshot
        };
        let idle = connected_device("hid:k7", false);
        assert!(tracker.update(&scan(0, idle.clone()), true).is_empty());
        assert!(tracker.update(&scan(5, idle.clone()), true).is_empty());
        let active = connected_device("hid:k7", true);
        assert_eq!(tracker.update(&scan(10, active.clone()), true).len(), 1);
        let missed = scan(15, idle.clone());
        assert_eq!(missed.devices[0].level, Some(Level::Percent(78)));
        assert!(missed.devices[0].stale);
        assert!(!missed.show_previous_reading(&missed.devices[0]));
        assert!(!missed.confirmed_link(&missed.devices[0]));
        assert!(!missed.show_inactive_receiver(&missed.devices[0]));
        assert!(tracker.update(&missed, true).is_empty());
        let rapid_refresh = scan(16, idle.clone());
        assert!(!rapid_refresh.show_previous_reading(&rapid_refresh.devices[0]));
        assert!(tracker.update(&rapid_refresh, true).is_empty());
        assert!(tracker.update(&scan(20, active.clone()), true).is_empty());
        tracker.update(&scan(25, idle.clone()), true);
        let no_reading = scan(30, idle.clone());
        assert!(no_reading.show_previous_reading(&no_reading.devices[0]));
        assert!(!no_reading.show_inactive_receiver(&no_reading.devices[0]));
        assert!(tracker.update(&no_reading, true).is_empty());
        let sleeping = scan(40, idle);
        assert!(sleeping.show_inactive_receiver(&sleeping.devices[0]));
        assert!(tracker.update(&sleeping, true).is_empty());
        assert!(tracker.update(&scan(70, active), true).is_empty());
    }

    #[test]
    fn only_confirmed_absence_rearms_connection_notice() {
        let at = Instant::now();
        let mut tracker = Connections::default();
        let mut stability = LinkStability::default();
        let mut previous = Snapshot::default();
        let mut scan = |seconds, devices, unavailable| {
            let mut snapshot = connection_snapshot(at, seconds, devices);
            snapshot.unavailable.extend(unavailable);
            stability.update(&mut snapshot, &previous);
            previous = snapshot.clone();
            snapshot
        };
        let devices = vec![
            connected_device("hid:k7", true),
            connected_device("bt:keyboard", false),
            connected_device("gamepad:pad", true),
        ];
        tracker.update(&scan(0, devices.clone(), vec![]), true);
        for seconds in [5, 6, 9] {
            let missing = scan(seconds, vec![], vec![]);
            assert_eq!(missing.devices.len(), 3);
            assert!(missing
                .devices
                .iter()
                .all(|d| d.stale && !missing.confirmed_link(d)));
            assert!(tracker.update(&missing, true).is_empty());
        }
        assert!(tracker
            .update(&scan(10, devices.clone(), vec![]), true)
            .is_empty());
        tracker.update(&scan(15, vec![], vec![]), true);
        let failed = scan(
            20,
            vec![],
            vec![Source::Bluetooth, Source::Controller, Source::Receiver],
        );
        assert_eq!(failed.devices.len(), 3);
        tracker.update(&failed, true);
        let first = scan(25, vec![], vec![]);
        assert_eq!(
            first.devices.len(),
            3,
            "un fallo de fuente no cuenta como ausencia"
        );
        tracker.update(&first, true);
        let disconnected = scan(30, vec![], vec![]);
        assert!(disconnected.devices.is_empty());
        tracker.update(&disconnected, true);
        // Bluetooth confirma el enlace aunque no informe porcentaje.
        assert_eq!(tracker.update(&scan(35, devices, vec![]), true).len(), 3);
    }

    #[test]
    fn failed_sources_do_not_create_disconnect_or_startup_notifications() {
        let at = Instant::now();
        let mut tracker = Connections::default();
        let mut failed = connection_snapshot(at, 0, Vec::new());
        failed.unavailable.insert(Source::Bluetooth);
        assert!(tracker.update(&failed, true).is_empty());
        let bt = connected_device("bt:mouse", true);
        assert!(tracker
            .update(&connection_snapshot(at, 5, vec![bt.clone()]), true)
            .is_empty());
        failed.updated = Some(at + Duration::from_secs(10));
        assert!(tracker.update(&failed, true).is_empty());
        assert!(tracker
            .update(&connection_snapshot(at, 60, vec![bt]), true)
            .is_empty());
    }

    #[test]
    fn enabling_connections_does_not_announce_already_connected_devices() {
        let at = Instant::now();
        let mut tracker = Connections::default();
        tracker.update(&connection_snapshot(at, 0, Vec::new()), false);
        let devices = vec![connected_device("bt:mouse", true)];
        let current = connection_snapshot(at, 5, devices.clone());
        assert!(tracker.update(&current, false).is_empty());
        assert!(tracker.update(&current, true).is_empty());
        let mut devices = devices;
        devices.push(connected_device("hid:k7", true));
        assert!(tracker
            .update(&connection_snapshot(at, 10, devices.clone()), true)
            .is_empty());
        devices.push(connected_device("bt:keyboard", false));
        assert_eq!(
            tracker
                .update(&connection_snapshot(at, 15, devices), true)
                .len(),
            1
        );
    }

    fn device(level: Option<Level>, charging: bool) -> Device {
        Device {
            id: "mouse".into(),
            name: "Mouse".into(),
            kind: DeviceKind::Mouse,
            connection: "USB",
            level,
            charging,
            stale: false,
            windows_cached: false,
            detail: String::new(),
        }
    }

    #[test]
    fn sleeping_mouse_keeps_an_explicit_previous_reading_without_alerting() {
        let old = Snapshot {
            devices: vec![device(Some(Level::Percent(18)), false)],
            ..Snapshot::default()
        };
        let mut current = Snapshot {
            devices: vec![device(None, false)],
            ..Snapshot::default()
        };
        current.devices[0].kind = DeviceKind::Unknown;
        preserve_last_readings(&mut current, &old);
        assert_eq!(current.devices[0].kind, DeviceKind::Mouse);
        assert_eq!(current.devices[0].level, Some(Level::Percent(18)));
        assert!(current.devices[0].status().contains("Última lectura"));
        let mut alerts = Alerts::default();
        assert!(alerts.update(&current.devices, true).is_empty());
        assert_eq!(alerts.update(&old.devices, true).len(), 1);
    }

    #[test]
    fn bluetooth_prefers_a_direct_level_when_both_sources_report() {
        let mut phone = connected_device("bt:phone", false);
        let windows = Some(Level::Percent(80));
        phone.update_bluetooth_level(None, windows);
        assert!(phone.windows_cached && !phone.stale);
        assert_eq!(phone.status(), "80 %");

        let direct = Some(Level::Percent(75));
        phone.update_bluetooth_level(direct, windows);
        assert_eq!(phone.level, direct);
        assert!(!phone.windows_cached && !phone.stale);
        phone.update_bluetooth_level(None, windows);
        assert_eq!(phone.level, direct);

        let mut cached = connected_device("bt:phone", false);
        cached.update_bluetooth_level(None, Some(Level::Percent(15)));
        let mut current = Snapshot {
            devices: vec![cached],
            ..Snapshot::default()
        };
        let previous = Snapshot {
            devices: vec![phone],
            ..Snapshot::default()
        };
        preserve_last_readings(&mut current, &previous);
        let retained = &mut current.devices[0];
        assert_eq!(retained.level, direct);
        assert!(retained.stale && !retained.windows_cached);
        let mut alerts = Alerts::default();
        assert!(alerts.update(&current.devices, true).is_empty());
        let retained = &mut current.devices[0];
        retained.update_bluetooth_level(Some(Level::Percent(20)), windows);
        assert_eq!(alerts.update(&current.devices, true).len(), 1);
    }

    #[test]
    fn connected_bluetooth_alerts_at_twenty_from_windows_and_ignores_retained_values() {
        let mut phone = connected_device("bt:phone", false);
        phone.update_bluetooth_level(None, Some(Level::Percent(20)));
        let previous = Snapshot {
            devices: vec![phone.clone()],
            ..Snapshot::default()
        };
        let mut current = Snapshot {
            devices: vec![connected_device("bt:phone", false)],
            ..Snapshot::default()
        };
        preserve_last_readings(&mut current, &previous);

        let mut alerts = Alerts::default();
        assert!(alerts.update(&current.devices, true).is_empty());
        assert_eq!(alerts.update(std::slice::from_ref(&phone), true).len(), 1);
        assert!(alerts.update(std::slice::from_ref(&phone), true).is_empty());
        phone.update_bluetooth_level(None, Some(Level::Percent(25)));
        assert!(alerts.update(std::slice::from_ref(&phone), true).is_empty());
        phone.update_bluetooth_level(None, Some(Level::Percent(20)));
        assert_eq!(alerts.update(&[phone], true).len(), 1);
    }

    #[test]
    fn bluetooth_zero_requires_a_direct_reading() {
        let mut phone = connected_device("bt:phone", false);
        let zero = Some(Level::Percent(0));
        phone.update_bluetooth_level(None, zero);
        assert_eq!(phone.level, None);
        assert_eq!(phone.status(), "Batería no disponible");
        let mut alerts = Alerts::default();
        assert!(alerts.update(std::slice::from_ref(&phone), true).is_empty());

        phone.update_bluetooth_level(zero, zero);
        assert_eq!(phone.level, zero);
        assert_eq!(alerts.update(&[phone], true).len(), 1);
    }

    #[test]
    fn k7_ultra_hardware_capture_and_asleep_responses() {
        // 5253:1021, obtenido del K7 Ultra conectado por 2.4 GHz durante la prueba.
        let awake = [
            0x11, 0xf9, 0xc8, 0xc7, 0xaf, 0xbe, 0xe3, 0xfa, 0xfa, 0xff, 0xf6, 0xb1, 0xff, 0xae,
            0xff, 0xff,
        ];
        assert_eq!(parse_mchose(&awake), Some((Level::Percent(78), false)));
        let mut asleep = [0xff; 21];
        asleep[0] = 0x11;
        asleep[1] = 0xf9;
        assert_eq!(parse_mchose(&asleep), None);
        let mut cached_without_link = awake;
        cached_without_link[10] = !1u8; // Mismo 78 %, bit de enlace desactivado.
        assert_eq!(parse_mchose(&cached_without_link), None);
    }

    #[test]
    fn bluetooth_and_cached_receiver_do_not_appear_as_two_active_mice() {
        let mut bluetooth = device(Some(Level::Percent(78)), false);
        bluetooth.id = "bt:mouse".into();
        bluetooth.name = "MCHOSE K7 Ultra".into();
        bluetooth.connection = "Bluetooth";
        let mut receiver = bluetooth.clone();
        receiver.id = "hid:receiver".into();
        receiver.connection = "USB / 2.4 GHz";
        let previous = Snapshot {
            devices: vec![receiver.clone()],
            ..Snapshot::default()
        };
        receiver.level = None;
        let mut current = Snapshot {
            devices: vec![bluetooth, receiver],
            ..Snapshot::default()
        };
        preserve_last_readings(&mut current, &previous);
        assert_eq!(current.devices[1].level, Some(Level::Percent(78)));
        assert!(current.devices[1].receiver_without_link());
        let items = crate::tray::battery_items(&current, true, true);
        let active_connections = |items: &[crate::menu::Item]| {
            items
                .iter()
                .filter_map(|item| {
                    if let crate::menu::Item::DeviceStatus {
                        name,
                        status,
                        connection,
                        ..
                    } = item
                    {
                        if name == "MCHOSE K7 Ultra" {
                            assert_eq!(status, "78 %");
                            return Some(*connection);
                        }
                    }
                    None
                })
                .collect::<Vec<_>>()
        };
        use crate::island::NoticeIcon;
        assert!(active_connections(&items) == [NoticeIcon::Bluetooth]);
        assert!(items.iter().any(|item| matches!(item,
            crate::menu::Item::Group { label, .. } if label == "Receptores sin enlace confirmado (1)")));

        // Igual nombre y porcentaje no prueban identidad: dos enlaces activos
        // pueden corresponder a dos unidades y deben conservarse separados.
        current.devices[1].stale = false;
        let items = crate::tray::battery_items(&current, true, true);
        assert!(active_connections(&items) == [NoticeIcon::Bluetooth, NoticeIcon::Wireless]);
    }

    #[test]
    fn compx_rejects_wrong_query_empty_reply_and_bad_checksum() {
        let mut reply = [0u8; 32];
        reply[0] = 0x10;
        reply[2] = 1;
        reply[3] = 14;
        reply[4..8].copy_from_slice(b"M620");
        reply[13] = 18;
        reply[31] = reply[4..31].iter().fold(0u8, |sum, b| sum.wrapping_add(*b));
        assert!(valid_compx(&reply, 0x10));
        assert!(!valid_compx(&reply, 0x12));
        reply[13] = 99;
        assert!(!valid_compx(&reply, 0x10));
        assert!(!valid_compx(&[0; 32], 0x10));
    }

    #[test]
    fn low_alert_requires_valid_reading_and_rearms_after_recharge() {
        let mut alerts = Alerts::default();
        for level in [None, Some(Level::Percent(21)), Some(Level::Percent(100))] {
            assert!(alerts.update(&[device(level, false)], true).is_empty());
        }
        let boundary = device(Some(Level::Percent(20)), false);
        let low = device(Some(Level::Percent(19)), false);
        assert_eq!(
            alerts.update(std::slice::from_ref(&boundary), true).len(),
            1
        );
        assert!(alerts.update(&[], true).is_empty());
        assert!(alerts.update(&[device(None, false)], true).is_empty());
        assert!(alerts.update(std::slice::from_ref(&low), true).is_empty());
        assert!(alerts
            .update(&[device(Some(Level::Percent(21)), false)], true)
            .is_empty());
        assert!(alerts.update(std::slice::from_ref(&low), true).is_empty());
        alerts.update(&[device(Some(Level::Percent(25)), false)], true);
        assert_eq!(alerts.update(std::slice::from_ref(&low), true).len(), 1);
        alerts.update(&[device(Some(Level::Percent(19)), true)], true);
        assert_eq!(alerts.update(&[low], true).len(), 1);
    }

    #[test]
    fn disabled_alerts_do_not_consume_warning_and_coarse_levels_are_honest() {
        let mut alerts = Alerts::default();
        let low = device(Some(Level::Low), false);
        assert!(alerts.update(std::slice::from_ref(&low), false).is_empty());
        assert_eq!(alerts.update(&[low], true).len(), 1);
        assert_eq!(capacity_level(1, 3), Some(Level::Low));
        assert_eq!(capacity_level(18, 100), Some(Level::Percent(18)));
        assert_eq!(capacity_level(1, 0), None);
        assert_eq!(capacity_level(101, 100), None);
        assert_eq!(capacity_level(-1, 100), None);
    }

    #[test]
    fn ajazz_rejects_offline_and_transient_frames() {
        assert_eq!(
            parse_ajazz(&[5, 0, 0, 18, 1, 1, 1, 2]),
            Some(Level::Percent(18))
        );
        // Respuesta real del dongle AJAZZ 2.4G 8K (3151:402D).
        assert_eq!(
            parse_ajazz(&[5, 0, 0, 0x53, 1, 0, 1, 2, 0, 0]),
            Some(Level::Percent(83))
        );
        assert!(known_ajazz_apex(0x3151, 0x402d, std::iter::empty()));
        assert!(known_ajazz_apex(
            0x3151,
            0x4000,
            [(0xffff, 2), (1, 2)].into_iter()
        ));
        assert!(!known_ajazz_apex(
            0x3151,
            0x4000,
            [(0xffff, 2), (1, 6)].into_iter()
        ));
        assert!(!known_ajazz_apex(0x258a, 0x402d, [(1, 2)].into_iter()));
        assert_eq!(
            parse_ajazz(&[5, 0, 0, 0, 1, 1, 1, 2]),
            Some(Level::Percent(0))
        );
        for frame in [
            &[5, 0, 0, 0, 0, 0, 0, 0][..],
            &[5, 0xad, 4, 18, 1, 1, 1, 2],
            &[5, 0, 0, 255, 1, 1, 1, 2],
            &[5, 0, 0],
        ] {
            assert_eq!(parse_ajazz(frame), None);
        }
    }

    #[test]
    fn mchose_checks_response_identity_and_range() {
        let mut frame = vec![0x11, !6u8];
        frame.extend([0x53, 0x52, 0x31, 0, 2, 5, 7, 0, 9, 18, 1].map(|b| !b));
        assert_eq!(parse_mchose(&frame), Some((Level::Percent(18), true)));
        frame[11] = !101;
        assert_eq!(parse_mchose(&frame), None);
        frame[11] = !18;
        frame[1] = !4;
        assert_eq!(parse_mchose(&frame), None);
        assert_eq!(parse_mchose(&[0x11, !6, 0, 0]), None);
    }
}
