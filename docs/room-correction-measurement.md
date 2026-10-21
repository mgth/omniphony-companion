# Room-correction measurements

Measurements for a future room-correction design step: the take keeps the response
a correction filter can be designed from. Nothing here designs, sends or applies a
filter, and nothing changes timing, levels, alignment proposals or a take's verdict.

## The stored response

The arrival review deconvolves each segment and then weights it into 300–9000 Hz
raised-cosine bands to find early peaks; the `impulse.segments[].response` of the
capture report is that band-weighted response, about −25 dB at 500 Hz against
+15 dB at 4 kHz relative to its shape before weighting. It stays as it is and keeps
serving the arrival review, but it cannot be equalized.

Every take with located sweeps now also stores:

- `response.wav`: three float32 channels, `reference_before`, `speaker`,
  `reference_after`, from 100 ms before each segment's correlation anchor to 350 ms
  after it. No band weighting and no microphone calibration. The extraction window
  ends 150 ms after the segment's trailing 250 ms of silence, so every swept
  frequency has its full tail up to 400 ms and the next sweep is never inside it.
- `response.json`: how to read it (compact, about 40 KB): the clock-compensated
  rate (`sample_rate_hz`; the WAV header can only hold the nominal capture rate),
  the anchor frame and each segment's sub-frame anchor offset, identities
  (output, reference, microphone, position), the sweep, the take's verdict and
  clipping count, the calibration used, and the magnitude.

The capture report references it as `response_file` (or gives `response_error`).
Clipped or rejected takes keep a response for inspection; `accepted_for_analysis`
and `clipped_samples` in `response.json` say not to design from them.

This response uses its own deconvolution. The arrival review resamples the sweep
template with linear interpolation, which droops the treble whenever the capture
and render clocks differ (up to about 2 dB at 10 kHz at 48 kHz, depending on the
sample phase). The stored response evaluates the sweep analytically at the
compensated rate instead, so its magnitude holds within 0.1 dB of a synthetic
room's from 300 Hz to 9 kHz at 120 ppm drift. It also accepts any valid sweep,
bass-only ones included, which the arrival review refuses.

### Magnitude

`magnitude` averages power over 1/48 octave at the grid frequencies
`1000 · 2^(k/48)` covering the sweep, so takes at any rate share frequencies. The
window opens 10 ms before the anchor (5 ms raised-cosine rise) and closes over the
last 50 ms of the stored response. `noise_db` is the deconvolution noise in the same
window, from the 80 ms before the anchor: `magnitude_db − noise_db` approximates the
signal-to-noise ratio per frequency. Fractional-octave smoothing, frequency-dependent
windowing and spatial averaging are left to the design step, which can recompute
them from `response.wav`.

## Microphone calibration

Choose the calibration file in the setup rail (Microphone → change → Microphone
calibration file). The rail says which file the next takes use, its point count
and range, or that there is none: alignment does not need one, room correction
will. An unreadable file stops measurements until it is fixed or cleared, as before.

Accepted files have two columns, frequency in Hz and dB, or three with an ignored
phase column; whitespace or commas separate them. Lines starting with `#`, `;`, `*`
or a double quote are comments, which covers the UMIK-1 `"Sens Factor …"` header.
The sensitivity factor is not used: levels stay relative.

The file gives the microphone's deviation from flat. `calibrated_magnitude_db` is
`magnitude_db` minus that deviation, interpolated linearly over log frequency and
held at the file's first or last value outside its range. The take folder keeps a
copy as `microphone-calibration.txt`; `response.json` records its source name, its
SHA-256, point count and range, so a design can tell calibrations apart. The
recording, `response.wav`, timing, levels and `calibration_applied: false` in the
capture report stay uncalibrated.

## Validation

Unit tests recover a known synthetic room (direct path plus a 2 ms echo at half
level, 120 ppm drift) within 0.08 dB of the expected 1/48-octave magnitude, check
the analytic template against the played sweep, and run a synthetic take with a
UMIK-style calibration through the take pipeline into stored, reloadable files.

Two bench takes (1 s sweeps, 200 Hz–10 kHz, −51 and −19 ppm drift) were re-analysed
with `cargo run -p companion-service --example take_response -- TAKE NEW_FOLDER`
and compared with an independent numpy/scipy deconvolution (FFT resampling of the
recording, band-limited spectral inverse): in-band correlation 0.998–0.999 over the
first 50 ms after sub-sample alignment and 0.993 to 300 ms; magnitude differences
of 0.02 dB median and at most 0.6 dB, in echo dips, from 250 Hz to 9 kHz. No take
was made with a calibrated microphone.
