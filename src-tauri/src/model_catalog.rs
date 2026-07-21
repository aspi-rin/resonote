use super::{ArtifactKind, ArtifactSpec, ModelCatalogEntry, ModelError, ModelPackageSpec};

const SHERPA_RELEASE: &str = "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models";
const QWEN_ONNX_REVISION: &str = "cb045ad80b8970c9d411d463e5b78991a566596c";
const QWEN_ONNX_RELEASE: &str = "https://modelscope.cn/models/zengshuishui/Qwen3-ASR-onnx/resolve";

pub fn model_catalog() -> Vec<ModelCatalogEntry> {
    package_catalog()
        .into_iter()
        .map(|spec| ModelCatalogEntry {
            display_name: spec.display_name,
            id: spec.id,
            total_bytes: spec.artifacts.iter().map(|item| item.size).sum(),
        })
        .collect()
}

pub(super) fn model_spec(model_id: &str) -> Result<ModelPackageSpec, ModelError> {
    package_catalog()
        .into_iter()
        .find(|item| item.id == model_id)
        .ok_or_else(|| ModelError::UnknownModel(model_id.to_owned()))
}

fn package_catalog() -> Vec<ModelPackageSpec> {
    vec![qwen3_asr_0_6b_spec(), qwen3_asr_1_7b_spec()]
}

fn qwen3_asr_0_6b_spec() -> ModelPackageSpec {
    ModelPackageSpec {
        artifacts: vec![
            ArtifactSpec {
                file_name: "sherpa-onnx-qwen3-asr-0.6B-int8-2026-03-25.tar.bz2".to_owned(),
                kind: ArtifactKind::ModelArchive,
                sha256: "393f8a14e2f5fb96746aaab342997a40641001fbd5bf9592a080a8329178ee96"
                    .to_owned(),
                size: 878_702_423,
                url: format!("{SHERPA_RELEASE}/sherpa-onnx-qwen3-asr-0.6B-int8-2026-03-25.tar.bz2"),
            },
            vad_artifact(),
        ],
        display_name: "Qwen3-ASR 0.6B INT8 · Multilingual".to_owned(),
        id: "qwen3-asr-0.6b-int8".to_owned(),
        model_directory: "qwen3-asr",
        required_files: qwen_required_files(),
        revision: "sherpa-onnx-1.13.4-qwen3-2026-03-25".to_owned(),
    }
}

fn qwen3_asr_1_7b_spec() -> ModelPackageSpec {
    let model_base = format!("{QWEN_ONNX_RELEASE}/{QWEN_ONNX_REVISION}/model_1.7B");
    let tokenizer_base = format!("{QWEN_ONNX_RELEASE}/{QWEN_ONNX_REVISION}/tokenizer");
    ModelPackageSpec {
        artifacts: vec![
            model_file(
                "qwen3-asr/conv_frontend.onnx",
                48_080_441,
                "fa894a4ba53da6a4238f2a6ca0b09362e505d39cecbd646051b033e2e8d7e2fb",
                format!("{model_base}/conv_frontend.onnx"),
            ),
            model_file(
                "qwen3-asr/encoder.int8.onnx",
                314_222_162,
                "436fbd910a0c8914851e5ac1354e807be9f283d08a5da728adaa609731c41469",
                format!("{model_base}/encoder.int8.onnx"),
            ),
            model_file(
                "qwen3-asr/decoder.int8.onnx",
                2_037_458_645,
                "c43c853fa6e97d08365cb8a5502b360b595cd43c00dc60e4d8ca7cc18cad460b",
                format!("{model_base}/decoder.int8.onnx"),
            ),
            model_file(
                "qwen3-asr/tokenizer/vocab.json",
                2_776_833,
                "ca10d7e9fb3ed18575dd1e277a2579c16d108e32f27439684afa0e10b1440910",
                format!("{tokenizer_base}/vocab.json"),
            ),
            model_file(
                "qwen3-asr/tokenizer/merges.txt",
                1_671_853,
                "8831e4f1a044471340f7c0a83d7bd71306a5b867e95fd870f74d0c5308a904d5",
                format!("{tokenizer_base}/merges.txt"),
            ),
            model_file(
                "qwen3-asr/tokenizer/tokenizer_config.json",
                12_487,
                "4942d005604266809309cabc9f4e9cb89ce855d59b14681fdc0e1cc62ea26c4c",
                format!("{tokenizer_base}/tokenizer_config.json"),
            ),
            vad_artifact(),
        ],
        display_name: "Qwen3-ASR 1.7B INT8 · High accuracy".to_owned(),
        id: "qwen3-asr-1.7b-int8".to_owned(),
        model_directory: "qwen3-asr",
        required_files: qwen_required_files(),
        revision: format!("sherpa-onnx-1.13.4-qwen3-{QWEN_ONNX_REVISION}"),
    }
}

fn model_file(file_name: &str, size: u64, sha256: &str, url: String) -> ArtifactSpec {
    ArtifactSpec {
        file_name: file_name.to_owned(),
        kind: ArtifactKind::ModelFile,
        sha256: sha256.to_owned(),
        size,
        url,
    }
}

fn vad_artifact() -> ArtifactSpec {
    ArtifactSpec {
        file_name: "silero_vad.onnx".to_owned(),
        kind: ArtifactKind::VadModel,
        sha256: "9e2449e1087496d8d4caba907f23e0bd3f78d91fa552479bb9c23ac09cbb1fd6".to_owned(),
        size: 643_854,
        url: format!("{SHERPA_RELEASE}/silero_vad.onnx"),
    }
}

fn qwen_required_files() -> Vec<&'static str> {
    vec![
        "conv_frontend.onnx",
        "encoder.int8.onnx",
        "decoder.int8.onnx",
        "tokenizer/vocab.json",
        "tokenizer/merges.txt",
        "tokenizer/tokenizer_config.json",
    ]
}
