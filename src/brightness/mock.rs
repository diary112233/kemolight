use std::sync::{Arc, Mutex};

use super::{
    clamp_percent, BrightnessBackend, BrightnessError, BrightnessValue, DisplayId, DisplayInfo,
    DisplayKind, Result,
};

#[derive(Debug, Clone)]
struct MockDisplay {
    id: DisplayId,
    name: String,
    kind: DisplayKind,
    current: u32,
    supported: bool,
}

#[derive(Clone)]
pub struct MockBrightnessBackend {
    displays: Arc<Mutex<Vec<MockDisplay>>>,
}

impl Default for MockBrightnessBackend {
    fn default() -> Self {
        Self {
            displays: Arc::new(Mutex::new(vec![
                MockDisplay {
                    id: DisplayId::new("mock:internal"),
                    name: "Built-in Display".to_string(),
                    kind: DisplayKind::InternalBacklight,
                    current: 60,
                    supported: true,
                },
                MockDisplay {
                    id: DisplayId::new("mock:external"),
                    name: "USB-C Monitor".to_string(),
                    kind: DisplayKind::ExternalDdcCi,
                    current: 70,
                    supported: true,
                },
            ])),
        }
    }
}

impl BrightnessBackend for MockBrightnessBackend {
    fn list_displays(&self) -> Result<Vec<DisplayInfo>> {
        let displays = self.lock()?;
        Ok(displays
            .iter()
            .map(|display| DisplayInfo {
                id: display.id.clone(),
                name: display.name.clone(),
                kind: display.kind,
                current: Some(display.current),
                max: 100,
                supported: display.supported,
            })
            .collect())
    }

    fn get_brightness(&self, display_id: &DisplayId) -> Result<BrightnessValue> {
        let displays = self.lock()?;
        let display = displays
            .iter()
            .find(|display| &display.id == display_id)
            .ok_or_else(|| BrightnessError::DisplayNotFound(display_id.clone()))?;

        Ok(BrightnessValue {
            current: display.current,
            max: 100,
        })
    }

    fn set_brightness(&self, display_id: &DisplayId, value: u32) -> Result<()> {
        let mut displays = self.lock()?;
        let display = displays
            .iter_mut()
            .find(|display| &display.id == display_id)
            .ok_or_else(|| BrightnessError::DisplayNotFound(display_id.clone()))?;

        display.current = clamp_percent(value);
        Ok(())
    }
}

impl MockBrightnessBackend {
    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Vec<MockDisplay>>> {
        self.displays
            .lock()
            .map_err(|_| BrightnessError::Other("mock backend lock was poisoned".to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_backend_lists_and_sets_displays() -> Result<()> {
        let backend = MockBrightnessBackend::default();
        let displays = backend.list_displays()?;
        assert_eq!(displays.len(), 2);

        let id = displays[0].id.clone();
        backend.set_brightness(&id, 150)?;
        assert_eq!(backend.get_brightness(&id)?.percent(), 100);

        backend.set_brightness(&id, 42)?;
        assert_eq!(backend.get_brightness(&id)?.percent(), 42);
        Ok(())
    }
}
