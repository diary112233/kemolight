use std::{
    fs, io,
    path::{Path, PathBuf},
};

#[cfg(feature = "linux-ddcutil")]
use std::{
    env,
    process::{Command, Output},
};

use crate::config::PreferredBackend;

use super::{
    clamp_percent, BrightnessBackend, BrightnessError, BrightnessValue, DisplayId, DisplayInfo,
    DisplayKind, Result,
};

const SYSFS_BACKLIGHT: &str = "/sys/class/backlight";
#[cfg(feature = "linux-ddcutil")]
const DDCUTIL: &str = "ddcutil";

pub struct LinuxBrightnessBackend {
    sysfs: Option<SysfsBacklightBackend>,
    ddcutil: Option<DdcutilBackend>,
}

impl LinuxBrightnessBackend {
    pub fn new(preferred: PreferredBackend) -> Self {
        let sysfs = matches!(preferred, PreferredBackend::Auto | PreferredBackend::Sysfs)
            .then(SysfsBacklightBackend::default);
        let ddcutil = matches!(
            preferred,
            PreferredBackend::Auto | PreferredBackend::Ddcutil
        )
        .then(DdcutilBackend::default);

        Self { sysfs, ddcutil }
    }
}

impl BrightnessBackend for LinuxBrightnessBackend {
    fn list_displays(&self) -> Result<Vec<DisplayInfo>> {
        let mut displays = Vec::new();
        let mut first_error = None;

        if let Some(sysfs) = &self.sysfs {
            match sysfs.list_displays() {
                Ok(mut detected) => displays.append(&mut detected),
                Err(error) => {
                    first_error.get_or_insert(error);
                }
            };
        }

        if let Some(ddcutil) = &self.ddcutil {
            match ddcutil.list_displays() {
                Ok(mut detected) => displays.append(&mut detected),
                Err(error) => {
                    if !displays.is_empty() && matches!(error, BrightnessError::CommandNotFound(_))
                    {
                        displays.push(DisplayInfo {
                            id: DisplayId::new("ddcutil:unavailable"),
                            name: "External DDC/CI (install ddcutil)".to_string(),
                            kind: DisplayKind::ExternalDdcCi,
                            current: None,
                            max: 100,
                            supported: false,
                        });
                    }
                    first_error.get_or_insert(error);
                }
            };
        }

        if displays.is_empty() {
            if let Some(error) = first_error {
                return Err(error);
            }
        }

        Ok(displays)
    }

    fn get_brightness(&self, display_id: &DisplayId) -> Result<BrightnessValue> {
        if display_id
            .as_str()
            .starts_with(SysfsBacklightBackend::ID_PREFIX)
        {
            return self
                .sysfs
                .as_ref()
                .ok_or_else(|| {
                    BrightnessError::BackendUnavailable("sysfs backend is disabled".into())
                })?
                .get_brightness(display_id);
        }

        if display_id.as_str().starts_with(DdcutilBackend::ID_PREFIX) {
            return self
                .ddcutil
                .as_ref()
                .ok_or_else(|| {
                    BrightnessError::BackendUnavailable("ddcutil backend is disabled".into())
                })?
                .get_brightness(display_id);
        }

        Err(BrightnessError::DisplayNotFound(display_id.clone()))
    }

    fn set_brightness(&self, display_id: &DisplayId, value: u32) -> Result<()> {
        if display_id
            .as_str()
            .starts_with(SysfsBacklightBackend::ID_PREFIX)
        {
            return self
                .sysfs
                .as_ref()
                .ok_or_else(|| {
                    BrightnessError::BackendUnavailable("sysfs backend is disabled".into())
                })?
                .set_brightness(display_id, value);
        }

        if display_id.as_str().starts_with(DdcutilBackend::ID_PREFIX) {
            return self
                .ddcutil
                .as_ref()
                .ok_or_else(|| {
                    BrightnessError::BackendUnavailable("ddcutil backend is disabled".into())
                })?
                .set_brightness(display_id, value);
        }

        Err(BrightnessError::DisplayNotFound(display_id.clone()))
    }
}

pub struct SysfsBacklightBackend {
    root: PathBuf,
}

impl Default for SysfsBacklightBackend {
    fn default() -> Self {
        Self {
            root: PathBuf::from(SYSFS_BACKLIGHT),
        }
    }
}

impl SysfsBacklightBackend {
    const ID_PREFIX: &'static str = "sysfs:";

    #[cfg(test)]
    fn with_root(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn path_for(&self, display_id: &DisplayId) -> Result<PathBuf> {
        let Some(name) = display_id.as_str().strip_prefix(Self::ID_PREFIX) else {
            return Err(BrightnessError::DisplayNotFound(display_id.clone()));
        };

        let path = self.root.join(name);
        if path.is_dir() {
            Ok(path)
        } else {
            Err(BrightnessError::DisplayNotFound(display_id.clone()))
        }
    }

    fn read_raw(path: &Path, file: &str) -> Result<u64> {
        let contents = fs::read_to_string(path.join(file)).map_err(map_io_error)?;
        contents
            .trim()
            .parse::<u64>()
            .map_err(|err| BrightnessError::Other(format!("invalid {file} value: {err}")))
    }

    fn read_display(path: &Path, name: String) -> Result<DisplayInfo> {
        let max_raw = Self::read_raw(path, "max_brightness")?;
        let current = match Self::read_raw(path, "brightness") {
            Ok(raw) if max_raw > 0 => Some(raw_to_percent(raw, max_raw)),
            Ok(_) => None,
            Err(_) => None,
        };

        Ok(DisplayInfo {
            id: DisplayId::new(format!("{}{}", Self::ID_PREFIX, name)),
            name,
            kind: DisplayKind::InternalBacklight,
            current,
            max: 100,
            supported: max_raw > 0,
        })
    }
}

impl BrightnessBackend for SysfsBacklightBackend {
    fn list_displays(&self) -> Result<Vec<DisplayInfo>> {
        let entries = match fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => return Err(map_io_error(err)),
        };

        let mut displays = Vec::new();
        for entry in entries {
            let entry = entry.map_err(map_io_error)?;
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            match Self::read_display(&path, name) {
                Ok(display) => displays.push(display),
                Err(error) => {
                    displays.push(DisplayInfo {
                        id: DisplayId::new(format!(
                            "{}{}",
                            Self::ID_PREFIX,
                            entry.file_name().to_string_lossy()
                        )),
                        name: entry.file_name().to_string_lossy().to_string(),
                        kind: DisplayKind::InternalBacklight,
                        current: None,
                        max: 100,
                        supported: false,
                    });
                    if matches!(error, BrightnessError::PermissionDenied(_)) {
                        return Err(error);
                    }
                }
            }
        }

        Ok(displays)
    }

    fn get_brightness(&self, display_id: &DisplayId) -> Result<BrightnessValue> {
        let path = self.path_for(display_id)?;
        let raw = Self::read_raw(&path, "brightness")?;
        let max_raw = Self::read_raw(&path, "max_brightness")?;
        if max_raw == 0 {
            return Err(BrightnessError::BackendUnavailable(
                "sysfs max_brightness is zero".into(),
            ));
        }
        Ok(BrightnessValue {
            current: raw_to_percent(raw, max_raw),
            max: 100,
        })
    }

    fn set_brightness(&self, display_id: &DisplayId, value: u32) -> Result<()> {
        let path = self.path_for(display_id)?;
        let max_raw = Self::read_raw(&path, "max_brightness")?;
        if max_raw == 0 {
            return Err(BrightnessError::BackendUnavailable(
                "sysfs max_brightness is zero".into(),
            ));
        }
        let raw = percent_to_raw(clamp_percent(value), max_raw);
        fs::write(path.join("brightness"), raw.to_string()).map_err(map_io_error)
    }
}

#[derive(Default)]
pub struct DdcutilBackend;

impl DdcutilBackend {
    const ID_PREFIX: &'static str = "ddcutil:";

    #[cfg(feature = "linux-ddcutil")]
    fn display_number(display_id: &DisplayId) -> Result<u32> {
        display_id
            .as_str()
            .strip_prefix(Self::ID_PREFIX)
            .and_then(|value| value.parse::<u32>().ok())
            .ok_or_else(|| BrightnessError::DisplayNotFound(display_id.clone()))
    }

    #[cfg(feature = "linux-ddcutil")]
    fn command_output(args: &[&str]) -> Result<Output> {
        if !command_exists(DDCUTIL) {
            return Err(BrightnessError::CommandNotFound(
                "ddcutil (install with: sudo apt install ddcutil)".to_string(),
            ));
        }

        Command::new(DDCUTIL)
            .args(args)
            .output()
            .map_err(map_io_error)
    }

    #[cfg(feature = "linux-ddcutil")]
    fn successful_output(args: &[&str]) -> Result<Output> {
        let output = Self::command_output(args)?;
        if output.status.success() {
            Ok(output)
        } else {
            Err(classify_ddcutil_error(&output))
        }
    }

    #[cfg(not(feature = "linux-ddcutil"))]
    fn feature_disabled() -> BrightnessError {
        BrightnessError::BackendUnavailable("linux-ddcutil feature is disabled".into())
    }
}

impl BrightnessBackend for DdcutilBackend {
    fn list_displays(&self) -> Result<Vec<DisplayInfo>> {
        #[cfg(not(feature = "linux-ddcutil"))]
        {
            Err(Self::feature_disabled())
        }

        #[cfg(feature = "linux-ddcutil")]
        {
            let output = Self::successful_output(&["detect", "--brief"])?;
            let stdout = String::from_utf8_lossy(&output.stdout);
            let mut displays = parse_ddcutil_detect(&stdout);

            for display in &mut displays {
                match self.get_brightness(&display.id) {
                    Ok(value) => {
                        display.current = Some(value.percent());
                        display.supported = true;
                    }
                    Err(BrightnessError::DdcCiDisabled) => display.supported = false,
                    Err(_) => {
                        display.current = None;
                    }
                }
            }

            Ok(displays)
        }
    }

    fn get_brightness(&self, display_id: &DisplayId) -> Result<BrightnessValue> {
        #[cfg(not(feature = "linux-ddcutil"))]
        {
            let _ = display_id;
            Err(Self::feature_disabled())
        }

        #[cfg(feature = "linux-ddcutil")]
        {
            let display = Self::display_number(display_id)?;
            let output =
                Self::successful_output(&["--display", &display.to_string(), "getvcp", "10"])?;
            let stdout = String::from_utf8_lossy(&output.stdout);
            let (current, max) = parse_ddcutil_getvcp(&stdout).ok_or_else(|| {
                BrightnessError::BackendUnavailable(format!(
                    "could not parse ddcutil getvcp output for display {display}"
                ))
            })?;

            Ok(BrightnessValue {
                current: raw_to_percent(current.into(), max.into()),
                max: 100,
            })
        }
    }

    fn set_brightness(&self, display_id: &DisplayId, value: u32) -> Result<()> {
        #[cfg(not(feature = "linux-ddcutil"))]
        {
            let _ = (display_id, value);
            Err(Self::feature_disabled())
        }

        #[cfg(feature = "linux-ddcutil")]
        {
            let display = Self::display_number(display_id)?;
            let value = clamp_percent(value).to_string();
            Self::successful_output(&["--display", &display.to_string(), "setvcp", "10", &value])?;
            Ok(())
        }
    }
}

fn raw_to_percent(raw: u64, max: u64) -> u32 {
    if max == 0 {
        return 0;
    }
    ((raw.saturating_mul(100) + (max / 2)) / max).min(100) as u32
}

fn percent_to_raw(percent: u32, max: u64) -> u64 {
    (u64::from(clamp_percent(percent)) * max + 50) / 100
}

fn map_io_error(err: io::Error) -> BrightnessError {
    if err.kind() == io::ErrorKind::PermissionDenied {
        BrightnessError::PermissionDenied(err.to_string())
    } else {
        BrightnessError::Io(err.to_string())
    }
}

#[cfg(feature = "linux-ddcutil")]
fn command_exists(command: &str) -> bool {
    let Some(paths) = env::var_os("PATH") else {
        return false;
    };
    env::split_paths(&paths).any(|path| path.join(command).is_file())
}

#[cfg(feature = "linux-ddcutil")]
fn classify_ddcutil_error(output: &Output) -> BrightnessError {
    let mut text = String::from_utf8_lossy(&output.stdout).to_string();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    let lower = text.to_ascii_lowercase();

    if lower.contains("permission denied") || lower.contains("operation not permitted") {
        return BrightnessError::PermissionDenied(text.trim().to_string());
    }

    if lower.contains("ddc/ci")
        || lower.contains("unsupported feature")
        || lower.contains("no ddc communication")
        || lower.contains("failed to get vcp")
    {
        return BrightnessError::DdcCiDisabled;
    }

    BrightnessError::BackendUnavailable(text.trim().to_string())
}

#[cfg(any(feature = "linux-ddcutil", test))]
fn parse_ddcutil_detect(output: &str) -> Vec<DisplayInfo> {
    #[derive(Default)]
    struct Pending {
        number: Option<u32>,
        model: Option<String>,
        connector: Option<String>,
    }

    fn push_pending(displays: &mut Vec<DisplayInfo>, pending: &mut Pending) {
        let Some(number) = pending.number.take() else {
            return;
        };
        let label = pending
            .model
            .take()
            .or_else(|| pending.connector.take())
            .unwrap_or_else(|| format!("DDC/CI Display {number}"));

        displays.push(DisplayInfo {
            id: DisplayId::new(format!("{}{}", DdcutilBackend::ID_PREFIX, number)),
            name: label,
            kind: DisplayKind::ExternalDdcCi,
            current: None,
            max: 100,
            supported: true,
        });
    }

    let mut displays = Vec::new();
    let mut pending = Pending::default();

    for line in output.lines() {
        let trimmed = line.trim();
        if let Some(number) = trimmed
            .strip_prefix("Display ")
            .and_then(|value| value.split_whitespace().next())
            .and_then(|value| value.parse::<u32>().ok())
        {
            push_pending(&mut displays, &mut pending);
            pending.number = Some(number);
            continue;
        }

        if let Some(model) = trimmed.strip_prefix("Model:") {
            let model = model.trim();
            if !model.is_empty() {
                pending.model = Some(model.to_string());
            }
            continue;
        }

        if let Some(connector) = trimmed.strip_prefix("DRM connector:") {
            let connector = connector.trim();
            if !connector.is_empty() {
                pending.connector = Some(connector.to_string());
            }
        }
    }

    push_pending(&mut displays, &mut pending);
    displays
}

#[cfg(any(feature = "linux-ddcutil", test))]
fn parse_ddcutil_getvcp(output: &str) -> Option<(u32, u32)> {
    let current = number_after(output, "current value")?;
    let max = number_after(output, "max value")?;
    if max == 0 {
        return None;
    }
    Some((current, max))
}

#[cfg(any(feature = "linux-ddcutil", test))]
fn number_after(input: &str, label: &str) -> Option<u32> {
    let start = input.find(label)?;
    let tail = &input[start + label.len()..];
    let digits: String = tail
        .chars()
        .skip_while(|ch| !ch.is_ascii_digit())
        .take_while(|ch| ch.is_ascii_digit())
        .collect();
    if digits.is_empty() {
        None
    } else {
        digits.parse().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        env,
        fs::{self, File},
        io::Write,
        time::{SystemTime, UNIX_EPOCH},
    };

    #[test]
    fn raw_percent_mapping_clamps() {
        assert_eq!(raw_to_percent(5, 10), 50);
        assert_eq!(raw_to_percent(15, 10), 100);
        assert_eq!(percent_to_raw(50, 200), 100);
        assert_eq!(percent_to_raw(150, 200), 200);
    }

    #[test]
    fn parses_ddcutil_detect_output() {
        let output = r#"
Display 1
   I2C bus:             /dev/i2c-4
   DRM connector:       card0-DP-1
   EDID synopsis:
      Mfg id:           DEL
      Model:            DELL U2720Q
Display 2
   I2C bus:             /dev/i2c-5
   DRM connector:       card0-HDMI-A-1
"#;

        let displays = parse_ddcutil_detect(output);
        assert_eq!(displays.len(), 2);
        assert_eq!(displays[0].id.as_str(), "ddcutil:1");
        assert_eq!(displays[0].name, "DELL U2720Q");
        assert_eq!(displays[1].name, "card0-HDMI-A-1");
    }

    #[test]
    fn parses_ddcutil_getvcp_output() {
        let output = "VCP code 0x10 (Brightness): current value =    42, max value =   100";
        assert_eq!(parse_ddcutil_getvcp(output), Some((42, 100)));
    }

    #[test]
    fn sysfs_backend_lists_mocked_backlight() -> Result<()> {
        let temp = temp_dir("kemolight-sysfs-test")?;
        let backlight = temp.join("intel_backlight");
        fs::create_dir_all(&backlight).map_err(map_io_error)?;
        write_file(backlight.join("max_brightness"), "1000")?;
        write_file(backlight.join("brightness"), "400")?;

        let backend = SysfsBacklightBackend::with_root(&temp);
        let displays = backend.list_displays()?;
        assert_eq!(displays.len(), 1);
        assert_eq!(displays[0].current, Some(40));

        fs::remove_dir_all(temp).map_err(map_io_error)?;
        Ok(())
    }

    fn temp_dir(prefix: &str) -> Result<PathBuf> {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|err| BrightnessError::Other(err.to_string()))?
            .as_nanos();
        let path = env::temp_dir().join(format!("{prefix}-{nanos}"));
        fs::create_dir_all(&path).map_err(map_io_error)?;
        Ok(path)
    }

    fn write_file(path: PathBuf, contents: &str) -> Result<()> {
        let mut file = File::create(path).map_err(map_io_error)?;
        file.write_all(contents.as_bytes()).map_err(map_io_error)
    }
}
