use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sherpa_onnx::{OfflineQwen3ASRModelConfig, OfflineRecognizer, OfflineRecognizerConfig, Wave};
use thiserror::Error;

use crate::models::{ModelError, ModelManager};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AsrTranscription {
    pub language: String,
    pub text: String,
}

pub struct SherpaAsrRecognizer {
    recognizer: OfflineRecognizer,
}

impl SherpaAsrRecognizer {
    pub fn load(manager: &ModelManager, model_id: &str, threads: u16) -> Result<Self, AsrError> {
        if !(1..=16).contains(&threads) {
            return Err(AsrError::InvalidThreads(threads));
        }
        let model = manager.installed_model(model_id)?;
        let mut config = OfflineRecognizerConfig::default();
        config.model_config.num_threads = i32::from(threads);
        config.model_config.provider = Some("cpu".to_owned());
        config.feat_config.feature_dim = 128;
        config.model_config.qwen3_asr = OfflineQwen3ASRModelConfig {
            conv_frontend: Some(path_string(&model.conv_frontend)?),
            encoder: Some(path_string(&model.encoder)?),
            decoder: Some(path_string(&model.decoder)?),
            tokenizer: Some(path_string(&model.tokenizer)?),
            max_total_len: 512,
            max_new_tokens: 512,
            temperature: 1e-6,
            top_p: 0.8,
            seed: 42,
            hotwords: None,
        };
        let recognizer = OfflineRecognizer::create(&config).ok_or(AsrError::CreateRecognizer)?;
        Ok(Self { recognizer })
    }

    pub fn transcribe_wav(
        &self,
        wav_path: &Path,
        forced_language: Option<&str>,
    ) -> Result<AsrTranscription, AsrError> {
        let path = path_string(wav_path)?;
        let wave = Wave::read(&path).ok_or_else(|| AsrError::ReadWave(wav_path.to_path_buf()))?;
        let stream = self.recognizer.create_stream();
        let requested_language = forced_language
            .map(str::trim)
            .filter(|item| !item.is_empty());
        if let Some(language) = requested_language {
            stream.set_option("language", language);
        }
        stream.accept_waveform(wave.sample_rate(), wave.samples());
        self.recognizer.decode(&stream);
        let result = stream.get_result().ok_or(AsrError::MissingResult)?;
        Ok(AsrTranscription {
            language: requested_language.unwrap_or_default().to_owned(),
            text: result.text.trim().to_owned(),
        })
    }
}

fn path_string(path: &Path) -> Result<String, AsrError> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| AsrError::NonUnicodePath(path.to_path_buf()))
}

#[derive(Debug, Error)]
pub enum AsrError {
    #[error("sherpa-onnx could not create the ASR recognizer")]
    CreateRecognizer,
    #[error("ASR thread count must be between 1 and 16, received {0}")]
    InvalidThreads(u16),
    #[error("sherpa-onnx returned no recognition result")]
    MissingResult,
    #[error(transparent)]
    Model(#[from] ModelError),
    #[error("model or audio path is not valid Unicode: {0}")]
    NonUnicodePath(PathBuf),
    #[error("sherpa-onnx could not read WAV audio: {0}")]
    ReadWave(PathBuf),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_invalid_thread_counts_before_loading_a_model() {
        let root = tempfile::tempdir().unwrap();
        let manager = ModelManager::new(root.path().to_path_buf()).unwrap();
        assert!(matches!(
            SherpaAsrRecognizer::load(&manager, "qwen3-asr-0.6b-int8", 0),
            Err(AsrError::InvalidThreads(0))
        ));
    }
}
