//! 16-bit mono PCM WAV files: incremental, crash-tolerant writing, reading,
//! header repair after a crash, and mixing two tracks.

use std::fs::{File, OpenOptions};
use std::io::{self, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;

pub const HEADER_LEN: u64 = 44;
const BYTES_PER_SAMPLE: u64 = 2;

fn header(sample_rate: u32, data_len: u32) -> [u8; 44] {
    let mut out = [0u8; 44];
    let byte_rate = sample_rate * BYTES_PER_SAMPLE as u32;
    out[0..4].copy_from_slice(b"RIFF");
    out[4..8].copy_from_slice(&(36u32.saturating_add(data_len)).to_le_bytes());
    out[8..16].copy_from_slice(b"WAVEfmt ");
    out[16..20].copy_from_slice(&16u32.to_le_bytes());
    out[20..22].copy_from_slice(&1u16.to_le_bytes()); // PCM
    out[22..24].copy_from_slice(&1u16.to_le_bytes()); // mono
    out[24..28].copy_from_slice(&sample_rate.to_le_bytes());
    out[28..32].copy_from_slice(&byte_rate.to_le_bytes());
    out[32..34].copy_from_slice(&(BYTES_PER_SAMPLE as u16).to_le_bytes());
    out[34..36].copy_from_slice(&16u16.to_le_bytes());
    out[36..40].copy_from_slice(b"data");
    out[40..44].copy_from_slice(&data_len.to_le_bytes());
    out
}

/// A complete in-memory WAV file (used for upload requests).
pub fn wav_bytes(samples: &[i16], sample_rate: u32) -> Vec<u8> {
    let data_len = (samples.len() as u64 * BYTES_PER_SAMPLE).min(u32::MAX as u64) as u32;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(&header(sample_rate, data_len));
    for s in samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

/// Appends samples to a WAV file. The header is rewritten by `sync_header`
/// (call it periodically) and `finalize`; between syncs a crash leaves a
/// header that `repair_header` fixes from the file length.
pub struct WavWriter {
    out: BufWriter<File>,
    sample_rate: u32,
    samples: u64,
}

impl WavWriter {
    pub fn create(path: &Path, sample_rate: u32) -> io::Result<Self> {
        let mut file = File::create(path)?;
        file.write_all(&header(sample_rate, 0))?;
        Ok(Self {
            out: BufWriter::with_capacity(64 * 1024, file),
            sample_rate,
            samples: 0,
        })
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn samples_written(&self) -> u64 {
        self.samples
    }

    pub fn write_samples(&mut self, samples: &[i16]) -> io::Result<()> {
        let mut bytes = Vec::with_capacity(samples.len() * 2);
        for s in samples {
            bytes.extend_from_slice(&s.to_le_bytes());
        }
        self.out.write_all(&bytes)?;
        self.samples += samples.len() as u64;
        Ok(())
    }

    pub fn write_silence(&mut self, count: u64) -> io::Result<()> {
        const BLOCK: usize = 4096;
        let zeros = [0i16; BLOCK];
        let mut left = count;
        while left > 0 {
            let n = left.min(BLOCK as u64) as usize;
            self.write_samples(&zeros[..n])?;
            left -= n as u64;
        }
        Ok(())
    }

    /// Flush buffered audio and rewrite the header sizes. With `durable`, also
    /// fsync so the data survives a power loss, not just a process crash.
    pub fn sync_header(&mut self, durable: bool) -> io::Result<()> {
        self.out.flush()?;
        let data_len = (self.samples * BYTES_PER_SAMPLE).min(u32::MAX as u64 - 36) as u32;
        let file = self.out.get_mut();
        file.seek(SeekFrom::Start(0))?;
        file.write_all(&header(self.sample_rate, data_len))?;
        file.seek(SeekFrom::End(0))?;
        if durable {
            file.sync_data()?;
        }
        Ok(())
    }

    /// Write the final header and close. Returns the number of samples.
    pub fn finalize(mut self) -> io::Result<u64> {
        self.sync_header(true)?;
        Ok(self.samples)
    }
}

/// Samples of silence to insert before `incoming` new samples so the track
/// stays aligned with the recording clock. A track that delivered nothing for
/// a while (device hiccup, a tap that only runs while audio plays, a late
/// start) is padded; jitter below `tolerance` is ignored.
pub fn silence_to_insert(written: u64, incoming: u64, expected_total: u64, tolerance: u64) -> u64 {
    let after = written + incoming;
    if expected_total > after + tolerance {
        expected_total - after
    } else {
        0
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct WavData {
    pub sample_rate: u32,
    pub samples: Vec<i16>,
}

impl WavData {
    pub fn duration_ms(&self) -> u64 {
        if self.sample_rate == 0 {
            return 0;
        }
        self.samples.len() as u64 * 1000 / self.sample_rate as u64
    }
}

/// Read a 16-bit PCM mono WAV. Tolerates a stale header (crash before the
/// final sync): the data chunk runs to the end of the file in that case.
pub fn read_wav(path: &Path) -> io::Result<WavData> {
    let mut bytes = Vec::new();
    File::open(path)?.read_to_end(&mut bytes)?;
    parse_wav(&bytes)
}

pub fn parse_wav(bytes: &[u8]) -> io::Result<WavData> {
    let invalid = |msg: &str| io::Error::new(io::ErrorKind::InvalidData, msg.to_string());
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(invalid("not a WAV file"));
    }
    let mut at = 12usize;
    let mut sample_rate = 0u32;
    let mut format_ok = false;
    while at + 8 <= bytes.len() {
        let id = &bytes[at..at + 4];
        let len = u32::from_le_bytes(bytes[at + 4..at + 8].try_into().unwrap()) as usize;
        let body = at + 8;
        if id == b"fmt " {
            if body + 16 > bytes.len() {
                return Err(invalid("truncated fmt chunk"));
            }
            let format = u16::from_le_bytes(bytes[body..body + 2].try_into().unwrap());
            let channels = u16::from_le_bytes(bytes[body + 2..body + 4].try_into().unwrap());
            sample_rate = u32::from_le_bytes(bytes[body + 4..body + 8].try_into().unwrap());
            let bits = u16::from_le_bytes(bytes[body + 14..body + 16].try_into().unwrap());
            format_ok = format == 1 && channels == 1 && bits == 16;
        } else if id == b"data" {
            if !format_ok || sample_rate == 0 {
                return Err(invalid("only 16-bit mono PCM WAV is supported"));
            }
            let available = bytes.len() - body;
            // len == 0 or past EOF: header was never finalized; take the rest.
            let data_len = if len == 0 || len > available {
                available
            } else {
                len
            };
            let data_len = data_len - data_len % 2;
            let samples = bytes[body..body + data_len]
                .as_chunks::<2>()
                .0
                .iter()
                .map(|&c| i16::from_le_bytes(c))
                .collect();
            return Ok(WavData {
                sample_rate,
                samples,
            });
        }
        at = body + len + len % 2;
    }
    Err(invalid("WAV file has no data chunk"))
}

/// Fix the size fields of a WAV written by `WavWriter` from the actual file
/// length. Returns the number of samples. Used to recover after a crash.
pub fn repair_header(path: &Path) -> io::Result<u64> {
    let mut file = OpenOptions::new().read(true).write(true).open(path)?;
    let len = file.metadata()?.len();
    if len < HEADER_LEN {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "WAV file is shorter than its header",
        ));
    }
    let mut head = [0u8; 44];
    file.read_exact(&mut head)?;
    if &head[0..4] != b"RIFF" || &head[36..40] != b"data" {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "not a Parley WAV file",
        ));
    }
    let sample_rate = u32::from_le_bytes(head[24..28].try_into().unwrap());
    let mut data_len = len - HEADER_LEN;
    if data_len % 2 == 1 {
        data_len -= 1;
        file.set_len(HEADER_LEN + data_len)?;
    }
    let data_len32 = data_len.min(u32::MAX as u64 - 36) as u32;
    file.seek(SeekFrom::Start(0))?;
    file.write_all(&header(sample_rate, data_len32))?;
    file.sync_data()?;
    Ok(data_len / BYTES_PER_SAMPLE)
}

/// Sum two tracks sample by sample (clamped). The result is as long as the
/// longer track.
pub fn mix(a: &[i16], b: &[i16]) -> Vec<i16> {
    let len = a.len().max(b.len());
    (0..len)
        .map(|i| {
            let s = *a.get(i).unwrap_or(&0) as i32 + *b.get(i).unwrap_or(&0) as i32;
            s.clamp(i16::MIN as i32, i16::MAX as i32) as i16
        })
        .collect()
}

/// Write a whole WAV file atomically (temp file + rename).
pub fn write_wav(path: &Path, samples: &[i16], sample_rate: u32) -> io::Result<()> {
    let tmp = path.with_extension("wav.tmp");
    std::fs::write(&tmp, wav_bytes(samples, sample_rate))?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wav_bytes_round_trip() {
        let samples = vec![0i16, 1, -1, i16::MAX, i16::MIN];
        let bytes = wav_bytes(&samples, 16_000);
        assert_eq!(bytes.len(), 44 + 10);
        assert_eq!(
            u32::from_le_bytes(bytes[24..28].try_into().unwrap()),
            16_000
        );
        assert_eq!(
            u32::from_le_bytes(bytes[28..32].try_into().unwrap()),
            32_000
        );
        let parsed = parse_wav(&bytes).unwrap();
        assert_eq!(parsed.sample_rate, 16_000);
        assert_eq!(parsed.samples, samples);
    }

    #[test]
    fn writer_produces_a_valid_file_and_counts_samples() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.wav");
        let mut w = WavWriter::create(&path, 16_000).unwrap();
        w.write_samples(&[1, 2, 3]).unwrap();
        w.write_silence(5000).unwrap();
        w.write_samples(&[4]).unwrap();
        assert_eq!(w.samples_written(), 5004);
        assert_eq!(w.finalize().unwrap(), 5004);
        let data = read_wav(&path).unwrap();
        assert_eq!(data.samples.len(), 5004);
        assert_eq!(&data.samples[..3], &[1, 2, 3]);
        assert_eq!(data.samples[5003], 4);
        assert_eq!(data.duration_ms(), 312);
    }

    #[test]
    fn synced_header_is_readable_while_still_recording() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.wav");
        let mut w = WavWriter::create(&path, 16_000).unwrap();
        w.write_samples(&[7; 100]).unwrap();
        w.sync_header(false).unwrap();
        w.write_samples(&[8; 50]).unwrap(); // buffered, not yet synced
        let data = read_wav(&path).unwrap();
        assert_eq!(data.samples.len(), 100);
        drop(w);
    }

    #[test]
    fn crashed_file_is_read_and_repaired_from_its_length() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.wav");
        {
            let mut w = WavWriter::create(&path, 16_000).unwrap();
            w.write_samples(&[3; 1000]).unwrap();
            w.out.flush().unwrap(); // data on disk, header still says 0
            std::mem::forget(w); // simulate a crash: no finalize
        }
        // Header claims 0 bytes of data; the reader takes the rest of the file.
        assert_eq!(read_wav(&path).unwrap().samples.len(), 1000);
        // A torn final sample is trimmed.
        let mut f = OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(&[9]).unwrap();
        drop(f);
        assert_eq!(repair_header(&path).unwrap(), 1000);
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(u32::from_le_bytes(bytes[40..44].try_into().unwrap()), 2000);
        assert_eq!(u32::from_le_bytes(bytes[4..8].try_into().unwrap()), 2036);
        assert_eq!(read_wav(&path).unwrap().samples, vec![3; 1000]);
    }

    #[test]
    fn repair_rejects_foreign_or_short_files() {
        let dir = tempfile::tempdir().unwrap();
        let short = dir.path().join("short.wav");
        std::fs::write(&short, b"RIFF").unwrap();
        assert!(repair_header(&short).is_err());
        let foreign = dir.path().join("foreign.wav");
        std::fs::write(&foreign, vec![0u8; 100]).unwrap();
        assert!(repair_header(&foreign).is_err());
        assert!(repair_header(&dir.path().join("missing.wav")).is_err());
    }

    #[test]
    fn parse_rejects_non_wav_and_unsupported_formats() {
        assert!(parse_wav(b"hello").is_err());
        let mut stereo = wav_bytes(&[0, 0], 16_000);
        stereo[22] = 2;
        assert!(parse_wav(&stereo).is_err());
        let mut no_data = wav_bytes(&[], 16_000);
        no_data.truncate(36);
        assert!(parse_wav(&no_data).is_err());
    }

    #[test]
    fn parse_skips_unknown_chunks() {
        let base = wav_bytes(&[5, 6], 8_000);
        let mut bytes = base[..36].to_vec();
        bytes.extend_from_slice(b"JUNK");
        bytes.extend_from_slice(&3u32.to_le_bytes());
        bytes.extend_from_slice(&[0, 0, 0, 0]); // 3 bytes + pad
        bytes.extend_from_slice(&base[36..]);
        let parsed = parse_wav(&bytes).unwrap();
        assert_eq!(parsed.samples, vec![5, 6]);
        assert_eq!(parsed.sample_rate, 8_000);
    }

    #[test]
    fn silence_is_inserted_only_beyond_tolerance() {
        // In sync.
        assert_eq!(silence_to_insert(16_000, 1600, 17_600, 8000), 0);
        // Small jitter is ignored.
        assert_eq!(silence_to_insert(16_000, 1600, 20_000, 8000), 0);
        // A late start / long gap is padded up to the clock.
        assert_eq!(silence_to_insert(0, 1600, 33_600, 8000), 32_000);
        // A track running ahead is never trimmed.
        assert_eq!(silence_to_insert(40_000, 1600, 17_600, 8000), 0);
    }

    #[test]
    fn mix_sums_and_clamps_to_the_longer_track() {
        assert_eq!(mix(&[1, 2, 3], &[10]), vec![11, 2, 3]);
        assert_eq!(mix(&[i16::MAX], &[100]), vec![i16::MAX]);
        assert_eq!(mix(&[i16::MIN], &[-100]), vec![i16::MIN]);
        assert!(mix(&[], &[]).is_empty());
    }

    #[test]
    fn write_wav_is_atomic_and_readable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mixed.wav");
        write_wav(&path, &[1, 2], 16_000).unwrap();
        assert!(!dir.path().join("mixed.wav.tmp").exists());
        assert_eq!(read_wav(&path).unwrap().samples, vec![1, 2]);
    }
}
