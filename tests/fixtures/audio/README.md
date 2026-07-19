# audio fixtures

- `dance_robot_activate.flac`: a mastered dance track used as a deterministic,
  repeatable drive signal for audio-reactive sketches (Radiance) — see the
  debug-only `WC_AUDIO_FILE` feature (`crates/wc-core/src/audio/input/file_drive.rs`)
  and its decode test. A live mic is unrepeatable across runs and machines;
  this fixture lets beat/onset/band response be exercised against the same
  audio every time.

  - **Title**: Dance Robot, Activate!
  - **Artist**: Loyalty Freak Music
  - **Album**: ROLLER DISCO DANCE DANCE
  - **License**: CC0 1.0 (public domain dedication)
  - **Source**: https://archive.org/details/LoyaltyFreakMusic-ROLLERDISCODANCEDANCE

  Only the FLAC is vendored (~17 MB). A second CC0 track from the same album
  (`cant_stop_my_feet.mp3`) was evaluated but is deliberately NOT vendored —
  `WC_AUDIO_FILE`'s decoder is FLAC/WAV only (`claxon`/`hound`), no MP3
  decoder is in the dependency graph, and one fixture is enough to exercise
  the decode + analysis-drive path.
