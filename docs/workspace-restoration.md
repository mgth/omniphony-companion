# Workspace restoration

Reopening the companion restores acquisition preferences and the previous working
view: the last 100 take results (the existing in-memory history limit), selected
take or saved review, current alignment, graph mode, and virtual-burst choices.
Eframe persistence retains window size/position where supported by the desktop,
panel extents, scrolling, expanded sections and reference/target graph selection.
The **Previous measurements** selector also exposes historical takes when their
speaker is no longer in the connected renderer's layout.

The service stores a small workspace index and view preferences with the existing
atomic, debounced `settings.json`. Normal exit flushes pending saves. Entries use
absolute session/report paths so a different launch directory cannot redirect
restoration. The index contains no audio or duplicated plots. Reports remain in
their original session folders; moving/deleting those folders requires restoring
them or opening the new location explicitly. Missing/corrupt entries are reported
individually and their references are retained; healthy results still open.

On the first upgrade from settings-only preferences, a bounded scan of the selected
recording folder imports recent single takes, batch children and alignment-pass
children, plus the latest alignment. The previous version did not record a selected
result or graph, so that first import selects the latest available result. Subsequent
launches use the saved index and exact view choices rather than rescanning.

For each indexed take, the service also discovers its latest `analysis-*.json`.
This includes reports produced by older versions of **Reanalyze saved passes**.
Clicking a speaker card selects that speaker and opens the analysis for the exact
take folder and speaker identity. A tinted background and outline mark the selection;
rows with no measurements show an explicit empty state instead of another speaker's
result. The single result view labels its source and alignment pass;
the original capture verdict remains under **Original capture details**. A saved
review for another speaker never replaces a selection when its worker finishes.

## Display state and live state

Loading runs on a background worker and reads the saved reports as they were written.
It does not reanalyze PCM, generate new analysis files, replace an original verdict,
start the microphone meter, play sound, resume an interrupted capture, or apply a
profile. **Reanalyze this take** remains an explicit separate operation.

A cached speaker layout is labeled **Saved speaker layout** while disconnected.
Device enumeration and renderer discovery then refresh live state independently.
A historical take stays accessible if settings or renderer revision changed, and
is labeled as a previous measurement. It does not become current calibration evidence.

Alignment reload uses the existing validation path and retains applied, verified,
restored and uncertain-application state. Apply/Restore/Verify remain explicit,
revision-checked actions. Connecting records whether the renderer has the session's
original or corrected settings, whatever its port or process; see
`gain-delay-alignment.md`.

## Bounds and validation

- Settings/index file: 1 MiB, at most 100 take references and 512 cached outputs.
- Display report: 32 MiB; cumulative take/review report budget: 256 MiB, newest first.
  Dense waveform reports can exceed 8 MiB. The core observation reader retains its
  separate 8 MiB limit; increasing the display limit does not relax correction checks.
- Saved-analysis discovery: 512 entries per known take folder and a separate
  256 MiB cumulative read budget. Only small identity/quality summaries stay in
  the index; plots load on selection in a worker. Invalid latest reports produce
  a warning instead of silently falling back to an older analysis.
- First-import discovery: at most 4096 directory entries, four levels, only known
  session directory prefixes, with no directory-symlink traversal.
- Result references update on completed controller events; large report arrays are
  not serialized, copied or written on every UI frame.

Tests cover immediate exit/restart, read-only loading, accepted/rejected/cancelled
verdicts, missing/corrupt entries, first-import directory shapes, reports above the
old limit, graph/result selection with an offline renderer, and alignment application
state without repeated mutation. Audio and network operations are never recovered
from serialized job state.
