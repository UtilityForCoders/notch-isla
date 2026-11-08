//! Batería por el uso estándar HID "Battery Strength" (página Generic Device
//! Controls `0x06`, uso `0x20`). Solo se piden los informes que el propio
//! dispositivo declara en su descriptor; no se envían comandos de fabricante.

use windows::core::PCWSTR;
use windows::Win32::Devices::HumanInterfaceDevice::{
    HidD_FreePreparsedData, HidD_GetFeature, HidD_GetInputReport, HidD_GetPreparsedData,
    HidP_Feature, HidP_GetCaps, HidP_GetSpecificValueCaps, HidP_GetUsageValue, HidP_Input,
    HIDP_CAPS, HIDP_REPORT_TYPE, HIDP_STATUS_SUCCESS, HIDP_VALUE_CAPS, PHIDP_PREPARSED_DATA,
};
use windows::Win32::Foundation::{CloseHandle, GENERIC_READ, GENERIC_WRITE, HANDLE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};

const GENERIC_DEVICE_PAGE: u16 = 0x06;
const BATTERY_STRENGTH: u16 = 0x20;

/// Resultado para una colección HID.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Strength {
    /// El descriptor no declara Battery Strength.
    Undeclared,
    /// Lo declara, pero el informe no se pudo leer o venía fuera de rango.
    NoReading,
    Percent(u8),
}

struct Handle(HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

struct Preparsed(PHIDP_PREPARSED_DATA);

impl Drop for Preparsed {
    fn drop(&mut self) {
        unsafe {
            let _ = HidD_FreePreparsedData(self.0);
        }
    }
}

fn open(path: &[u16], access: u32) -> Option<Handle> {
    unsafe {
        CreateFileW(
            PCWSTR(path.as_ptr()),
            access,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_FLAGS_AND_ATTRIBUTES(0),
            None,
        )
        .ok()
        .map(Handle)
    }
}

/// Escala el valor lógico declarado a porcentaje. Rechaza rangos inválidos.
pub(super) fn scale(value: i64, min: i64, max: i64) -> Option<u8> {
    if max <= min || value < min || value > max {
        return None;
    }
    Some(((value - min) * 100 / (max - min)) as u8)
}

pub(super) fn battery_strength(path: &str) -> Strength {
    let wide: Vec<u16> = path.encode_utf16().chain(Some(0)).collect();
    // Windows abre en exclusiva las colecciones de mouse y teclado: sin permisos
    // todavía se puede leer el descriptor y, en muchos controladores, los informes.
    let Some(handle) = open(&wide, GENERIC_READ.0 | GENERIC_WRITE.0).or_else(|| open(&wide, 0))
    else {
        return Strength::Undeclared;
    };
    let mut data = PHIDP_PREPARSED_DATA::default();
    if !unsafe { HidD_GetPreparsedData(handle.0, &mut data) } {
        return Strength::Undeclared;
    }
    let data = Preparsed(data);
    let mut caps = HIDP_CAPS::default();
    if unsafe { HidP_GetCaps(data.0, &mut caps) } != HIDP_STATUS_SUCCESS {
        return Strength::Undeclared;
    }
    let mut declared = false;
    for (kind, length) in [
        (HidP_Input, caps.InputReportByteLength),
        (HidP_Feature, caps.FeatureReportByteLength),
    ] {
        let Some(value) = specific_cap(kind, &data) else {
            continue;
        };
        declared = true;
        if let Some(percent) = read(&handle, &data, kind, length, &value) {
            return Strength::Percent(percent);
        }
    }
    if declared {
        Strength::NoReading
    } else {
        Strength::Undeclared
    }
}

fn specific_cap(kind: HIDP_REPORT_TYPE, data: &Preparsed) -> Option<HIDP_VALUE_CAPS> {
    let mut caps = [HIDP_VALUE_CAPS::default(); 4];
    let mut count = caps.len() as u16;
    let status = unsafe {
        HidP_GetSpecificValueCaps(
            kind,
            Some(GENERIC_DEVICE_PAGE),
            None,
            Some(BATTERY_STRENGTH),
            caps.as_mut_ptr(),
            &mut count,
            data.0,
        )
    };
    (status == HIDP_STATUS_SUCCESS && count > 0).then_some(caps[0])
}

fn read(
    handle: &Handle,
    data: &Preparsed,
    kind: HIDP_REPORT_TYPE,
    length: u16,
    cap: &HIDP_VALUE_CAPS,
) -> Option<u8> {
    if length == 0 {
        return None;
    }
    let mut report = vec![0u8; usize::from(length)];
    report[0] = cap.ReportID;
    let ok = unsafe {
        if kind == HidP_Input {
            HidD_GetInputReport(handle.0, report.as_mut_ptr().cast(), u32::from(length))
        } else {
            HidD_GetFeature(handle.0, report.as_mut_ptr().cast(), u32::from(length))
        }
    };
    if !ok {
        return None;
    }
    let mut value = 0u32;
    let status = unsafe {
        HidP_GetUsageValue(
            kind,
            GENERIC_DEVICE_PAGE,
            Some(cap.LinkCollection),
            BATTERY_STRENGTH,
            &mut value,
            data.0,
            &report,
        )
    };
    if status != HIDP_STATUS_SUCCESS {
        return None;
    }
    let (min, max) = (i64::from(cap.LogicalMin), i64::from(cap.LogicalMax));
    // Un máximo lógico negativo indica un campo sin signo de 8 bits mal declarado.
    let max = if max < 0 && cap.BitSize == 8 {
        255
    } else {
        max
    };
    scale(i64::from(value), min, max)
}

#[cfg(test)]
mod tests {
    use super::scale;

    #[test]
    fn battery_strength_scales_declared_range() {
        assert_eq!(scale(64, 0, 100), Some(64));
        assert_eq!(scale(255, 0, 255), Some(100));
        assert_eq!(scale(128, 0, 255), Some(50));
        assert_eq!(scale(101, 0, 100), None);
        assert_eq!(scale(5, 10, 10), None);
    }
}
