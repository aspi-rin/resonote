use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sherpa_onnx::{
    OfflineFunASRNanoModelConfig, OfflineQwen3ASRModelConfig, OfflineRecognizer,
    OfflineRecognizerConfig, OfflineWhisperModelConfig, Wave,
};
use thiserror::Error;

use crate::models::{InstalledModel, ModelError, ModelFamily, ModelManager};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AsrTranscription {
    pub language: String,
    pub text: String,
}

pub struct SherpaAsrRecognizer {
    language: String,
    recognizer: OfflineRecognizer,
    stream_language: Option<String>,
}

impl SherpaAsrRecognizer {
    pub fn load(
        manager: &ModelManager,
        model_id: &str,
        threads: u16,
        forced_language: Option<&str>,
    ) -> Result<Self, AsrError> {
        if !(1..=16).contains(&threads) {
            return Err(AsrError::InvalidThreads(threads));
        }
        let model = manager.installed_model(model_id)?;
        let mut config = OfflineRecognizerConfig::default();
        config.model_config.num_threads = i32::from(threads);
        config.model_config.provider = Some("cpu".to_owned());
        let stream_language = configure_model(&mut config, &model, forced_language)?;
        let recognizer = OfflineRecognizer::create(&config).ok_or(AsrError::CreateRecognizer)?;
        Ok(Self {
            language: forced_language.unwrap_or_default().to_owned(),
            recognizer,
            stream_language,
        })
    }

    pub fn transcribe_wav(&self, wav_path: &Path) -> Result<AsrTranscription, AsrError> {
        let path = path_string(wav_path)?;
        let wave = Wave::read(&path).ok_or_else(|| AsrError::ReadWave(wav_path.to_path_buf()))?;
        let stream = self.recognizer.create_stream();
        if let Some(language) = &self.stream_language {
            stream.set_option("language", language);
        }
        stream.accept_waveform(wave.sample_rate(), wave.samples());
        self.recognizer.decode(&stream);
        let result = stream.get_result().ok_or(AsrError::MissingResult)?;
        Ok(AsrTranscription {
            language: self.language.clone(),
            text: result.text.trim().to_owned(),
        })
    }
}

fn configure_model(
    config: &mut OfflineRecognizerConfig,
    model: &InstalledModel,
    forced_language: Option<&str>,
) -> Result<Option<String>, AsrError> {
    match model.family {
        ModelFamily::Qwen3Asr => {
            config.feat_config.feature_dim = 128;
            config.model_config.qwen3_asr = OfflineQwen3ASRModelConfig {
                conv_frontend: Some(model_path(model, "conv_frontend.onnx")?),
                encoder: Some(model_path(model, "encoder.int8.onnx")?),
                decoder: Some(model_path(model, "decoder.int8.onnx")?),
                tokenizer: Some(model_path(model, "tokenizer")?),
                max_total_len: 512,
                max_new_tokens: 512,
                temperature: 1e-6,
                top_p: 0.8,
                seed: 42,
                hotwords: None,
            };
            Ok(forced_language.map(str::to_owned))
        }
        ModelFamily::FunAsrNano => {
            config.feat_config.feature_dim = 80;
            config.model_config.funasr_nano = OfflineFunASRNanoModelConfig {
                encoder_adaptor: Some(model_path(model, "encoder_adaptor.int8.onnx")?),
                llm: Some(model_path(model, "llm.int8.onnx")?),
                embedding: Some(model_path(model, "embedding.int8.onnx")?),
                tokenizer: Some(model_path(model, "Qwen3-0.6B")?),
                system_prompt: Some("You are a helpful assistant.".to_owned()),
                user_prompt: Some("语音转写：".to_owned()),
                max_new_tokens: 512,
                temperature: 1e-6,
                top_p: 0.8,
                seed: 42,
                language: forced_language.map(funasr_language).map(str::to_owned),
                itn: 1,
                hotwords: None,
            };
            Ok(None)
        }
        ModelFamily::Whisper => {
            config.feat_config.feature_dim = 80;
            config.model_config.tokens = Some(model_path(model, "large-v3-tokens.txt")?);
            config.model_config.whisper = OfflineWhisperModelConfig {
                encoder: Some(model_path(model, "large-v3-encoder.int8.onnx")?),
                decoder: Some(model_path(model, "large-v3-decoder.int8.onnx")?),
                language: forced_language.map(whisper_language).map(str::to_owned),
                task: Some("transcribe".to_owned()),
                tail_paddings: -1,
                enable_token_timestamps: false,
                enable_segment_timestamps: false,
            };
            Ok(None)
        }
    }
}

fn model_path(model: &InstalledModel, relative: &str) -> Result<String, AsrError> {
    path_string(&model.directory.join(relative))
}

fn funasr_language(language: &str) -> &str {
    match language {
        "Chinese" => "中文",
        "English" => "英文",
        "Japanese" => "日文",
        "Cantonese" => "粤语",
        other => other,
    }
}

fn whisper_language(language: &str) -> &str {
    match language {
        "Chinese" => "zh",
        "English" => "en",
        "Japanese" => "ja",
        "Cantonese" => "yue",
        "Korean" => "ko",
        other => other,
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
            SherpaAsrRecognizer::load(&manager, "qwen3-asr-0.6b-int8", 0, None),
            Err(AsrError::InvalidThreads(0))
        ));
    }

    #[test]
    fn configures_fun_asr_nano_int8_with_itn_and_language_prompt() {
        let model = installed_model(ModelFamily::FunAsrNano);
        let mut config = OfflineRecognizerConfig::default();
        let stream_language = configure_model(&mut config, &model, Some("Chinese")).unwrap();

        assert_eq!(config.feat_config.feature_dim, 80);
        assert_eq!(config.model_config.funasr_nano.itn, 1);
        assert_eq!(
            config.model_config.funasr_nano.language.as_deref(),
            Some("中文")
        );
        assert!(
            config
                .model_config
                .funasr_nano
                .encoder_adaptor
                .as_deref()
                .unwrap()
                .ends_with("encoder_adaptor.int8.onnx")
        );
        assert_eq!(stream_language, None);
    }

    #[test]
    fn configures_whisper_large_v3_int8_with_whisper_language_code() {
        let model = installed_model(ModelFamily::Whisper);
        let mut config = OfflineRecognizerConfig::default();
        let stream_language = configure_model(&mut config, &model, Some("Japanese")).unwrap();

        assert_eq!(config.feat_config.feature_dim, 80);
        assert_eq!(config.model_config.whisper.language.as_deref(), Some("ja"));
        assert_eq!(config.model_config.whisper.tail_paddings, -1);
        assert!(
            config
                .model_config
                .tokens
                .as_deref()
                .unwrap()
                .ends_with("large-v3-tokens.txt")
        );
        assert_eq!(stream_language, None);
    }

    fn installed_model(family: ModelFamily) -> InstalledModel {
        InstalledModel {
            directory: PathBuf::from("/models/test"),
            family,
            model_id: "test".to_owned(),
            revision: "test".to_owned(),
            vad_model: PathBuf::from("/models/silero_vad.onnx"),
        }
    }
}
