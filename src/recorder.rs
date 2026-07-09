
use std::fs::File;
use std::io::{Seek, SeekFrom, Write};
use std::path::Path;

const HEADER_LEN: u64 = 44;

pub struct WavRecorder {
    file: File,
    data_bytes: u32,
}

impl WavRecorder {
    pub fn create(path: &Path, rate: u32) -> std::io::Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut file = File::create(path)?;
        file.write_all(&Self::header(rate, 0))?;
        Ok(Self { file, data_bytes: 0 })
    }

    fn header(rate: u32, data_bytes: u32) -> [u8; 44] {
        let byte_rate = rate * 2;
        let mut h = [0u8; 44];
        h[0..4].copy_from_slice(b"RIFF");
        h[4..8].copy_from_slice(&(36 + data_bytes).to_le_bytes());
        h[8..12].copy_from_slice(b"WAVE");
        h[12..16].copy_from_slice(b"fmt ");
        h[16..20].copy_from_slice(&16u32.to_le_bytes());
        h[20..22].copy_from_slice(&1u16.to_le_bytes());
        h[22..24].copy_from_slice(&1u16.to_le_bytes());
        h[24..28].copy_from_slice(&rate.to_le_bytes());
        h[28..32].copy_from_slice(&byte_rate.to_le_bytes());
        h[32..34].copy_from_slice(&2u16.to_le_bytes());
        h[34..36].copy_from_slice(&16u16.to_le_bytes());
        h[36..40].copy_from_slice(b"data");
        h[40..44].copy_from_slice(&data_bytes.to_le_bytes());
        h
    }

    pub fn write(&mut self, samples: &[f32]) {
        let mut buf: Vec<u8> = Vec::with_capacity(samples.len() * 2);
        for &s in samples {
            let q = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
            buf.extend_from_slice(&q.to_le_bytes());
        }
        if self.file.write_all(&buf).is_ok() {
            self.data_bytes = self.data_bytes.saturating_add(buf.len() as u32);
        }
    }

    fn finalize(&mut self) {
        let riff = 36 + self.data_bytes;
        let _ = self.file.seek(SeekFrom::Start(4));
        let _ = self.file.write_all(&riff.to_le_bytes());
        let _ = self.file.seek(SeekFrom::Start(40));
        let _ = self.file.write_all(&self.data_bytes.to_le_bytes());
        let _ = self.file.seek(SeekFrom::Start(HEADER_LEN));
        let _ = self.file.flush();
    }
}

impl Drop for WavRecorder {
    fn drop(&mut self) {
        self.finalize();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_valid_wav_header_and_sizes() {
        let path = std::env::temp_dir().join("hw-rec-test.wav");
        {
            let mut w = WavRecorder::create(&path, 44100).unwrap();
            w.write(&[0.0, 0.5, -0.5, 1.0, -1.0]);
        }
        let b = std::fs::read(&path).unwrap();
        assert_eq!(&b[0..4], b"RIFF");
        assert_eq!(&b[8..12], b"WAVE");
        assert_eq!(&b[36..40], b"data");
        assert_eq!(u16::from_le_bytes([b[22], b[23]]), 1);
        assert_eq!(u16::from_le_bytes([b[34], b[35]]), 16);
        assert_eq!(u32::from_le_bytes([b[24], b[25], b[26], b[27]]), 44100);
        let data = u32::from_le_bytes([b[40], b[41], b[42], b[43]]);
        let riff = u32::from_le_bytes([b[4], b[5], b[6], b[7]]);
        assert_eq!(data, 10, "размер data-чанка");
        assert_eq!(riff, 46, "RIFF size = 36 + data");
        assert_eq!(b.len() as u32, 44 + data, "файл = заголовок + данные");
        let _ = std::fs::remove_file(&path);
    }
}
