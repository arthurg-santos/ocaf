//! Input detection and external decoding.
//!
//! Uncompressed WAV files are read directly by `ocaf-core`. Every other
//! format is decoded to raw PCM by an installed FFmpeg and streamed into the
//! OCAF encoder, so no temporary WAV file is created.

use crate::commands::find_program;
use crate::format::bytes;
use ocaf_core::wav::WavReader;
use ocaf_core::{
    encode_pcm, encode_wav, EncodeOptions, EncodeSummary, Error, SampleFormat, StreamInfo,
};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

type InputResult<T> = Result<T, String>;

/// How the input file will be converted to PCM.
pub(crate) enum Source {
    /// A WAV file parsed by `ocaf-core`.
    Wav,
    /// A file decoded by FFmpeg.
    External(External),
}

impl Source {
    pub fn detect(
        input: &Path,
        requested: Option<PcmFormat>,
        ffmpeg: Option<&Path>,
        ffprobe: Option<&Path>,
    ) -> InputResult<Self> {
        let mut file =
            File::open(input).map_err(|e| format!("cannot open {}: {e}", input.display()))?;
        let mut magic = [0u8; 12];
        let read = read_prefix(&mut file, &mut magic)?;
        let is_wave = read == magic.len()
            && matches!(&magic[0..4], b"RIFF" | b"RF64" | b"BW64")
            && &magic[8..12] == b"WAVE";
        if is_wave {
            match WavReader::open(
                File::open(input).map_err(|e| format!("cannot open {}: {e}", input.display()))?,
            ) {
                Ok(_) => {
                    if let Some(format) = requested {
                        return Err(format!(
                            "--pcm-format {} only applies to non-WAV inputs; \
                             WAV files keep their native sample format",
                            format.name()
                        ));
                    }
                    return Ok(Source::Wav);
                }
                // Compressed WAV variants and RF64 are rejected by the native
                // reader but FFmpeg can decode them.
                Err(Error::Unsupported(_)) => {}
                Err(error) => return Err(error.to_string()),
            }
        }
        external(input, requested, ffmpeg, ffprobe)
    }

    pub fn encode(
        &self,
        input: &Path,
        output: &mut File,
        options: &EncodeOptions,
    ) -> InputResult<EncodeSummary> {
        match self {
            Source::Wav => {
                let file = File::open(input)
                    .map_err(|e| format!("cannot open {}: {e}", input.display()))?;
                encode_wav(file, output, options).map_err(|e| e.to_string())
            }
            Source::External(external) => external.encode(input, output, options),
        }
    }

    /// Codec name reported by FFprobe, used for provenance metadata.
    pub fn codec(&self) -> Option<&str> {
        match self {
            Source::Wav => None,
            Source::External(external) => Some(&external.codec),
        }
    }

    /// Extra status line for inputs that were decoded by FFmpeg.
    pub fn source_line(&self, input: &Path) -> Option<String> {
        let external = match self {
            Source::Wav => return None,
            Source::External(external) => external,
        };
        let size = fs::metadata(input)
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        Some(format!(
            "{} ({}), decoded by ffmpeg",
            external.codec,
            bytes(size)
        ))
    }

    /// Honest notes about what a lossless PCM codec can and cannot preserve.
    pub fn notes(&self) -> Vec<String> {
        let external = match self {
            Source::Wav => return Vec::new(),
            Source::External(external) => external,
        };
        let mut notes = Vec::new();
        if external.chosen.reduces_precision_from(external.natural) {
            notes.push(format!(
                "converting the decoder output from {} to {} is lossy",
                external.natural.name(),
                external.chosen.name()
            ));
        }
        if external.chosen.is_float() {
            notes.push(
                "the decoded float PCM is preserved bit-exactly; use --pcm-format s16 \
                 for a smaller archive"
                    .to_string(),
            );
        }
        notes.push(format!(
            "OCAF stores the decoded PCM, not the original {} bitstream",
            external.codec
        ));
        notes
    }
}

pub(crate) struct External {
    info: StreamInfo,
    ffmpeg: PathBuf,
    codec: String,
    natural: PcmFormat,
    chosen: PcmFormat,
}

impl External {
    fn encode(
        &self,
        input: &Path,
        output: &mut File,
        options: &EncodeOptions,
    ) -> InputResult<EncodeSummary> {
        let mut command = Command::new(&self.ffmpeg);
        command
            .arg("-nostdin")
            .arg("-v")
            .arg("error")
            .arg("-i")
            .arg(input)
            .arg("-map")
            .arg("0:a:0")
            .arg("-vn")
            .arg("-sn")
            .arg("-dn")
            .arg("-c:a")
            .arg(self.chosen.encoder())
            .arg("-f")
            .arg(self.chosen.muxer())
            .arg("pipe:1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        let mut child = command.spawn().map_err(|e| {
            format!(
                "cannot run {} for {}: {e}",
                self.ffmpeg.display(),
                input.display()
            )
        })?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "ffmpeg did not provide a PCM pipe".to_string())?;

        let result = encode_pcm(stdout, self.info, output, options);
        match result {
            Ok(summary) => {
                let status = child
                    .wait()
                    .map_err(|e| format!("cannot wait for ffmpeg: {e}"))?;
                if !status.success() {
                    return Err(format!(
                        "ffmpeg failed while decoding {} ({status})",
                        input.display()
                    ));
                }
                Ok(summary)
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                Err(error.to_string())
            }
        }
    }
}

/// PCM format written into the OCAF file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PcmFormat {
    S16,
    S24,
    S32,
    F32,
    F64,
}

impl PcmFormat {
    pub fn parse(value: &str) -> InputResult<Self> {
        match value.to_ascii_lowercase().as_str() {
            "s16" | "i16" | "16" => Ok(PcmFormat::S16),
            "s24" | "i24" | "24" => Ok(PcmFormat::S24),
            "s32" | "i32" | "32" => Ok(PcmFormat::S32),
            "f32" | "float" | "float32" => Ok(PcmFormat::F32),
            "f64" | "double" | "float64" => Ok(PcmFormat::F64),
            other => Err(format!(
                "unknown PCM format {other:?} (use auto, s16, s24, s32, f32 or f64)"
            )),
        }
    }

    fn from_probe(sample_fmt: &str, bits: Option<u32>) -> InputResult<Self> {
        match sample_fmt {
            "s16" | "s16p" => Ok(PcmFormat::S16),
            "s32" | "s32p" => Ok(match bits {
                Some(bits) if bits <= 16 => PcmFormat::S16,
                Some(bits) if bits <= 24 => PcmFormat::S24,
                _ => PcmFormat::S32,
            }),
            "flt" | "fltp" => Ok(PcmFormat::F32),
            "dbl" | "dblp" => Ok(PcmFormat::F64),
            "u8" | "u8p" => Ok(PcmFormat::S16),
            "s64" | "s64p" => Err(
                "FFmpeg reports 64-bit integer PCM, which OCAF does not support; \
                 pass --pcm-format s32 to convert"
                    .to_string(),
            ),
            other => Err(format!(
                "unsupported FFmpeg sample format {other:?}; pass --pcm-format s16, s24, s32, f32 or f64"
            )),
        }
    }

    pub fn bits(self) -> u16 {
        match self {
            PcmFormat::S16 => 16,
            PcmFormat::S24 => 24,
            PcmFormat::S32 | PcmFormat::F32 => 32,
            PcmFormat::F64 => 64,
        }
    }

    pub fn sample_format(self) -> SampleFormat {
        match self {
            PcmFormat::S16 | PcmFormat::S24 | PcmFormat::S32 => SampleFormat::Int,
            PcmFormat::F32 | PcmFormat::F64 => SampleFormat::Float,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            PcmFormat::S16 => "16-bit integer PCM",
            PcmFormat::S24 => "24-bit integer PCM",
            PcmFormat::S32 => "32-bit integer PCM",
            PcmFormat::F32 => "32-bit float PCM",
            PcmFormat::F64 => "64-bit float PCM",
        }
    }

    fn is_float(self) -> bool {
        matches!(self, PcmFormat::F32 | PcmFormat::F64)
    }

    fn reduces_precision_from(self, natural: PcmFormat) -> bool {
        if self == natural {
            return false;
        }
        matches!(
            (natural, self),
            (PcmFormat::F64, PcmFormat::F32)
                | (
                    PcmFormat::F32 | PcmFormat::F64,
                    PcmFormat::S16 | PcmFormat::S24 | PcmFormat::S32
                )
                | (PcmFormat::S32, PcmFormat::S24 | PcmFormat::S16)
                | (PcmFormat::S24, PcmFormat::S16)
        )
    }

    fn encoder(self) -> &'static str {
        match self {
            PcmFormat::S16 => "pcm_s16le",
            PcmFormat::S24 => "pcm_s24le",
            PcmFormat::S32 => "pcm_s32le",
            PcmFormat::F32 => "pcm_f32le",
            PcmFormat::F64 => "pcm_f64le",
        }
    }

    fn muxer(self) -> &'static str {
        match self {
            PcmFormat::S16 => "s16le",
            PcmFormat::S24 => "s24le",
            PcmFormat::S32 => "s32le",
            PcmFormat::F32 => "f32le",
            PcmFormat::F64 => "f64le",
        }
    }
}

struct Probe {
    codec: String,
    sample_fmt: String,
    sample_rate: u32,
    channels: u16,
    channel_layout: String,
    bits_per_raw_sample: Option<u32>,
    bits_per_sample: Option<u32>,
}

impl Probe {
    fn bits(&self) -> Option<u32> {
        self.bits_per_raw_sample
            .or_else(|| self.bits_per_sample.filter(|bits| *bits > 0))
    }
}

fn external(
    input: &Path,
    requested: Option<PcmFormat>,
    ffmpeg: Option<&Path>,
    ffprobe: Option<&Path>,
) -> InputResult<Source> {
    let ffmpeg = resolve_tool(ffmpeg, "ffmpeg", input)?;
    let ffprobe = resolve_probe(ffprobe, ffmpeg.as_path(), input)?;
    let probe = probe_file(input, &ffprobe)?;
    let natural = PcmFormat::from_probe(&probe.sample_fmt, probe.bits())?;
    let chosen = requested.unwrap_or(natural);
    let info = StreamInfo {
        sample_rate: probe.sample_rate,
        channels: probe.channels,
        bits_per_sample: chosen.bits(),
        sample_format: chosen.sample_format(),
        channel_mask: channel_mask(&probe.channel_layout, probe.channels),
        total_samples: 0,
    };
    info.validate().map_err(|e| e.to_string())?;
    Ok(Source::External(External {
        info,
        ffmpeg,
        codec: probe.codec,
        natural,
        chosen,
    }))
}

fn resolve_tool(requested: Option<&Path>, name: &str, input: &Path) -> InputResult<PathBuf> {
    if let Some(path) = requested {
        return find_program(&path.to_string_lossy())
            .ok_or_else(|| format!("{name} executable {} was not found", path.display()));
    }
    find_program(name).ok_or_else(|| {
        format!(
            "cannot decode {}: {name} was not found in PATH\n\
             install FFmpeg or pass --{name} PATH",
            input.display()
        )
    })
}

fn resolve_probe(requested: Option<&Path>, ffmpeg: &Path, input: &Path) -> InputResult<PathBuf> {
    if let Some(path) = requested {
        return find_program(&path.to_string_lossy())
            .ok_or_else(|| format!("ffprobe executable {} was not found", path.display()));
    }
    if let Some(parent) = ffmpeg.parent() {
        let sibling = parent.join("ffprobe");
        if let Some(found) = find_program(&sibling.to_string_lossy()) {
            return Ok(found);
        }
    }
    find_program("ffprobe").ok_or_else(|| {
        format!(
            "cannot decode {}: ffprobe was not found in PATH\n\
             install FFmpeg or pass --ffprobe PATH",
            input.display()
        )
    })
}

fn probe_file(input: &Path, ffprobe: &Path) -> InputResult<Probe> {
    let output = Command::new(ffprobe)
        .args([
            "-v",
            "error",
            "-select_streams",
            "a:0",
            "-show_entries",
            "stream=codec_name,sample_fmt,sample_rate,channels,channel_layout,\
             bits_per_raw_sample,bits_per_sample",
            "-of",
            "default=noprint_wrappers=1",
        ])
        .arg(input)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run {}: {e}", ffprobe.display()))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stderr = stderr.trim();
        return Err(if stderr.is_empty() {
            format!("{} could not read {}", ffprobe.display(), input.display())
        } else {
            format!(
                "{} could not read {}: {stderr}",
                ffprobe.display(),
                input.display()
            )
        });
    }
    parse_probe(&String::from_utf8_lossy(&output.stdout))
}

fn parse_probe(text: &str) -> InputResult<Probe> {
    let mut fields = HashMap::new();
    for line in text.lines() {
        if let Some((key, value)) = line.split_once('=') {
            fields.insert(key.trim(), value.trim());
        }
    }
    let codec = fields.get("codec_name").copied().unwrap_or_default();
    let sample_fmt = fields.get("sample_fmt").copied().unwrap_or_default();
    let sample_rate = fields
        .get("sample_rate")
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or(0);
    let channels = fields
        .get("channels")
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(0);
    if codec.is_empty() || sample_fmt.is_empty() || sample_rate == 0 || channels == 0 {
        return Err(
            "no decodable audio stream was found; use ffprobe to inspect the file".to_string(),
        );
    }
    Ok(Probe {
        codec: codec.to_string(),
        sample_fmt: sample_fmt.to_string(),
        sample_rate,
        channels,
        channel_layout: fields
            .get("channel_layout")
            .copied()
            .unwrap_or_default()
            .to_string(),
        bits_per_raw_sample: fields
            .get("bits_per_raw_sample")
            .and_then(|value| value.parse::<u32>().ok()),
        bits_per_sample: fields
            .get("bits_per_sample")
            .and_then(|value| value.parse::<u32>().ok()),
    })
}

fn read_prefix(reader: &mut impl Read, buffer: &mut [u8]) -> InputResult<usize> {
    let mut filled = 0;
    while filled < buffer.len() {
        match reader.read(&mut buffer[filled..]) {
            Ok(0) => break,
            Ok(read) => filled += read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(format!("cannot read input: {error}")),
        }
    }
    Ok(filled)
}

fn channel_mask(layout: &str, channels: u16) -> u32 {
    let layout = layout.to_ascii_lowercase();
    let mask = match layout.as_str() {
        "mono" => 0x004,
        "stereo" => 0x003,
        "2.1" => 0x00b,
        "3.0" => 0x007,
        "3.0(back)" => 0x103,
        "4.0" => 0x107,
        "quad" => 0x033,
        "quad(side)" => 0x603,
        "3.1" => 0x00f,
        "5.0" => 0x037,
        "5.0(side)" => 0x607,
        "4.1" => 0x10f,
        "5.1" => 0x03f,
        "5.1(side)" => 0x60f,
        "6.0" => 0x707,
        "6.0(front)" => 0x6c3,
        "3.1.2" => 0x500f,
        "hexagonal" => 0x137,
        "6.1" => 0x70f,
        "6.1(back)" => 0x13f,
        "6.1(front)" => 0x6cb,
        "7.0" => 0x637,
        "7.0(front)" => 0x6c7,
        "7.1" => 0x63f,
        "7.1(wide)" => 0x0ff,
        "7.1(wide-side)" => 0x6cf,
        "5.1.2" => 0x560f,
        "5.1.2(back)" => 0x503f,
        "5.1.4" => 0x2d60f,
        "7.1.2" => 0x563f,
        "7.1.4" => 0x2d63f,
        "octagonal" => 0x737,
        "cube" => 0x2d033,
        "9.1.4" => 0x2d6ff,
        _ => 0,
    };
    if mask != 0 {
        return mask;
    }
    match (layout.as_str(), channels) {
        ("", 1) | ("unknown", 1) => 0x004,
        ("", 2) | ("unknown", 2) => 0x003,
        ("", 6) | ("unknown", 6) => 0x03f,
        ("", 8) | ("unknown", 8) => 0x63f,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::{channel_mask, parse_probe, PcmFormat};

    #[test]
    fn parses_integer_flac_probe_output() {
        let probe = parse_probe(
            "codec_name=flac\nsample_fmt=s32\nsample_rate=96000\nchannels=2\n\
             channel_layout=stereo\nbits_per_raw_sample=24\nbits_per_sample=0\n",
        )
        .unwrap();
        assert_eq!(probe.codec, "flac");
        assert_eq!(probe.sample_rate, 96_000);
        assert_eq!(probe.channels, 2);
        assert_eq!(probe.bits(), Some(24));
        assert_eq!(
            PcmFormat::from_probe(&probe.sample_fmt, probe.bits()).unwrap(),
            PcmFormat::S24
        );
    }

    #[test]
    fn parses_lossy_float_probe_output() {
        let probe = parse_probe(
            "codec_name=mp3\nsample_fmt=fltp\nsample_rate=44100\nchannels=2\n\
             channel_layout=stereo\nbits_per_raw_sample=N/A\nbits_per_sample=0\n",
        )
        .unwrap();
        assert_eq!(
            PcmFormat::from_probe(&probe.sample_fmt, probe.bits()).unwrap(),
            PcmFormat::F32
        );
    }

    #[test]
    fn rejects_probe_output_without_audio() {
        assert!(parse_probe("").is_err());
        assert!(parse_probe("codec_name=\nsample_fmt=\n").is_err());
    }

    #[test]
    fn selects_the_natural_integer_depth() {
        assert_eq!(
            PcmFormat::from_probe("s16", Some(16)).unwrap(),
            PcmFormat::S16
        );
        assert_eq!(
            PcmFormat::from_probe("s32", Some(24)).unwrap(),
            PcmFormat::S24
        );
        assert_eq!(
            PcmFormat::from_probe("s32", Some(32)).unwrap(),
            PcmFormat::S32
        );
        assert_eq!(PcmFormat::from_probe("s32", None).unwrap(), PcmFormat::S32);
        assert_eq!(PcmFormat::from_probe("flt", None).unwrap(), PcmFormat::F32);
        assert_eq!(PcmFormat::from_probe("dbl", None).unwrap(), PcmFormat::F64);
        assert!(PcmFormat::from_probe("s64", None).is_err());
        assert!(PcmFormat::from_probe("mystery", None).is_err());
    }

    #[test]
    fn precision_reduction_is_reported() {
        assert!(PcmFormat::S16.reduces_precision_from(PcmFormat::S24));
        assert!(PcmFormat::S24.reduces_precision_from(PcmFormat::S32));
        assert!(PcmFormat::F32.reduces_precision_from(PcmFormat::F64));
        assert!(PcmFormat::S16.reduces_precision_from(PcmFormat::F32));
        assert!(!PcmFormat::S24.reduces_precision_from(PcmFormat::S16));
        assert!(!PcmFormat::F32.reduces_precision_from(PcmFormat::F32));
        assert!(!PcmFormat::F64.reduces_precision_from(PcmFormat::S32));
    }

    #[test]
    fn maps_standard_channel_layouts_to_wave_masks() {
        assert_eq!(channel_mask("mono", 1), 0x004);
        assert_eq!(channel_mask("stereo", 2), 0x003);
        assert_eq!(channel_mask("5.1", 6), 0x03f);
        assert_eq!(channel_mask("5.1(side)", 6), 0x60f);
        assert_eq!(channel_mask("7.1", 8), 0x63f);
        assert_eq!(channel_mask("unknown", 2), 0x003);
        assert_eq!(channel_mask("unknown", 4), 0);
        assert_eq!(channel_mask("made-up-layout", 2), 0);
    }
}
