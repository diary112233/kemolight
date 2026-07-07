use super::{BrightnessBackend, BrightnessError, BrightnessValue, DisplayId, DisplayInfo, Result};

pub struct MacosBrightnessBackend;

impl BrightnessBackend for MacosBrightnessBackend {
    fn list_displays(&self) -> Result<Vec<DisplayInfo>> {
        Err(BrightnessError::Unsupported)
    }

    fn get_brightness(&self, display_id: &DisplayId) -> Result<BrightnessValue> {
        let _ = display_id;
        Err(BrightnessError::Unsupported)
    }

    fn set_brightness(&self, display_id: &DisplayId, value: u32) -> Result<()> {
        let _ = (display_id, value);
        Err(BrightnessError::Unsupported)
    }
}
