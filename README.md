
<p align="center">
  <img src="https://codeberg.org/racf/source/raw/branch/main/assets/logo.png" width="480" alt="RACF">
</p>

<div align="center">

# RACF — Readable Audio Code Factor
### MIDI is great, but it needs more

***I’m fed up with using software to compose music... well, what if I created an SVG, but for audio?***

</div>

---

RACF is an audio format that applies the philosophy of SVG to sound: a score is
a readable, declarative text document that describes instruments, notes, timing,
mixing, and channel routing. `racf-player` turns that document into audio.

The codebase is split into three crates so the format, the audio backend, and
the tooling can evolve independently.

## Workspace layout

```text
crates/
  racf-core/     format model, XML parser, pitch/duration math,
                 channel layouts, instrument definitions, metadata,
                 lyrics, embedded audio, timeline compiler
  racf-player/   DSP engines, realtime mixer, cpal output, MPRIS, CLI
  racf-utils/    MP3/WAV/FLAC → .racf conversion, inspection, validation
```

`racf-core` has no dependency on an audio device. It answers **what should
sound, when, on which channel, and with what parameters**. `racf-player`
answers **how to synthesize and route it**. `racf-utils` answers **how to get
audio and data in and out of the format**.

### Core pipeline

```text
.racf XML
   │  racf_core::parse_document
   ▼
Document { tempo, layout, metadata, lyrics, imports, instruments, tracks, audio_clips }
   │  racf_core::compile_timeline
   ▼
Timeline { ScheduledEvent { timestamp_ms, action } }
   │  racf_player::play
   ▼
Voice / SampleVoice → MasterMixer → device channels
```

The compiler resolves `import::` presets, expands sequences/beats/chords, applies
track gain, `force`, fades, note velocity, tempo changes, and `targetDuration`,
then produces absolute-time `Play(NoteEvent)` events plus one `PlayAudio` event
per embedded clip. The player constructs one voice per note (or sample clip) and
mixes them in realtime.

## Running

```sh
cargo run -p racf-player -- <file.racf>
cargo run -p racf-player -- --dry-run <file.racf>
cargo run -p racf-player -- --no-mpris <file.racf>
cargo run -p racf-player -- --volume 80 --eq bass --loop <file.racf>
```

`--dry-run` parses and compiles the document, prints a tree report plus a
timeline preview, and does not open an audio device. This is useful for
validating documents in CI or on machines without audio hardware. `--no-mpris`
disables the D-Bus service; setting `RACF_NO_MPRIS=1` has the same effect.

Playback starts at `--volume 100`; `--eq` accepts `flat`, `bass`, `vocal`,
`rock`, or `clarity`, and `--loop` repeats the timeline until stopped. The
interactive status line also accepts commands (`:` opens the prompt) for
volume, seeking, pause/resume, EQ bands, looping, and status.

The loading path for embedded audio is optimized for large documents.
Resampling uses precomputed polyphase Lanczos kernels and renders output frames
in parallel, so multi-megabyte MP3-derived RACF files load in roughly a second
instead of several.

## Metadata

A document can describe itself. All fields are optional and live in a single
`<metadata>` element:

```xml
<metadata>
    <title>Night Drive</title>
    <description>A short synth sketch.</description>
    <artist>Andres</artist>
    <album>Sketches</album>
</metadata>
```

Attributes work as a compact alternative and as a fallback when a child element
is absent:

```xml
<metadata title="Night Drive" artist="Andres" album="Sketches" />
```

`racf-player` reports these fields in a tree-style summary and publishes them
over MPRIS. `racf-utils` copies ID3/Vorbis tags from the source file into the
generated document.

## Lyrics

A document can carry lyrics as plain text, LRC, or SubRip (SRT). Put a
`<lyrics>` element at the top level or inside `<metadata>`:

```xml
<lyrics format="lrc">
[ar:Andres]
[ti:Night Drive]
[00:01.50]First line
[00:05.00][00:09.25]Repeated line
</lyrics>
```

`format` may be `native`, `lrc`, or `srt`. Omitting it defaults to RACF's native
format, which is one line per entry and also accepts LRC-style timestamps.
Timed lyrics follow playback: `racf-player` highlights the active line below the
`[RACF] playing` status line. Untimed lyrics are shown as a static block.

LRC support includes multiple timestamps on one line (`[00:05.00][00:09.25]`),
`[mm:ss.xx]` and `[hh:mm:ss.xx]` timestamps, header tags such as `[ar:…]`,
`[ti:…]`, and `[al:…]`, and `[offset:…]`. SRT cues use the usual
`HH:MM:SS,mmm --> HH:MM:SS,mmm` timing.

## Embedded audio

RACF can carry sampled audio inside the document, so a converted file remains a
single portable artifact. Samples are base64-encoded inside `<audio>`:

```xml
<audio>
    <clip id="main" at="0ms" gain="1.0" pan="0.0"
          sampleRate="8000" channels="1" encoding="mulaw"
          duration="4.05s">
        <data>
            …base64…
        </data>
    </clip>
</audio>
```

`<clip>` attributes:

| Attribute | Meaning |
| --- | --- |
| `id` | Clip name, used in diagnostics (defaults to `clip-N`) |
| `at` / `offset` | Start time: `250ms`, `1.5s`, `mm:ss`, or seconds |
| `gain` | Linear gain (default `1.0`) |
| `pan` | Equal-power front-left/front-right position (default `0.0`) |
| `channel` | Optional exact channel role, same names as note routing |
| `sampleRate` / `rate` | Sample rate in Hz (default `44100`) |
| `channels` | 1–8 interleaved channels (default `1`) |
| `encoding` | `mulaw` (default), `pcm8`, or `pcm16` |
| `duration` | Informational; the real duration is derived from the data |

Mono clips follow `channel`, then `pan`, then the layout's front pair. Clips
with more channels map source channels onto the layout starting at `channel`
when given, otherwise at the front pair. Preparing a clip decodes it and
resamples it to the output device rate when the document is loaded.

## MPRIS

On Linux, `racf-player` publishes a status-only MPRIS2 service on the session bus
under `org.mpris.MediaPlayer2.racf`:

- `PlaybackStatus`: `Playing` while audio plays, `Stopped` afterwards;
- `Metadata`: title, description (`xesam:comment`), artist, album, and length;
- `Position`: elapsed playback time, updated twice per second;
- `CanControl` is true, but every transport flag (`CanPlay`, `CanPause`,
  `CanSeek`, `CanGoNext`, `CanGoPrevious`) is false — the player is a one-shot
  CLI tool.

Verify it with:

```sh
playerctl -p racf status
gdbus call --session --dest org.mpris.MediaPlayer2.racf \
    --object-path /org/mpris/MediaPlayer2 \
    --method org.freedesktop.DBus.Properties.Get \
    org.mpris.MediaPlayer2.Player Metadata
```

MPRIS is best-effort: if the session bus is unreachable, playback continues and
a single diagnostic is printed.

## Utilities

`racf-utils` converts audio and works with existing documents.

```sh
# MP3 → .racf with embedded audio (the default output is input.racf)
cargo run -p racf-utils -- convert song.mp3

# Highest quality: source rate/channels as 16-bit PCM
cargo run -p racf-utils -- convert song.flac --quality raw

# Explicit overrides on top of a profile
cargo run -p racf-utils -- convert song.mp3 -o song.racf \
    --quality lossy --sample-rate 16000 --channels 2 --encoding pcm16

# Metadata overrides (fall back to tags, then the input filename)
cargo run -p racf-utils -- convert song.mp3 --title "Night Drive" --artist Andres

# Import lyrics explicitly, force a format, or skip embedded/sidecar lyrics
cargo run -p racf-utils -- convert song.mp3 --lyrics song.lrc
cargo run -p racf-utils -- convert song.mp3 --lyrics words.txt --lyrics-format native
cargo run -p racf-utils -- convert song.mp3 --no-lyrics

# Inspect or validate existing documents
cargo run -p racf-utils -- inspect file.racf
cargo run -p racf-utils -- validate one.racf two.racf
```

### Output quality

`convert` has two profiles:

| Profile | Sample rate | Channels | Encoding | Typical use |
| --- | --- | --- | --- | --- |
| `lossy` (default) | 22,050 Hz | mono | µ-law | small files, speech, casual listening |
| `raw` | source rate | source count, clamped to 1–2 | PCM16 | highest fidelity |

`--sample-rate`, `--channels`, and `--encoding` override the selected profile.
Lossy output is roughly 22 KiB of audio data per second before base64 overhead.
Raw output multiplies that by the source rate, channel count, and two-byte
samples; a naive full-rate PCM dump is what turns a short song into a 200 MB
document.

To keep accidental bloat out of the repository, conversion refuses to write a
file larger than 64 MiB. Use `--max-size-mb N` to change the limit (`0` disables
it) or `--allow-large` to write anyway. The error message suggests the usual
fixes: lower `--sample-rate`, force `--channels 1`, or pick `--encoding mulaw`.

### Lyrics import

When converting, lyrics come from the first available source:

1. an explicit `--lyrics FILE` (the format is detected from `.lrc`/`.srt` or the
   content unless `--lyrics-format` overrides it);
2. an embedded lyrics tag (`USLT`/`SYLT`, Vorbis `LYRICS`, …);
3. a sidecar `.lrc` or `.srt` file next to the input.

`--no-lyrics` skips the embedded and sidecar sources. LRC header tags `ti`,
`ar`, and `al` fill missing title, artist, and album fields during conversion.

Supported source formats are MP3, WAV, and FLAC (the codecs enabled for
Symphonia). Tags are read from the probe metadata revision; `TXXX:comment`
frames are recognized as descriptions. Set `RACF_DEBUG_TAGS=1` to print the
parsed tags.

## Channel routing

Channels are named roles, not raw indices. A document keeps its meaning when it
is played on a device with a different channel count.

### Layouts

Set a preset on the root element:

```xml
<racf version="1.0" channelLayout="stereo">
```

Supported presets: `mono`, `stereo`, `quad`, `5.1`, `7.1` (numeric aliases such
as `1`, `2`, `4`, `6`, and `8` also work).

For an explicit layout, list the channels in output order. This overrides
`channelLayout`:

```xml
<racf version="1.0">
    <channels>
        <channel role="front-left" />
        <channel role="front-right" />
        <channel role="front-center" />
        <channel role="low-frequency" />
        <channel role="rear-left" />
        <channel role="rear-right" />
    </channels>
</racf>
```

### Named roles

| Role | Aliases |
| --- | --- |
| `front-left` | `left`, `l`, `fl` |
| `front-right` | `right`, `r`, `fr` |
| `front-center` | `center`, `centre`, `c`, `fc`, `mono` |
| `low-frequency` | `lfe`, `sub`, `subwoofer`, `bass` |
| `rear-left` | `back-left`, `rl`, `bl` |
| `rear-right` | `back-right`, `rr`, `br` |
| `side-left` | `sl` |
| `side-right` | `sr` |

Matching is case-insensitive and treats `-`, `_`, and spaces as equivalent.

### Routing attributes

`pan` is still the normal equal-power front-left/front-right control. Add a
`channel` attribute to route exactly instead:

```xml
<track instrument="Lead" pan="-0.6" gain="0.8">
    <!-- Exact channel: bypasses pan -->
    <note pitch="C5" duration="1/4" channel="front-left" />

    <!-- This note overrides the track channel -->
    <lyric pitch="A4" vowel="AH" duration="1/2" channel="front-center" />
</track>

<!-- Whole-track routing -->
<track instrument="Bass" channel="low-frequency" gain="0.7">
    <sequence pattern="C2:1/1, F2:1/1" />
</track>
```

`channel` is accepted on `<track>`, `<note>`, `<chord>`, `<lyric>`,
`<sequence>`, and `<beat>`. Precedence is:

```text
note/item channel  >  track channel  >  pan  >  layout front pair
```

If a requested role is not present in the document layout, the player falls
back to the pan position. The mixer renders into the document layout, then
downmixes to the real device:

- same channel count → direct copy;
- stereo device → folds center, LFE, and surrounds into L/R;
- mono device → weighted average of the bus;
- mono document → duplicated to stereo when needed;
- larger device → extra channels stay silent.

## Document elements compiled today

- Root: `tempo`, `timeSignature`, `bitDepth`, `bitrate`, `sampleRate`/`kHz`,
  `targetDuration`, `channelLayout`
- `<metadata>` / `<title>` / `<description>` / `<artist>` / `<album>`
- `<lyrics format="native|lrc|srt">` with plain, LRC, or SRT text
- `<audio>` / `<clip>` with `<data>` base64 payload
- `<imports>` / `<import src="import::..." alias="...">`
- `<definitions>` / `<instrument>` with `oscillator`, distortion `effect`,
  vocal parameters, and engine `type`
- `<track>` with `instrument`, `gain`, `pan`, `channel`
- Track items: `note`, `lyric`, `chord`, `rest`, `sequence`, `beat`, `tempo`,
  `force`/`gain`, `fade-in`, `fade-out`, `separation`

A channel-aware sample document with metadata shows both features together: run
it with `--dry-run` to see the published fields and the per-track routing.

## Development

```sh
cargo test --workspace
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo check --workspace
```

MPRIS integration is compiled only on Linux. On other platforms the player
builds and plays normally without the D-Bus service.

## Contrib
<table>
  <tbody>
    <tr>
      <td align="center" valign="top" width="14.28%"><img src="https://codeberg.org/avatars/6497ffe1597f5fb6406f06a4bb716218ddfbb5c99dd1fee3d48b2a947ea8be59" width="100px;" alt="PascalX"/><br /><sub><b>pascalX</b></sub></a><br /><a href="https://codeberg.org/pascalX" title="Code">💻</a></td>
    </tr>
  </tbody>
</table>
