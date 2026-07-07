use std::{mem, ptr};

use windows_sys::{
    core::BOOL,
    Win32::{
        Devices::Display::{
            DestroyPhysicalMonitor, DestroyPhysicalMonitors, GetMonitorBrightness,
            GetMonitorCapabilities, GetNumberOfPhysicalMonitorsFromHMONITOR,
            GetPhysicalMonitorsFromHMONITOR, SetMonitorBrightness, MC_CAPS_BRIGHTNESS,
            PHYSICAL_MONITOR,
        },
        Foundation::{HANDLE, LPARAM, RECT},
        Graphics::Gdi::{EnumDisplayMonitors, HDC, HMONITOR},
    },
};

use super::{
    clamp_percent, BrightnessBackend, BrightnessError, BrightnessValue, DisplayId, DisplayInfo,
    DisplayKind, Result,
};

const ID_PREFIX: &str = "windows-ddcci:";

pub struct WindowsBrightnessBackend;

impl BrightnessBackend for WindowsBrightnessBackend {
    fn list_displays(&self) -> Result<Vec<DisplayInfo>> {
        let monitors = enumerate_physical_monitors()?;
        let mut displays = Vec::with_capacity(monitors.len());

        for monitor in monitors {
            let brightness = monitor.get_brightness();
            let supports_brightness = monitor.supports_brightness() || brightness.is_ok();
            let current = brightness.ok().map(|value| value.percent());

            displays.push(DisplayInfo {
                id: monitor.id(),
                name: monitor.name(),
                kind: DisplayKind::ExternalDdcCi,
                current,
                max: 100,
                supported: supports_brightness && current.is_some(),
            });
        }

        Ok(displays)
    }

    fn get_brightness(&self, display_id: &DisplayId) -> Result<BrightnessValue> {
        find_monitor(display_id)?.get_brightness()
    }

    fn set_brightness(&self, display_id: &DisplayId, value: u32) -> Result<()> {
        find_monitor(display_id)?.set_brightness(value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MonitorAddress {
    logical_index: usize,
    physical_index: usize,
}

impl MonitorAddress {
    fn id(self) -> DisplayId {
        DisplayId::new(format!(
            "{ID_PREFIX}{}:{}",
            self.logical_index, self.physical_index
        ))
    }

    fn parse(display_id: &DisplayId) -> Result<Self> {
        let Some(value) = display_id.as_str().strip_prefix(ID_PREFIX) else {
            return Err(BrightnessError::DisplayNotFound(display_id.clone()));
        };
        let Some((logical, physical)) = value.split_once(':') else {
            return Err(BrightnessError::DisplayNotFound(display_id.clone()));
        };
        let logical_index = logical
            .parse()
            .map_err(|_| BrightnessError::DisplayNotFound(display_id.clone()))?;
        let physical_index = physical
            .parse()
            .map_err(|_| BrightnessError::DisplayNotFound(display_id.clone()))?;

        Ok(Self {
            logical_index,
            physical_index,
        })
    }
}

struct PhysicalMonitor {
    address: MonitorAddress,
    handle: HANDLE,
    description: String,
}

impl PhysicalMonitor {
    fn id(&self) -> DisplayId {
        self.address.id()
    }

    fn name(&self) -> String {
        if self.description.is_empty() {
            format!(
                "DDC/CI Display {}.{}",
                self.address.logical_index + 1,
                self.address.physical_index + 1
            )
        } else {
            self.description.clone()
        }
    }

    fn supports_brightness(&self) -> bool {
        let mut capabilities = 0;
        let mut color_temperatures = 0;
        unsafe {
            GetMonitorCapabilities(self.handle, &mut capabilities, &mut color_temperatures) != 0
                && (capabilities & MC_CAPS_BRIGHTNESS) != 0
        }
    }

    fn get_brightness(&self) -> Result<BrightnessValue> {
        let mut min = 0;
        let mut current = 0;
        let mut max = 0;
        let ok = unsafe { GetMonitorBrightness(self.handle, &mut min, &mut current, &mut max) };
        if ok == 0 || max <= min {
            return Err(ddcci_error("failed to read monitor brightness"));
        }

        Ok(BrightnessValue {
            current: raw_to_percent(current, min, max),
            max: 100,
        })
    }

    fn set_brightness(&self, value: u32) -> Result<()> {
        let (min, max) = self.raw_range()?;
        let raw = percent_to_raw(clamp_percent(value), min, max);
        let ok = unsafe { SetMonitorBrightness(self.handle, raw) };
        if ok == 0 {
            return Err(ddcci_error("failed to set monitor brightness"));
        }

        Ok(())
    }

    fn raw_range(&self) -> Result<(u32, u32)> {
        let mut min = 0;
        let mut current = 0;
        let mut max = 0;
        let ok = unsafe { GetMonitorBrightness(self.handle, &mut min, &mut current, &mut max) };
        if ok == 0 || max <= min {
            return Err(ddcci_error("failed to read monitor brightness range"));
        }

        Ok((min, max))
    }
}

impl Drop for PhysicalMonitor {
    fn drop(&mut self) {
        unsafe {
            DestroyPhysicalMonitor(self.handle);
        }
    }
}

struct PhysicalMonitorArray {
    monitors: Vec<PHYSICAL_MONITOR>,
}

impl PhysicalMonitorArray {
    fn from_hmonitor(hmonitor: HMONITOR) -> Result<Self> {
        let mut count = 0;
        let ok = unsafe { GetNumberOfPhysicalMonitorsFromHMONITOR(hmonitor, &mut count) };
        if ok == 0 {
            return Err(ddcci_error("failed to count physical monitors"));
        }
        if count == 0 {
            return Ok(Self {
                monitors: Vec::new(),
            });
        }

        let mut monitors = vec![PHYSICAL_MONITOR::default(); count as usize];
        let ok = unsafe { GetPhysicalMonitorsFromHMONITOR(hmonitor, count, monitors.as_mut_ptr()) };
        if ok == 0 {
            return Err(ddcci_error("failed to get physical monitors"));
        }

        Ok(Self { monitors })
    }
}

impl Drop for PhysicalMonitorArray {
    fn drop(&mut self) {
        if self.monitors.is_empty() {
            return;
        }

        unsafe {
            DestroyPhysicalMonitors(self.monitors.len() as u32, self.monitors.as_ptr());
        }
    }
}

fn enumerate_physical_monitors() -> Result<Vec<PhysicalMonitor>> {
    let mut hmonitors = Vec::new();
    let ok = unsafe {
        EnumDisplayMonitors(
            ptr::null_mut(),
            ptr::null(),
            Some(enum_monitor),
            &mut hmonitors as *mut Vec<HMONITOR> as LPARAM,
        )
    };
    if ok == 0 {
        return Err(ddcci_error("failed to enumerate display monitors"));
    }

    let mut monitors = Vec::new();
    for (logical_index, hmonitor) in hmonitors.into_iter().enumerate() {
        let physical_monitors = PhysicalMonitorArray::from_hmonitor(hmonitor)?;
        for (physical_index, physical_monitor) in physical_monitors.monitors.iter().enumerate() {
            let description = physical_monitor.szPhysicalMonitorDescription;
            monitors.push(PhysicalMonitor {
                address: MonitorAddress {
                    logical_index,
                    physical_index,
                },
                handle: physical_monitor.hPhysicalMonitor,
                description: wide_to_string(&description),
            });
        }
        mem::forget(physical_monitors);
    }

    Ok(monitors)
}

unsafe extern "system" fn enum_monitor(
    hmonitor: HMONITOR,
    _hdc: HDC,
    _rect: *mut RECT,
    data: LPARAM,
) -> BOOL {
    let monitors = &mut *(data as *mut Vec<HMONITOR>);
    monitors.push(hmonitor);
    1
}

fn find_monitor(display_id: &DisplayId) -> Result<PhysicalMonitor> {
    let address = MonitorAddress::parse(display_id)?;
    enumerate_physical_monitors()?
        .into_iter()
        .find(|monitor| monitor.address == address)
        .ok_or_else(|| BrightnessError::DisplayNotFound(display_id.clone()))
}

fn raw_to_percent(current: u32, min: u32, max: u32) -> u32 {
    if max <= min {
        return 0;
    }
    ((current.saturating_sub(min).saturating_mul(100) + ((max - min) / 2)) / (max - min)).min(100)
}

fn percent_to_raw(percent: u32, min: u32, max: u32) -> u32 {
    min + (u64::from(clamp_percent(percent)) * u64::from(max - min) / 100) as u32
}

fn wide_to_string(value: &[u16]) -> String {
    let len = value.iter().position(|ch| *ch == 0).unwrap_or(value.len());
    String::from_utf16_lossy(&value[..len]).trim().to_string()
}

fn ddcci_error(context: &str) -> BrightnessError {
    let os_error = std::io::Error::last_os_error();
    let message = match os_error.raw_os_error() {
        Some(code) if code != 0 => format!("{context}: {os_error}"),
        _ => context.to_string(),
    };

    BrightnessError::BackendUnavailable(message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monitor_address_round_trips_id() -> Result<()> {
        let address = MonitorAddress {
            logical_index: 2,
            physical_index: 1,
        };

        assert_eq!(MonitorAddress::parse(&address.id())?, address);
        Ok(())
    }

    #[test]
    fn raw_percent_mapping_respects_monitor_range() {
        assert_eq!(raw_to_percent(10, 10, 110), 0);
        assert_eq!(raw_to_percent(60, 10, 110), 50);
        assert_eq!(raw_to_percent(120, 10, 110), 100);
        assert_eq!(percent_to_raw(50, 10, 110), 60);
        assert_eq!(percent_to_raw(150, 10, 110), 110);
    }

    #[test]
    fn wide_to_string_stops_at_nul() {
        assert_eq!(wide_to_string(&[65, 66, 0, 67]), "AB");
    }
}
