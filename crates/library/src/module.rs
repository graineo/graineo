//! Downloadable modules (`~/Library/Application Support/<APP>/modules/<id>/`): models for the
//! semantic search. Download from Hugging Face with progress and resume after abort
//! (`curl -C -`) from a **fixed revision** (commit hash) per model; every file has a SHA-256 and
//! a size stored in this source. A deviation discards the download with a clear message.
//! Modules installed earlier keep working as long as their files match the checksums.
//! For tests an existing Hugging Face cache can serve as the source (`GRAINEO_MODULE_SOURCE`):
//! then the files there are verified and linked instead of copied (read-only).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::{json, Value};

/// One file of a module with its expected size and SHA-256.
pub struct ModuleFile {
    pub path: &'static str,
    pub target: &'static str,
    pub size: u64,
    pub sha256: &'static str,
}

const fn f(path: &'static str, target: &'static str, size: u64, sha256: &'static str) -> ModuleFile {
    ModuleFile { path, target, size, sha256 }
}

pub struct Definition {
    pub id: &'static str,
    pub name: &'static str,
    pub repo: &'static str,
    pub revision: &'static str,
    /// path in the repo, target in the module folder, size, SHA-256
    pub files: &'static [ModuleFile],
    pub license: &'static str,
    pub purpose: &'static str,
    /// File produced locally after the download (e.g. a smaller F16 model); `None`: none.
    pub derived: Option<&'static Derived>,
    /// Facts for the model choice in the settings (`None`: not a search model).
    pub info: Option<&'static ModelInfo>,
    /// Licence class: open (may be offered like any module) or non-commercial only (see [`LicenseClass`]).
    pub class: LicenseClass,
}

/// Licence class of a module's files.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LicenseClass {
    /// Open licence (Apache-2.0, MIT, BSD …) without a restriction of the use.
    Open,
    /// Licence that allows non-commercial use only (e.g. CC BY-NC 4.0). Such a module is optional: graineo never
    /// hosts or ships its files, it is only downloaded from its original source at the user's explicit wish, after
    /// the user confirmed non-commercial use ([`confirm_non_commercial`]); every function that can use it also works
    /// with an open module.
    NonCommercial(&'static NonCommercial),
}

/// Terms of a non-commercial module, shown in the settings and in the confirmation (texts German, translated where
/// displayed).
#[derive(Debug, PartialEq, Eq)]
pub struct NonCommercial {
    /// licence name, e.g. "CC BY-NC 4.0"
    pub name: &'static str,
    /// link to the licence text
    pub url: &'static str,
    /// the core of the licence in one sentence (German)
    pub summary: &'static str,
    /// original source of the model (authors' repository or model card)
    pub source: &'static str,
    /// attribution as the licence asks for (authors, title, source)
    pub attribution: &'static str,
}

/// What a search model is and does, for a choice understandable without background knowledge.
/// Texts are German (translated where displayed, `crate::tr` in the app). Numbers are measured on this
/// Mac (M-series, Accelerate) with graineo's own implementation unless the source says otherwise.
pub struct ModelInfo {
    /// short display name
    pub title: &'static str,
    /// purpose in one sentence
    pub summary: &'static str,
    /// `"text"` (search words → vector) or `"image"` (photo → vector)
    pub kind: &'static str,
    pub languages: &'static str,
    /// manufacturer / authors
    pub maker: &'static str,
    /// licence as on the model card / project
    pub license: &'static str,
    /// bytes on disk once installed (= `Definition::installed_size`)
    pub disk_bytes: u64,
    /// working memory while loaded, MB (measured)
    pub memory_mb: u32,
    /// computing time per search query (text) or per photo (image), ms (measured)
    pub ms_per_item: u32,
    /// loading time, ms (measured, warm file cache)
    pub load_ms: u32,
    /// quality figure, e.g. "R@1 51,9 % (XTD10, Deutsch)"
    pub quality: &'static str,
    /// where the quality figure comes from
    pub quality_source: &'static str,
    /// choosing it means computing all photo vectors again
    pub needs_reindex: bool,
}

/// A file computed from a downloaded one at installation. The source file is removed afterwards and
/// is no longer needed for the status; the result has a pinned size and SHA-256 like a download.
pub struct Derived {
    /// target of the source file in the module folder (consumed)
    pub from: &'static str,
    pub target: &'static str,
    pub size: u64,
    pub sha256: &'static str,
    pub make: fn(&Path, &Path) -> Result<(), String>,
}

impl Definition {
    /// Files that make up the installed module: downloads (without a consumed source) and the derived file.
    fn kept(&self) -> impl Iterator<Item = (&'static str, u64, &'static str)> + '_ {
        let from = self.derived.map(|x| x.from);
        self.files.iter().filter(move |f| Some(f.target) != from).map(|f| (f.target, f.size, f.sha256)).chain(self.derived.map(|x| (x.target, x.size, x.sha256)))
    }

    /// Bytes on disk once installed.
    pub fn installed_size(&self) -> u64 {
        self.kept().map(|k| k.1).sum()
    }
}

pub const TEXT: Definition = Definition {
    id: "text-encoder",
    name: "Text-Encoder (semantische Suche)",
    repo: "sentence-transformers/clip-ViT-B-32-multilingual-v1",
    revision: "58edf8cada9e398793dca955574a48cbb7f18be2",
    files: &[
        f("model.safetensors", "model.safetensors", 538947416, "7b968c09938603158413ea2c91af82e82158ebb6db901a6cf97c28ce7d7ebc6d"),
        f("vocab.txt", "vocab.txt", 995526, "fe0fda7c425b48c516fc8f160d594c8022a0808447475c1a7c6d6479763f310c"),
        f("config.json", "config.json", 572, "edc12f518bfc445dedfe8a7c58374ea659525e0b6e67814686bd6cd57f90f8b3"),
        f("2_Dense/model.safetensors", "2_Dense/model.safetensors", 1572984, "d12568dc7300970a4d3dbb49068ad16cd89b99840b74b026f8e48071e9414f74"),
        f("2_Dense/config.json", "2_Dense/config.json", 115, "48f4dfaf605a6c2869cd200970e54911c6c15053781d2bdcf3e81e89e77662ed"),
        f("1_Pooling/config.json", "1_Pooling/config.json", 190, "a37f83ada23e7887be6b88f4998927dbeac0038af301553c7cd5461413bf1a56"),
        f("modules.json", "modules.json", 341, "f83ea5d68ac85ec15f650b350f0dc37b03d63abd60442f518e14c06f479beee6"),
        f("README.md", "README.md", 5647, "023953714bc8541390994257494d21484994d84e45e01cd96304f0b5f9e770a2"),
    ],
    license: "Apache-2.0 (model card sentence-transformers/clip-ViT-B-32-multilingual-v1)",
    purpose: "Suchtext → Vektor im CLIP-Raum, 50+ Sprachen",
    derived: None,
    info: Some(&ModelInfo {
        title: "Standard",
        summary: "Kleines, schnelles Text-Modell: macht aus Suchwörtern einen Vektor, der zu den Bildvektoren passt.",
        kind: "text",
        languages: "50+ Sprachen (u. a. Deutsch, Englisch, Französisch, Spanisch, Japanisch)",
        maker: "sentence-transformers (N. Reimers), destilliert aus OpenAI CLIP",
        license: "Apache-2.0",
        disk_bytes: 538_947_416 + 995_526 + 572 + 1_572_984 + 115 + 190 + 341 + 5647,
        memory_mb: STD_MEMORY_MB,
        ms_per_item: STD_MS,
        load_ms: STD_LOAD_MS,
        quality: STD_QUALITY,
        quality_source: "XTD10 (1000 COCO-Bilder, deutsche Beschriftungen), Text→Bild, eigene Messung mit graineos Nachbau",
        needs_reindex: false,
    }),
    class: LicenseClass::Open,
};

pub const IMAGE: Definition = Definition {
    id: "image-encoder",
    name: "Bild-Encoder (Vektoren für neue Fotos)",
    repo: "sentence-transformers/clip-ViT-B-32",
    revision: "327ab6726d33c0e22f920c83f2ff9e4bd38ca37f",
    files: &[
        f("0_CLIPModel/model.safetensors", "model.safetensors", 605157884, "99d28a652e6ec46629ab7047a0ac82c69b1fe11e0ce672c43af65d3a9a3fc05d"),
        f("0_CLIPModel/config.json", "config.json", 4025, "f77ab2f54a03090c5dcac397670c254c39cd7a311611cafb5a79de3b07cd13ff"),
        f("0_CLIPModel/preprocessor_config.json", "preprocessor_config.json", 316, "910e70b3956ac9879ebc90b22fb3bc8a75b6a0677814500101a4c072bd7857bd"),
        f("README.md", "README.md", 1911, "84dc32f9617815fef11181bd6dca41031b7832b7201706fcae6d1776e46a7756"),
    ],
    license: "MIT (OpenAI CLIP), model card sentence-transformers/clip-ViT-B-32",
    purpose: "Foto → Vektor; für Fotos, die noch nicht beschrieben sind",
    derived: None,
    info: None,
    class: LicenseClass::Open,
};

/// SigLIP 2 Base (stage "Ausgewogen" of the search, `search_model.rs`): image and text tower in one file.
pub const SIGLIP_BASE: Definition = Definition {
    id: "siglip2-base",
    name: "Suchmodell Ausgewogen (SigLIP 2 Base)",
    repo: "google/siglip2-base-patch16-256",
    revision: "3f9f96cb90da5dbc758b01813f2f6f1aee24c1ab",
    files: &[
        f("model.safetensors", "model.safetensors", 1500985224, "6125cacc01fa93bdc98a0c5101cefcd69b2ed1f8ab4f38d86f4ad5984f5dc863"),
        f("tokenizer.model", "tokenizer.model", 4241003, "61a7b147390c64585d6c3543dd6fc636906c9af3865a5548f27f31aee1d4c8e2"),
        f("config.json", "config.json", 276, "7b5aedcb8893e31376e129c1ffd7a5392f1a806dbc793ce53eda220c2ec59edf"),
        f("preprocessor_config.json", "preprocessor_config.json", 394, "d14ba2ee3fd816f3de8abaddc31953565128eaf37c73ad4bed32101a98465aff"),
        f("README.md", "README.md", 3375, "3f093559c3a18d94d9336249cf2e4e42ec8752413f07eb6634e2820e4ea120ad"),
    ],
    license: "Apache-2.0 (model card google/siglip2-base-patch16-256)",
    purpose: "Foto und Suchtext → Vektor, 109 Sprachen",
    derived: None,
    info: Some(&ModelInfo {
        title: "SigLIP 2 Base",
        summary: "Bild- und Text-Modell in einem: versteht Suchwörter in vielen Sprachen am besten; alle Fotos werden einmal neu eingelesen.",
        kind: "image+text",
        languages: "109 Sprachen im Training",
        maker: "Google",
        license: "Apache-2.0",
        disk_bytes: 1500985224 + 4241003 + 276 + 394 + 3375,
        memory_mb: crate::search_model::MEASURED_BALANCED.0,
        ms_per_item: crate::search_model::MEASURED_BALANCED.2,
        load_ms: 760,
        quality: "Top-1 62,1 %, Top-10 92,1 % (Deutsch)",
        quality_source: "XTD10 (1000 COCO-Bilder, deutsche Beschriftungen), Text→Bild, eigene Messung mit graineos Nachbau",
        needs_reindex: true,
    }),
    class: LicenseClass::Open,
};

/// SigLIP 2 Large (stage "Beste Treffer").
pub const SIGLIP_LARGE: Definition = Definition {
    id: "siglip2-large",
    name: "Suchmodell Beste Treffer (SigLIP 2 Large)",
    repo: "google/siglip2-large-patch16-256",
    revision: "787800c8990e6f058423089178e718139608408c",
    files: &[
        f("model.safetensors", "model.safetensors", 3526204360, "fa34f822f016dbb167d8d0e3a8af99b5e199aa28573360d4295252b9ec418e2a"),
        f("tokenizer.model", "tokenizer.model", 4241003, "61a7b147390c64585d6c3543dd6fc636906c9af3865a5548f27f31aee1d4c8e2"),
        f("config.json", "config.json", 537, "172e39dbf0143b8fe22d2f08921730eb8c397967e58cb37d133161e28aa34104"),
        f("preprocessor_config.json", "preprocessor_config.json", 394, "d14ba2ee3fd816f3de8abaddc31953565128eaf37c73ad4bed32101a98465aff"),
        f("README.md", "README.md", 3378, "005588b9d15ae2fcf79107a0d1a83b675541a855c2b4fef6f03ece89d485905e"),
    ],
    license: "Apache-2.0 (model card google/siglip2-large-patch16-256)",
    purpose: "Foto und Suchtext → Vektor, 109 Sprachen, genauer",
    derived: None,
    info: Some(&ModelInfo {
        title: "SigLIP 2 Large",
        summary: "Größere Fassung von SigLIP 2: genauer, aber dreimal so groß und deutlich langsamer beim Einlesen.",
        kind: "image+text",
        languages: "109 Sprachen im Training",
        maker: "Google",
        license: "Apache-2.0",
        disk_bytes: 3526204360 + 4241003 + 537 + 394 + 3378,
        memory_mb: crate::search_model::MEASURED_BEST.0,
        ms_per_item: crate::search_model::MEASURED_BEST.2,
        load_ms: 1600,
        quality: crate::search_model::LARGE_QUALITY,
        quality_source: "XTD10 (1000 COCO-Bilder, deutsche Beschriftungen), Text→Bild, eigene Messung mit graineos Nachbau",
        needs_reindex: true,
    }),
    class: LicenseClass::Open,
};

/// Larger multilingual text model (M-CLIP, XLM-RoBERTa large) for the same image vectors. The F32
/// weights (2.2 GB) are converted to F16 (1.1 GB) after the checked download.
pub const MCLIP: Definition = Definition {
    id: "text-encoder-mclip",
    name: "Text-Modell mehrsprachig groß (M-CLIP)",
    repo: "M-CLIP/XLM-Roberta-Large-Vit-B-32",
    // safetensors variant of the model (equivalent to pytorch_model.bin of the main branch)
    revision: "5a819f3d77e25bbbf9513a6d335117a6d64fdc16",
    files: &[
        f("model.safetensors", "model.safetensors", 2241715472, "549b7f6523a32e309a38b6cd2d9a60ffd9ea1a164237a5dc61e25b0f27bc5951"),
        f("tokenizer.json", "tokenizer.json", 9081351, "2a0d7366dd7780ea36cc42431dd74cd79289b783ab01acd33013fcc96865a8e9"),
        f("tokenizer_config.json", "tokenizer_config.json", 399, "dfbf28b57f58ceb96d5a2e384b00c8768a902742e5cacd90f769e350ada9afb1"),
        f("special_tokens_map.json", "special_tokens_map.json", 239, "378eb3bf733eb16e65792d7e3fda5b8a4631387ca04d2015199c4d4f22ae554d"),
        f("config.json", "config.json", 221, "a1f66c67922b5edd63db2ea021acef4121e54aafe38fc829438f0f21cd994921"),
        f("README.md", "README.md", 3813, "96bc6cb5b61855e15b7d34591245c10a03242554b497300b874ba94d86689aef"),
    ],
    license: "MIT (Multilingual-CLIP, F. Carlsson; XLM-RoBERTa MIT)",
    purpose: "Suchtext → Vektor im CLIP-Raum, viele Sprachen, deutlich genauer",
    derived: Some(&Derived {
        from: "model.safetensors",
        target: crate::xlmr::MODEL_F16,
        size: MCLIP_F16_SIZE,
        sha256: MCLIP_F16_SHA,
        make: crate::xlmr::convert_f16,
    }),
    info: Some(&ModelInfo {
        title: "Mehrsprachig groß (M-CLIP)",
        summary: "Großes mehrsprachiges Text-Modell: versteht Suchwörter in vielen Sprachen deutlich besser; die Bildvektoren bleiben.",
        kind: "text",
        languages: "48 Trainingssprachen und weitere über XLM-RoBERTa (100 Sprachen)",
        maker: "F. Carlsson et al. (RISE, Multilingual-CLIP) auf Basis von XLM-RoBERTa large (Facebook AI)",
        license: "MIT",
        disk_bytes: MCLIP_F16_SIZE + 9_081_351 + 399 + 239 + 221 + 3813,
        memory_mb: MCLIP_MEMORY_MB,
        ms_per_item: MCLIP_MS,
        load_ms: MCLIP_LOAD_MS,
        quality: MCLIP_QUALITY,
        quality_source: "XTD10 (1000 COCO-Bilder, deutsche Beschriftungen), Text→Bild, eigene Messung mit graineos Nachbau",
        needs_reindex: false,
    }),
    class: LicenseClass::Open,
};

// Measured 12.11.2026 on an M-series Mac with 16 GB (other builds running, so the times are rather
// on the high side; minimum of the query times: standard 14 ms, M-CLIP 77 ms). Quality: own Rust
// implementation on XTD10 (1000 COCO images), German captions, text→image.
pub const STD_MEMORY_MB: u32 = 550;
pub const STD_MS: u32 = 20;
pub const STD_LOAD_MS: u32 = 1700;
pub const STD_QUALITY: &str = "Top-1 39,2 %, Top-10 79,0 % (Deutsch)";
pub const MCLIP_MEMORY_MB: u32 = 640;
pub const MCLIP_MS: u32 = 130;
pub const MCLIP_LOAD_MS: u32 = 720;
pub const MCLIP_QUALITY: &str = "Top-1 51,9 %, Top-10 88,9 % (Deutsch)";

const MCLIP_F16_SIZE: u64 = 1_121_528_120;
const MCLIP_F16_SHA: &str = "c0056b3e571dd06ac232d884ea25b0eaa6692a98571f6012adb45b0e6417f925";

/// Click selection for masks (MobileSAM: TinyViT image encoder + SAM mask decoder), run with tract on every system.
pub const MOBILESAM: Definition = Definition {
    id: "mobilesam",
    name: "Klick-Auswahl (MobileSAM)",
    repo: "Acly/MobileSAM",
    revision: "0d3b403339b4674a82493d5e97964dd78089ddc8",
    files: &[
        f("mobile_sam_image_encoder.onnx", "mobile_sam_image_encoder.onnx", 28157093, "580f5fb648ea1062c0aabc26217aed56921985f03f0cbbd852bba81d760cc749"),
        f("sam_mask_decoder_multi.onnx", "sam_mask_decoder_multi.onnx", 16496559, "8976b90a87ba50a6a72217a5ff994f7d25ce16f2229fcc1ed259e1294c622ffe"),
    ],
    license: "Apache-2.0 (code, ChaoningZhang/MobileSAM), MIT (weights, model card Acly/MobileSAM)",
    purpose: "Masken: Objekt anklicken, Auswahl wird berechnet",
    derived: None,
    info: None,
    class: LicenseClass::Open,
};

/// Removing objects in the retouch (Big-LaMa inpainting, fixed 512 × 512 input), run with tract on every system.
pub const LAMA: Definition = Definition {
    id: "lama",
    name: "Objekte entfernen (LaMa)",
    repo: "Carve/LaMa-ONNX",
    revision: "c3c0c9e468934d62e79c329e35d82dd09ff8c444",
    files: &[f("lama_fp32.onnx", "lama_fp32.onnx", 208044816, "1faef5301d78db7dda502fe59966957ec4b79dd64e16f03ed96913c7a4eb68d6")],
    license: "Apache-2.0 (code advimman/lama, model card Carve/LaMa-ONNX); trained on Places2",
    purpose: "Retusche: markierte Bereiche mit erfundenem Inhalt füllen",
    derived: None,
    info: None,
    class: LicenseClass::Open,
};

/// Depth masks: relative depth with Depth Anything V2 Small (ViT-S, 24.8 M parameters), run with tract on every
/// system. Code and Small weights Apache-2.0 (authors' repository and model card); the ONNX export is the one of
/// onnx-community (model card: Apache-2.0, base model depth-anything/Depth-Anything-V2-Small).
pub const DEPTH: Definition = Definition {
    id: "depth",
    name: "Tiefe (Depth Anything V2 Small)",
    repo: "onnx-community/depth-anything-v2-small",
    revision: "4472b7362082ad9968fee890ca0f1e5aca36b93d",
    files: &[
        f("onnx/model.onnx", "model.onnx", 99060839, "afb6a5c28f3b6bf1618c6e43f02073ef9dfdc70e937502d51603e57b0a1df10c"),
        f("README.md", "README.md", 1578, "2fdd0db4eb5d7725fe0f1522e03a63f0b3c7dc93c805669f3322f665fb3b4c86"),
    ],
    license: "Apache-2.0 (code and Small weights: DepthAnything/Depth-Anything-V2, model cards depth-anything/Depth-Anything-V2-Small and onnx-community/depth-anything-v2-small)",
    purpose: "Masken: Entfernung vom Objektiv schätzen (nah ↔ fern)",
    derived: None,
    info: None,
    class: LicenseClass::Open,
};

/// Terms of the larger Depth Anything V2 models (Base, Large): CC BY-NC 4.0 on the authors' model cards and in their
/// repository ("Depth-Anything-V2-Base/Large/Giant models are under the CC-BY-NC-4.0 license").
pub const DEPTH_NC_TERMS: NonCommercial = NonCommercial {
    name: "CC BY-NC 4.0",
    url: "https://creativecommons.org/licenses/by-nc/4.0/legalcode",
    summary: "Das Modell darf mit Namensnennung genutzt und weitergegeben werden, aber nicht für kommerzielle Zwecke.",
    source: "https://github.com/DepthAnything/Depth-Anything-V2",
    attribution: "Depth Anything V2 (L. Yang, B. Kang, Z. Huang, Z. Zhao, X. Xu, J. Feng, H. Zhao), https://github.com/DepthAnything/Depth-Anything-V2, CC BY-NC 4.0; ONNX export onnx-community",
};

/// Optional, better depth (Depth Anything V2 Base, ViT-B, 97.5 M parameters): non-commercial use only.
pub const DEPTH_BASE: Definition = Definition {
    id: "depth-base",
    name: "Tiefe genauer (Depth Anything V2 Base)",
    repo: "onnx-community/depth-anything-v2-base",
    revision: "dd4557d492cd7b563738ac8d9ccff9094620983c",
    files: &[
        f("onnx/model.onnx", "model.onnx", 388917010, "dab40f67d5ead5e0ed52acfae41a717a708b02bc57f6cb761c5de828f26a7289"),
        f("README.md", "README.md", 1575, "526657685ec35278fd7dd4ece1008bbd7ae6a6869928648782032802fdcea8e0"),
    ],
    license: "CC BY-NC 4.0 (weights: model card depth-anything/Depth-Anything-V2-Base and DepthAnything/Depth-Anything-V2; ONNX export onnx-community/depth-anything-v2-base)",
    purpose: "Masken: Entfernung genauer schätzen (nur nicht-kommerziell)",
    derived: None,
    info: None,
    class: LicenseClass::NonCommercial(&DEPTH_NC_TERMS),
};

pub const ALL: [&Definition; 7] = [&TEXT, &MCLIP, &IMAGE, &MOBILESAM, &LAMA, &DEPTH, &DEPTH_BASE];

/// Modules not offered for download (Settings → Modules lists them only once installed, so they can be
/// removed; the tools that need them stay hidden without the files). Empty: LaMa was withheld for a short
/// time because its published weights carry no licence of their own (the export's model card says Apache-2.0,
/// trained on Places2); released again on the user's decision (17.11.2026), graineo does not ship or host the file.
pub const WITHHELD: [&str; 0] = [];

/// Is the module offered for download in Settings → Modules?
pub fn offered(d: &Definition) -> bool {
    !WITHHELD.contains(&d.id)
}

/// Search models (SigLIP 2): loaded and removed in Settings → Search, not on the modules page.
pub const SEARCH: [&Definition; 2] = [&SIGLIP_BASE, &SIGLIP_LARGE];

/// Where a module file is downloaded from. Today Hugging Face at the pinned revision; prepared for a
/// signed module list with own mirrors (FAHRPLAN 19): such a list would set a base URL here, the
/// pinned size and SHA-256 per file stay the check.
pub fn url_for(d: &Definition, path: &str) -> String {
    // graineo never hosts a non-commercial module: always its original source
    match mirror_base().filter(|_| non_commercial(d).is_none()) {
        Some(base) => format!("{}/{}/{}/{}", base.trim_end_matches('/'), d.id, d.revision, path),
        None => format!("https://huggingface.co/{}/resolve/{}/{}", d.repo, d.revision, path),
    }
}

/// Base URL of a mirror (`None`: Hugging Face). Not configurable yet: comes with the signed module list.
fn mirror_base() -> Option<String> {
    None
}

pub fn folder(d: &Definition) -> PathBuf {
    crate::semantic::modules_folder().join(d.id)
}

#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    Missing,
    Partial,
    Installed,
}

pub fn status(d: &Definition) -> (Status, u64) {
    let o = folder(d);
    let mut da = 0;
    let mut size = 0;
    let mut count = 0;
    for (target, expected, _) in d.kept() {
        count += 1;
        // a file of the wrong size counts as not present (later install repairs it)
        if let Ok(m) = std::fs::metadata(o.join(target)) {
            if m.len() == expected {
                da += 1;
                size += m.len();
            }
        }
    }
    let z = if da == count && o.join("module.json").exists() {
        Status::Installed
    } else if da == 0 {
        Status::Missing
    } else {
        Status::Partial
    };
    (z, size)
}

pub fn info(d: &Definition) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(folder(d).join("module.json")).ok()?).ok()
}

/// Full check of an installed module against the stored checksums (reads all files).
pub fn verify(d: &Definition) -> Result<(), String> {
    let o = folder(d);
    for (target, size, sha) in d.kept() {
        if !existing_checked(&o.join(target), size, Some(sha))? {
            return Err(format!("{target}: Datei fehlt oder weicht von der hinterlegten Prüfsumme ab"));
        }
    }
    Ok(())
}

/// Checks a finished download against size and SHA-256. On a deviation the file is
/// deleted and a clear message returned.
fn check_download(part: &Path, path: &str, size: u64, sha: &str) -> Result<(), String> {
    let n = std::fs::metadata(part).map_or(0, |m| m.len());
    if n != size {
        let _ = std::fs::remove_file(part);
        return Err(format!("{path}: Größe {n} statt {size} – Download verworfen"));
    }
    if crate::relocate::sha256(part)? != sha {
        let _ = std::fs::remove_file(part);
        return Err(format!("Prüfsumme von {path} weicht von der fest hinterlegten ab – Download verworfen, nichts installiert"));
    }
    Ok(())
}

/// Install. `progress(finished, total, file)`; `cancel` aborts (partial files stay
/// for resuming).
pub fn install(d: &Definition, progress: &dyn Fn(u64, u64, &str), cancel: &AtomicBool) -> Result<(), String> {
    // a non-commercial module only after the user's confirmation, and only from its original source (never a mirror)
    if non_commercial(d).is_some() && confirmation(d).is_none() {
        return Err("Nur nach Bestätigung der nicht-kommerziellen Nutzung".into());
    }
    let o = folder(d);
    std::fs::create_dir_all(&o).map_err(|e| e.to_string())?;
    let local = std::env::var_os("GRAINEO_MODULE_SOURCE").map(PathBuf::from);
    let total: u64 = d.files.iter().map(|x| x.size).sum();
    let mut finished = 0u64;
    // derived file already there and correct: its source need not be downloaded again
    let derived_ok = match d.derived {
        Some(x) => existing_checked(&o.join(x.target), x.size, Some(x.sha256))?,
        None => false,
    };
    for mf in d.files {
        let (path, target, size, sha) = (mf.path, mf.target, mf.size, mf.sha256);
        if cancel.load(Ordering::Relaxed) {
            return Err("abgebrochen".into());
        }
        if derived_ok && d.derived.is_some_and(|x| x.from == target) {
            finished += size;
            continue;
        }
        let z = o.join(target);
        if let Some(p) = z.parent() {
            std::fs::create_dir_all(p).map_err(|e| e.to_string())?;
        }
        if let Some(q) = &local {
            // Test: link an existing HF cache (read-only)
            let snap = std::fs::read_dir(q.join("hub").join(format!("models--{}", d.repo.replace('/', "--"))).join("snapshots"))
                .map_err(|e| e.to_string())?
                .flatten()
                .map(|e| e.path())
                .find(|p| p.join(path).exists())
                .ok_or(format!("{path} nicht im lokalen Cache"))?;
            let real = std::fs::canonicalize(snap.join(path)).map_err(|e| e.to_string())?;
            progress(finished, total.max(1), &format!("prüfe {path}"));
            if std::fs::metadata(&real).map_or(0, |m| m.len()) != size || crate::relocate::sha256(&real)? != sha {
                return Err(format!("Prüfsumme von {path} stimmt nicht"));
            }
            let _ = std::fs::remove_file(&z);
            crate::platform::pointer(&real, &z).map_err(|e| e.to_string())?;
            finished += std::fs::metadata(&real).map_or(0, |m| m.len());
            continue;
        }
        if existing_checked(&z, size, Some(sha))? {
            finished += size;
            continue;
        }
        let part = z.with_extension("part");
        let url = url_for(d, path);
        // https only, limited redirects, connection setup at most 20 s; curl aborts below
        // 1 KiB/s over 60 s. In addition the loop below ends a download whose partial file has not grown
        // for two minutes. Resuming (`-C -`) makes every abort harmless.
        let mut kind = std::process::Command::new("curl")
            .args(["-sSfL", "--proto", "=https", "--proto-redir", "=https", "--max-redirs", "5", "--connect-timeout", "20", "--speed-limit", "1024", "--speed-time", "60", "-C", "-", "-o"])
            .arg(&part)
            .arg(&url)
            .spawn()
            .map_err(|e| format!("curl: {e}"))?;
        let mut last_size = std::fs::metadata(&part).map_or(0, |m| m.len());
        let mut last_progress = std::time::Instant::now();
        loop {
            if let Some(st) = kind.try_wait().map_err(|e| e.to_string())? {
                if !st.success() {
                    return Err(format!("Download von {path} fehlgeschlagen (wird beim nächsten Mal fortgesetzt)"));
                }
                break;
            }
            if cancel.load(Ordering::Relaxed) {
                let _ = kind.kill();
                let _ = kind.wait();
                return Err("abgebrochen (Teildatei bleibt für die Fortsetzung)".into());
            }
            let now = std::fs::metadata(&part).map_or(0, |m| m.len());
            if now != last_size {
                last_size = now;
                last_progress = std::time::Instant::now();
            } else if last_progress.elapsed() > std::time::Duration::from_secs(120) {
                let _ = kind.kill();
                let _ = kind.wait();
                return Err(format!("Download von {path} steht still (wird beim nächsten Mal fortgesetzt)"));
            }
            progress(finished + now, total, path);
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
        let n = std::fs::metadata(&part).map_or(0, |m| m.len());
        progress(finished + n, total, &format!("prüfe {path}"));
        check_download(&part, path, size, sha)?;
        std::fs::rename(&part, &z).map_err(|e| e.to_string())?;
        finished += n;
    }
    if let (Some(x), false) = (d.derived, derived_ok) {
        let (src, out) = (o.join(x.from), o.join(x.target));
        let part = out.with_extension("part");
        progress(total, total, &format!("wandle {} um", x.from));
        let _ = std::fs::remove_file(&part);
        (x.make)(&src, &part).inspect_err(|_| {
            let _ = std::fs::remove_file(&part);
        })?;
        check_download(&part, x.target, x.size, x.sha256)?;
        std::fs::rename(&part, &out).map_err(|e| e.to_string())?;
        // the source (or, in test mode, the link to it) is no longer needed
        std::fs::remove_file(&src).map_err(|e| e.to_string())?;
    }
    let meta = json!({"id": d.id, "repo": d.repo, "revision": d.revision, "license": d.license, "installed": crate::now(), "source": if local.is_some() { "local-cache (test)" } else { "huggingface" }, "verified": "sha256 fixed in app"});
    crate::files::write_atomic(&o.join("module.json"), serde_json::to_string_pretty(&meta).unwrap_or_default().as_bytes()).map_err(|e| e.to_string())?;
    progress(total, total, "fertig");
    Ok(())
}

pub fn remove(d: &Definition) -> Result<(), String> {
    let o = folder(d);
    if o.exists() {
        std::fs::remove_dir_all(&o).map_err(|e| e.to_string())?;
    }
    // a confirmation of non-commercial use belongs to the installed files
    forget_non_commercial(d);
    Ok(())
}

// ───────────────────────────────────────────── Non-commercial modules

/// Key in `settings.json`: confirmations of non-commercial use, per module id `{date, revision, app}`.
const CONSENTS: &str = "nonCommercialModules";

/// Is the module offered only for non-commercial use?
pub fn non_commercial(d: &Definition) -> Option<&'static NonCommercial> {
    match d.class {
        LicenseClass::NonCommercial(t) => Some(t),
        LicenseClass::Open => None,
    }
}

/// The user's confirmation of non-commercial use for this module at its pinned revision (`None`: not confirmed, or
/// for another revision).
pub fn confirmation(d: &Definition) -> Option<Value> {
    let v = crate::settings::value(CONSENTS).get(d.id).cloned()?;
    (v.get("revision").and_then(Value::as_str) == Some(d.revision)).then_some(v)
}

/// Record that the user confirmed non-commercial use of `d` (date, model id, revision, graineo version `app`).
pub fn confirm_non_commercial(d: &Definition, app: &str) -> Result<(), String> {
    if non_commercial(d).is_none() {
        return Ok(());
    }
    let mut all = crate::settings::value(CONSENTS);
    if !all.is_object() {
        all = json!({});
    }
    all[d.id] = json!({"date": crate::now(), "model": d.id, "repo": d.repo, "revision": d.revision, "app": app});
    crate::settings::set_checked(CONSENTS, all)
}

/// Remove the confirmation of `d` (the module was removed).
pub fn forget_non_commercial(d: &Definition) {
    let mut all = crate::settings::value(CONSENTS);
    if let Some(o) = all.as_object_mut() {
        if o.remove(d.id).is_some() {
            crate::settings::set(CONSENTS, all);
        }
    }
}

/// Can the module be used: installed and, for a non-commercial one, confirmed.
pub fn usable(d: &Definition) -> bool {
    status(d).0 == Status::Installed && (non_commercial(d).is_none() || confirmation(d).is_some())
}

/// Even when resuming, equal size is not enough for LFS files.
fn existing_checked(p: &Path, size: u64, sha: Option<&str>) -> Result<bool, String> {
    let m = match std::fs::metadata(p) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(format!("Moduldatei nicht prüfbar: {e}")),
    };
    if !m.is_file() || m.len() != size { return Ok(false); }
    match sha {
        Some(expected) => Ok(crate::relocate::sha256(p)? == expected),
        None => Ok(true), // For non-LFS files the repository only provides the size so far.
    }
}

#[cfg(test)]
mod integrity_tests {
    use super::*;
    #[test]
    fn same_size_damaged_modules_are_not_skipped() {
        let dir = std::env::temp_dir().join(format!("graineo-module-audit-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir(&dir).unwrap(); let p = dir.join("model");
        assert!(!existing_checked(&p, 4, None).unwrap());
        std::fs::write(&p, b"good").unwrap();
        let sha = crate::relocate::sha256(&p).unwrap();
        assert!(existing_checked(&p, 4, Some(&sha)).unwrap());
        std::fs::write(&p, b"evil").unwrap();
        assert!(!existing_checked(&p, 4, Some(&sha)).unwrap());
        assert!(!existing_checked(&p, 5, None).unwrap());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn download_with_wrong_checksum_is_discarded() {
        let dir = std::env::temp_dir().join(format!("graineo-module-hash-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir(&dir).unwrap();
        let p = dir.join("model.part");
        std::fs::write(&p, b"good").unwrap();
        let sha = crate::relocate::sha256(&p).unwrap();
        assert!(check_download(&p, "model", 4, &sha).is_ok());
        // same size, different content → checksum message, file removed
        std::fs::write(&p, b"evil").unwrap();
        let e = check_download(&p, "model", 4, &sha).unwrap_err();
        assert!(e.contains("Prüfsumme") && e.contains("verworfen"), "{e}");
        assert!(!p.exists());
        // wrong size → also discarded
        std::fs::write(&p, b"toolong").unwrap();
        assert!(check_download(&p, "model", 4, &sha).unwrap_err().contains("verworfen"));
        assert!(!p.exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn non_commercial_modules_need_a_confirmation() {
        let d = &DEPTH_BASE;
        assert!(non_commercial(d).is_some() && non_commercial(&DEPTH).is_none());
        assert!(confirmation(d).is_none());
        // no download without confirmation
        let e = install(d, &|_, _, _| {}, &AtomicBool::new(true)).unwrap_err();
        assert!(e.contains("Bestätigung"), "{e}");
        confirm_non_commercial(d, "9.9.9").unwrap();
        let c = confirmation(d).unwrap();
        assert_eq!(c["revision"], d.revision);
        assert_eq!(c["app"], "9.9.9");
        assert_eq!(c["model"], d.id);
        assert!(c["date"].as_i64().unwrap() > 0);
        // never from a mirror: the original source
        assert!(url_for(d, "onnx/model.onnx").starts_with("https://huggingface.co/onnx-community/depth-anything-v2-base/resolve/"));
        // removing the module forgets the confirmation
        remove(d).unwrap();
        assert!(confirmation(d).is_none());
        // open modules need none
        confirm_non_commercial(&DEPTH, "9.9.9").unwrap();
        assert!(crate::settings::value(CONSENTS).get(DEPTH.id).is_none());
    }

    #[test]
    fn definitions_are_pinned() {
        for d in ALL.iter().chain(SEARCH.iter()) {
            assert_eq!(d.revision.len(), 40, "{} not a commit hash", d.id);
            for mf in d.files {
                assert_eq!(mf.sha256.len(), 64);
                assert!(mf.size > 0);
            }
            if let Some(i) = d.info {
                assert_eq!(i.disk_bytes, d.installed_size(), "{}", d.id);
                assert!(i.ms_per_item > 0 && i.memory_mb > 0 && !i.quality.is_empty(), "{}", d.id);
            }
            if let LicenseClass::NonCommercial(t) = d.class {
                assert!(t.url.starts_with("https://") && t.source.starts_with("https://") && !t.summary.is_empty() && !t.attribution.is_empty(), "{}", d.id);
            }
            if let Some(x) = d.derived {
                assert_eq!(x.sha256.len(), 64, "{}", d.id);
                assert!(x.size > 0 && d.files.iter().any(|f| f.target == x.from));
            }
        }
    }
}

/// Adobe lens profiles (LCP) installed?
pub fn adobe_lcp() -> Option<PathBuf> {
    let p = sysdirs::system_data_dir()?.join("Adobe/CameraRaw/LensProfiles/1.0");
    p.is_dir().then_some(p)
}
