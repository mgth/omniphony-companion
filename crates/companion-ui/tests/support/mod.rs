use companion_service::{
    controller::{Backend, Controller},
    measurement::{MeasureProgress, MeasureRequest, TakeOutcome},
};
use measurement_capture::{InputConfiguration, InputDevice};
use omniphony_adapter::{Snapshot, protocol::Output};
use std::{
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

pub struct UiBench {
    pub starts: Arc<AtomicUsize>,
}
impl Backend for UiBench {
    fn inputs(&self) -> Result<Vec<InputDevice>, String> {
        Ok(vec![InputDevice {
            name: "Preview audio interface".into(),
            description: "Preview audio interface".into(),
            channel_names: vec!["Mic 1".into(), "Mic 2".into()],
            is_monitor: false,
            configurations: vec![InputConfiguration {
                channels: 2,
                min_sample_rate: 44100,
                max_sample_rate: 96000,
                sample_format: "f32".into(),
            }],
        }])
    }
    fn discover(&self, _: SocketAddr) -> Result<Snapshot, String> {
        Ok(Snapshot {
            instance_id: "ui-preview".into(),
            revision: "ui-preview:1".into(),
            sample_rate: 48000,
            master_gain_db: 0.0,
            max_delay_ms: 100.0,
            unavailable_reason: None,
            outputs: [
                "Front left",
                "Front right",
                "Center",
                "Surround left",
                "Surround right",
                "Subwoofer",
            ]
            .iter()
            .enumerate()
            .map(|(index, name)| Output {
                id: (*name).into(),
                name: (*name).into(),
                index: index as u32,
                gain_db: [-1.5, -2.0, 0.0, -0.5, -0.5, -3.0][index],
                delay_ms: [0.0, 0.35, 1.2, 3.1, 3.45, 0.0][index],
                muted: index == 5,
                freq_low_hz: Some(if index == 5 { 20.0 } else { 80.0 }),
                freq_high_hz: Some(if index == 5 { 120.0 } else { 20000.0 }),
            })
            .collect(),
        })
    }
    fn measure(
        &self,
        _: MeasureRequest,
        _: &AtomicBool,
        _: &mut dyn FnMut(MeasureProgress),
    ) -> Result<TakeOutcome, String> {
        self.starts.fetch_add(1, Ordering::Relaxed);
        Err(
            "UI preview only. No microphone or speaker was used, and no recording was created."
                .into(),
        )
    }
}
pub fn ready() -> (Controller, Arc<AtomicUsize>) {
    let starts = Arc::new(AtomicUsize::new(0));
    let backend = Arc::new(UiBench {
        starts: starts.clone(),
    });
    let mut c = Controller::new(backend.clone());
    let endpoint = "127.0.0.1:9000".parse().unwrap();
    c.state.inputs = backend.inputs().unwrap();
    c.state.snapshot = Some(backend.discover(endpoint).unwrap());
    c.state.endpoint = Some(endpoint);
    c.settings.device = Some("Preview audio interface".into());
    c.settings.reference_output = Some("Front left".into());
    (c, starts)
}
