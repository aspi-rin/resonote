use super::{
    ArtifactKind, ArtifactSpec, CATALOG_MODEL_ID, CATALOG_REVISION, ModelError, ModelPackageSpec,
};

pub fn model_catalog() -> Vec<ModelPackageSpec> {
    vec![qwen3_asr_spec()]
}

pub(super) fn model_spec(model_id: &str) -> Result<ModelPackageSpec, ModelError> {
    model_catalog()
        .into_iter()
        .find(|item| item.id == model_id)
        .ok_or_else(|| ModelError::UnknownModel(model_id.to_owned()))
}

fn qwen3_asr_spec() -> ModelPackageSpec {
    let release = "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models";
    ModelPackageSpec {
        artifacts: vec![
            ArtifactSpec {
                file_name: "sherpa-onnx-qwen3-asr-0.6B-int8-2026-03-25.tar.bz2".to_owned(),
                kind: ArtifactKind::ModelArchive,
                sha256: "393f8a14e2f5fb96746aaab342997a40641001fbd5bf9592a080a8329178ee96"
                    .to_owned(),
                size: 878_702_423,
                url: format!("{release}/sherpa-onnx-qwen3-asr-0.6B-int8-2026-03-25.tar.bz2"),
            },
            ArtifactSpec {
                file_name: "silero_vad.onnx".to_owned(),
                kind: ArtifactKind::VadModel,
                sha256: "9e2449e1087496d8d4caba907f23e0bd3f78d91fa552479bb9c23ac09cbb1fd6"
                    .to_owned(),
                size: 643_854,
                url: format!("{release}/silero_vad.onnx"),
            },
        ],
        display_name: "Qwen3-ASR 0.6B INT8 (sherpa-onnx)".to_owned(),
        id: CATALOG_MODEL_ID.to_owned(),
        revision: CATALOG_REVISION.to_owned(),
    }
}
