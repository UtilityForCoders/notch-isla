//! Logitech HID++ 2.0 por receptor (Unifying, Lightspeed, Bolt) o por cable.
//! Solo se usan funciones de lectura: versión del protocolo, tabla de funciones,
//! nombre y tipo del dispositivo y estado de batería (`0x1004`, `0x1000`, `0x1001`).
//! No se cambian DPI, iluminación, perfiles ni emparejamientos.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use hidapi::{HidApi, HidDevice};

use super::{Device, DeviceKind, Level};

pub(super) const LOGITECH: u16 = 0x046d;
const SHORT: u8 = 0x10;
const LONG: u8 = 0x11;
const SHORT_LEN: usize = 7;
const LONG_LEN: usize = 20;
/// Identificador de software propio, para separar nuestras respuestas de las de
/// otras aplicaciones (G HUB, Options+) que usen el mismo receptor.
const SW_ID: u8 = 0x0b;
const RECEIVER_INDEX: u8 = 0xff;
const TIMEOUT: Duration = Duration::from_millis(250);

const ROOT: u8 = 0x00;
const DEVICE_NAME: u16 = 0x0005;
const BATTERY_STATUS: u16 = 0x1000;
const BATTERY_VOLTAGE: u16 = 0x1001;
const UNIFIED_BATTERY: u16 = 0x1004;

#[derive(Debug, PartialEq, Eq)]
enum Failure {
    /// Respuesta de error de HID++ 1.0 (`8F`) o 2.0 (`FF`) con su código.
    Error(u8),
    Timeout,
    Io,
}

/// Lo que no cambia mientras el dispositivo sigue emparejado. Evita repetir
/// las consultas de nombre y tabla de funciones en cada sondeo.
#[derive(Clone)]
struct Profile {
    name: String,
    kind: DeviceKind,
    battery: Option<(u16, u8)>,
}

/// Perfiles por receptor e índice. Un índice que ya respondió sigue en la lista
/// (sin lectura) mientras el receptor esté conectado.
static PROFILES: Mutex<Option<HashMap<String, HashMap<u8, Profile>>>> = Mutex::new(None);

struct Channel {
    long: HidDevice,
    short: Option<HidDevice>,
}

impl Channel {
    fn request(
        &self,
        index: u8,
        feature: u8,
        function: u8,
        params: &[u8],
    ) -> Result<[u8; 16], Failure> {
        let mut message = [0u8; LONG_LEN];
        message[0] = LONG;
        message[1] = index;
        message[2] = feature;
        message[3] = (function << 4) | SW_ID;
        message[4..4 + params.len()].copy_from_slice(params);
        self.long.write(&message).map_err(|_| Failure::Io)?;
        let deadline = Instant::now() + TIMEOUT;
        while Instant::now() < deadline {
            let mut buffer = [0u8; LONG_LEN];
            if let Ok(count) = self.long.read_timeout(&mut buffer, 15) {
                if let Some(result) = match_response(&buffer[..count], index, feature, function) {
                    return result;
                }
            }
            if let Some(short) = &self.short {
                let mut buffer = [0u8; LONG_LEN];
                if let Ok(count) = short.read_timeout(&mut buffer, 5) {
                    if let Some(result) = match_response(&buffer[..count], index, feature, function)
                    {
                        return result;
                    }
                }
            }
        }
        Err(Failure::Timeout)
    }

    /// Versión del protocolo; `None` si el índice no responde o es HID++ 1.0.
    fn protocol(&self, index: u8) -> Option<u8> {
        let params = self.request(index, ROOT, 1, &[0, 0, 0x5a]).ok()?;
        (params[2] == 0x5a && params[0] >= 2).then_some(params[0])
    }

    fn feature(&self, index: u8, id: u16) -> Option<u8> {
        let params = self.request(index, ROOT, 0, &id.to_be_bytes()).ok()?;
        (params[0] != 0).then_some(params[0])
    }
}

/// Reconoce la respuesta a nuestra solicitud entre notificaciones y respuestas ajenas.
fn match_response(
    data: &[u8],
    index: u8,
    feature: u8,
    function: u8,
) -> Option<Result<[u8; 16], Failure>> {
    let call = (function << 4) | SW_ID;
    if data.len() < SHORT_LEN || data[1] != index {
        return None;
    }
    match data[0] {
        LONG if data.len() >= LONG_LEN && data[2] == feature && data[3] == call => {
            let mut params = [0u8; 16];
            params.copy_from_slice(&data[4..LONG_LEN]);
            Some(Ok(params))
        }
        LONG if data.len() >= LONG_LEN
            && data[2] == 0xff
            && data[3] == feature
            && data[4] == call =>
        {
            Some(Err(Failure::Error(data[5])))
        }
        SHORT if data[2] == 0x8f && data[3] == feature && data[4] == call => {
            Some(Err(Failure::Error(data[5])))
        }
        _ => None,
    }
}

/// `0x1004` Unified Battery, función `get_status`.
fn parse_unified(params: &[u8]) -> Option<(Level, bool)> {
    let level = match params[0] {
        1..=100 => Level::Percent(params[0]),
        0 if params[1] & 0x08 != 0 => Level::Full,
        0 if params[1] & 0x04 != 0 => Level::Medium,
        0 if params[1] & 0x03 != 0 => Level::Low,
        _ => return None,
    };
    Some((level, matches!(params[2], 1 | 2)))
}

/// `0x1000` Battery Status, función `getBatteryLevelStatus`.
fn parse_status(params: &[u8]) -> Option<(Level, bool)> {
    // 5 y superiores son errores de batería: no hay nivel fiable.
    if !(1..=100).contains(&params[0]) || params[2] > 4 {
        return None;
    }
    Some((Level::Percent(params[0]), matches!(params[2], 1 | 2 | 4)))
}

/// `0x1001` Battery Voltage. El porcentaje se estima con la curva de descarga de
/// una celda de litio, la misma aproximación que usa el proyecto Solaar.
fn parse_voltage(params: &[u8]) -> Option<(Level, bool)> {
    let millivolts = u16::from_be_bytes([params[0], params[1]]);
    if !(2500..=5000).contains(&millivolts) {
        return None;
    }
    let flags = params[2];
    let charging = flags & 0x80 != 0 && matches!(flags & 0x07, 0 | 2);
    Some((Level::Percent(voltage_percent(millivolts)), charging))
}

pub(super) fn voltage_percent(millivolts: u16) -> u8 {
    const CURVE: [(u16, u8); 13] = [
        (4186, 100),
        (4067, 90),
        (3989, 80),
        (3922, 70),
        (3859, 60),
        (3811, 50),
        (3778, 40),
        (3751, 30),
        (3717, 20),
        (3671, 10),
        (3646, 5),
        (3579, 2),
        (3500, 0),
    ];
    if millivolts >= CURVE[0].0 {
        return 100;
    }
    for pair in CURVE.windows(2) {
        let ((high_mv, high), (low_mv, low)) = (pair[0], pair[1]);
        if millivolts > low_mv {
            let span = u32::from(high_mv - low_mv);
            let offset = u32::from(millivolts - low_mv);
            return low + ((u32::from(high - low) * offset) / span) as u8;
        }
    }
    0
}

fn kind_from_type(raw: u8) -> DeviceKind {
    match raw {
        0 | 2 => DeviceKind::Keyboard,
        3..=5 => DeviceKind::Mouse,
        8 => DeviceKind::Headphones,
        10..=12 => DeviceKind::Gamepad,
        14 => DeviceKind::Speaker,
        _ => DeviceKind::Unknown,
    }
}

fn profile(channel: &Channel, index: u8, fallback: &str) -> Profile {
    let mut name = None;
    let mut kind = DeviceKind::Unknown;
    if let Some(feature) = channel.feature(index, DEVICE_NAME) {
        if let Ok(params) = channel.request(index, feature, 0, &[]) {
            let length = usize::from(params[0]).min(64);
            let mut bytes = Vec::with_capacity(length);
            while bytes.len() < length {
                let Ok(chunk) = channel.request(index, feature, 1, &[bytes.len() as u8]) else {
                    break;
                };
                let take = (length - bytes.len()).min(16);
                bytes.extend_from_slice(&chunk[..take]);
            }
            let text = String::from_utf8_lossy(&bytes)
                .trim_matches(char::from(0))
                .trim()
                .to_string();
            if !text.is_empty() {
                name = Some(text);
            }
        }
        if let Ok(params) = channel.request(index, feature, 2, &[]) {
            kind = kind_from_type(params[0]);
        }
    }
    let battery = [UNIFIED_BATTERY, BATTERY_STATUS, BATTERY_VOLTAGE]
        .into_iter()
        .find_map(|id| channel.feature(index, id).map(|feature| (id, feature)));
    Profile {
        name: name.unwrap_or_else(|| fallback.to_string()),
        kind,
        battery,
    }
}

fn battery(channel: &Channel, index: u8, (id, feature): (u16, u8)) -> Option<(Level, bool)> {
    match id {
        UNIFIED_BATTERY => parse_unified(&channel.request(index, feature, 1, &[]).ok()?),
        BATTERY_STATUS => parse_status(&channel.request(index, feature, 0, &[]).ok()?),
        _ => parse_voltage(&channel.request(index, feature, 0, &[]).ok()?),
    }
}

/// Clave de la interfaz: la ruta sin el sufijo `&colNN` ni el número final de
/// instancia, que cambian entre las colecciones de una misma interfaz.
pub(super) fn interface_key(path: &str) -> String {
    let lower = path.to_lowercase();
    let parts: Vec<&str> = lower.split('#').collect();
    if parts.len() < 3 {
        return lower;
    }
    let hardware = match parts[1].find("&col") {
        Some(at) => &parts[1][..at],
        None => parts[1],
    };
    let instance = parts[2].rsplit_once('&').map_or(parts[2], |(head, _)| head);
    format!("{}#{hardware}#{instance}", parts[0])
}

/// Las conexiones Bluetooth ya aparecen en la lista de Bluetooth.
pub(super) fn bluetooth_path(path: &str) -> bool {
    let lower = path.to_lowercase();
    lower.contains("{00001124-") || lower.contains("{00001812-") || lower.contains("bth")
}

/// Busca receptores y dispositivos Logitech HID++ y devuelve sus pares
/// fabricante/producto para que la lectura genérica no los duplique.
pub(super) fn scan(api: &HidApi, out: &mut Vec<Device>, trace: bool) -> HashSet<(u16, u16)> {
    let mut handled = HashSet::new();
    let mut present = HashSet::new();
    let mut seen = HashSet::new();
    for info in api.device_list() {
        if info.vendor_id() != LOGITECH || info.usage_page() != 0xff00 || info.usage() != 2 {
            continue;
        }
        let path = info.path().to_string_lossy().to_string();
        if bluetooth_path(&path) {
            continue;
        }
        let key = interface_key(&path);
        if !seen.insert(key.clone()) {
            continue;
        }
        handled.insert((info.vendor_id(), info.product_id()));
        present.insert(key.clone());
        let Ok(long) = info.open_device(api) else {
            continue;
        };
        let short = api
            .device_list()
            .find(|d| {
                d.vendor_id() == LOGITECH
                    && d.usage_page() == 0xff00
                    && d.usage() == 1
                    && interface_key(&d.path().to_string_lossy()) == key
            })
            .and_then(|d| d.open_device(api).ok());
        let channel = Channel { long, short };
        let fallback = info.product_string().unwrap_or("Logitech").to_string();
        // Un dispositivo por cable responde como HID++ 2.0 en el índice FF;
        // un receptor es HID++ 1.0 y sus periféricos ocupan los índices 1 a 6.
        let direct = channel.protocol(RECEIVER_INDEX).is_some();
        let indices: Vec<u8> = if direct {
            vec![RECEIVER_INDEX]
        } else {
            (1..=6).collect()
        };
        if trace {
            println!(
                "Logitech {:04x}: {}",
                info.product_id(),
                if direct {
                    "dispositivo directo"
                } else {
                    "receptor"
                }
            );
        }
        let mut profiles = PROFILES.lock().unwrap_or_else(|e| e.into_inner());
        let known = profiles
            .get_or_insert_with(HashMap::new)
            .entry(key.clone())
            .or_default();
        for index in indices {
            let online = channel.protocol(index).is_some();
            if trace {
                println!(
                    "Logitech índice {index:02x}: {}",
                    if online { "responde" } else { "sin respuesta" }
                );
            }
            if online && !known.contains_key(&index) {
                known.insert(index, profile(&channel, index, &fallback));
            }
            let Some(profile) = known.get(&index) else {
                continue;
            };
            let value = online
                .then_some(profile.battery)
                .flatten()
                .and_then(|feature| battery(&channel, index, feature));
            if trace {
                println!("Logitech {}: {:?} {value:?}", profile.name, profile.battery);
            }
            out.push(Device {
                id: format!("hidpp:{key}:{index:02x}"),
                name: profile.name.clone(),
                kind: profile.kind,
                connection: if direct { "USB" } else { "USB / 2.4 GHz" },
                level: value.map(|v| v.0),
                charging: value.is_some_and(|v| v.1),
                stale: false,
                windows_cached: false,
                detail: if value.is_some() {
                    String::new()
                } else if online && profile.battery.is_none() {
                    "No informa la batería por HID++".into()
                } else {
                    "Sin lectura: apagado, dormido u ocupado".into()
                },
            });
        }
        if !direct && known.is_empty() {
            // Receptor sin periféricos que hayan respondido en esta sesión.
            out.push(Device {
                id: format!("hidpp:{key}"),
                name: fallback,
                kind: DeviceKind::Unknown,
                connection: "USB / 2.4 GHz",
                level: None,
                charging: false,
                stale: false,
                windows_cached: false,
                detail: "Sin lectura: apagado, dormido u ocupado".into(),
            });
        }
    }
    // Olvidar los receptores retirados: al volver a conectarlos se consultan de nuevo.
    if let Some(profiles) = PROFILES.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
        profiles.retain(|key, _| present.contains(key));
    }
    handled
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hidpp_matches_only_our_response_or_error() {
        let mut reply = [0u8; LONG_LEN];
        reply[..7].copy_from_slice(&[LONG, 1, 4, 0x10 | SW_ID, 55, 2, 0]);
        assert_eq!(
            match_response(&reply, 1, 4, 1),
            Some(Ok({
                let mut p = [0u8; 16];
                p[..3].copy_from_slice(&[55, 2, 0]);
                p
            }))
        );
        // Otra aplicación (otro identificador de software) o un índice distinto.
        reply[3] = 0x10 | 0x01;
        assert_eq!(match_response(&reply, 1, 4, 1), None);
        assert_eq!(match_response(&reply, 2, 4, 1), None);
        let error = [SHORT, 3, 0x8f, 0, 0x10 | SW_ID, 0x09, 0];
        assert_eq!(
            match_response(&error, 3, 0, 1),
            Some(Err(Failure::Error(9)))
        );
        let mut error2 = [0u8; LONG_LEN];
        error2[..6].copy_from_slice(&[LONG, 1, 0xff, 6, SW_ID, 5]);
        assert_eq!(
            match_response(&error2, 1, 6, 0),
            Some(Err(Failure::Error(5)))
        );
    }

    #[test]
    fn hidpp_battery_formats() {
        assert_eq!(
            parse_unified(&[64, 4, 0, 0]),
            Some((Level::Percent(64), false))
        );
        assert_eq!(parse_unified(&[0, 2, 1, 1]), Some((Level::Low, true)));
        assert_eq!(parse_unified(&[0, 0, 0, 0]), None);
        assert_eq!(
            parse_status(&[18, 10, 0]),
            Some((Level::Percent(18), false))
        );
        assert_eq!(parse_status(&[90, 0, 1]), Some((Level::Percent(90), true)));
        assert_eq!(parse_status(&[50, 0, 6]), None);
        assert_eq!(parse_status(&[0, 0, 0]), None);
        assert_eq!(
            parse_voltage(&[0x0f, 0xa0, 0x00]),
            Some((Level::Percent(81), false))
        );
        assert_eq!(
            parse_voltage(&[0x10, 0x68, 0x80]),
            Some((Level::Percent(100), true))
        );
        assert_eq!(parse_voltage(&[0, 0, 0]), None);
        assert_eq!(voltage_percent(3500), 0);
        assert_eq!(voltage_percent(3717), 20);
        assert_eq!(voltage_percent(4300), 100);
    }

    #[test]
    fn collections_of_one_interface_share_a_key() {
        let long = r"\\?\HID#VID_046D&PID_C539&MI_02&Col02#7&1a2b&0&0001#{4d1e55b2-f16f-11cf-88cb-001111000030}";
        let short = r"\\?\HID#VID_046D&PID_C539&MI_02&Col01#7&1a2b&0&0000#{4d1e55b2-f16f-11cf-88cb-001111000030}";
        assert_eq!(interface_key(long), interface_key(short));
        assert_ne!(
            interface_key(long),
            interface_key(&long.replace("7&1a2b", "7&9f9f"))
        );
        assert!(bluetooth_path(
            r"\\?\HID#{00001812-0000-1000-8000-00805f9b34fb}_Dev_VID&02046d"
        ));
        assert!(!bluetooth_path(long));
    }
}
