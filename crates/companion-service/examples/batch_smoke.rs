//! Explicit virtual-sink bench against a separately started renderer. No hardware.
use companion_service::{
    batch,
    measurement::{MeasureProgress, MeasureRequest},
};
use omniphony_adapter::{Client, Sweep};
use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};
fn main() -> companion_service::AppResult<()> {
    let args: Vec<_> = std::env::args().collect();
    if args.len() != 4 || !args[2].starts_with("pipewire:monitor:companion-batch-test-") {
        return Err(
            "usage: batch_smoke LOCAL_ENDPOINT pipewire:monitor:companion-batch-test-N NEW_FOLDER"
                .into(),
        );
    }
    let endpoint = args[1].parse()?;
    let snapshot = Client::connect(endpoint, Duration::from_secs(2))?.discover()?;
    let outputs: Vec<_> = snapshot
        .outputs
        .iter()
        .take(2)
        .map(|o| o.id.clone())
        .collect();
    if outputs.len() != 2 {
        return Err("bench requires two renderer outputs".into());
    }
    let request = MeasureRequest {
        endpoint,
        snapshot,
        device: args[2].clone(),
        input_channel: 1,
        sample_rate: 48000,
        microphone_id: "virtual-monitor".into(),
        position_id: "virtual-bench".into(),
        output: outputs[0].clone(),
        reference_output: outputs[0].clone(),
        calibration: None,
        sweep: Sweep {
            duration_ms: 1000,
            peak_dbfs: -30.0,
            start_hz: 200.0,
            end_hz: 10000.0,
        },
        out_dir: args[3].clone().into(),
    };
    let result = batch::measure(
        request.clone(),
        outputs.clone(),
        &AtomicBool::new(false),
        |_, _| {},
    )?;
    if let Some(error) = result.error {
        return Err(error.into());
    }
    assert_eq!(result.takes.len(), 2);
    for (id, take) in &result.takes {
        println!(
            "{id}: accepted={}, {:?}",
            take.accepted, take.rejection_reason
        );
        let review = companion_service::review::saved_take(&take.directory)?;
        assert_eq!(review.origin, "sink_monitor_capture");
        assert!(review.sequence_window.is_some());
    }
    assert!(result.takes[0].1.accepted);
    assert!(
        !result.takes[1].1.accepted,
        "right output must not leak into left monitor"
    );
    println!(
        "Shared-reference virtual batch and offline provenance review passed: {}",
        result.directory.display()
    );
    let mut request = request;
    request.out_dir = request.out_dir.with_file_name("cancelled-capture");
    let cancelled = AtomicBool::new(false);
    let result = batch::measure(request, outputs, &cancelled, |_, p| {
        if matches!(p, MeasureProgress::Playing(ref p) if p.rendered_frames > 96000) {
            cancelled.store(true, Ordering::Relaxed);
        }
    })?;
    assert!(result.cancelled);
    assert!(result.error.is_some());
    assert!(result.takes.is_empty());
    assert!(result.directory.join("recording.wav").exists());
    assert!(result.directory.join("batch-analysis.json").exists());
    let mut client = Client::connect(endpoint, Duration::from_secs(2))?;
    let snapshot = client.discover()?;
    client.begin(&snapshot)?;
    client.end()?;
    println!("Cancellation retained the partial recording and released the renderer session.");
    Ok(())
}
